// allow: outbound-network
//! Slice 6 — real-world fixture e2e driver.
//!
//! This test walks the master `tests/fixtures/real_world/index.toml` and
//! for each enabled `[[entry]]` row stands up a `wiremock::MockServer`,
//! mounts the frozen response body from disk (Crossref `/works/<doi>`,
//! Unpaywall `/v2/<doi>`, or arXiv `/api/query`), points the orchestrator
//! at the mock origin via the `DOIGET_CROSSREF_BASE` /
//! `DOIGET_UNPAYWALL_BASE` / `DOIGET_ARXIV_BASE` env vars, drives
//! [`doiget_core::orchestrator::metadata_only`], and asserts the outcome
//! against the per-entry `expected.toml`.
//!
//! ## Network purity
//!
//! Per the workspace network-purity guard, this file imports `wiremock`
//! to mount fake origins; the `// allow: outbound-network` first-line
//! escape hatch above covers that. All HTTP traffic terminates at
//! `127.0.0.1:N` mock servers — no live API is touched.
//!
//! ## Why metadata_only only
//!
//! The fixture set covers metadata response shapes. The `fetch_paper`
//! PDF leg is exercised with synthetic `%PDF-fake-bytes` payloads in
//! `crates/doiget-cli/tests/fetch_doi_oa_pdf_e2e.rs` and
//! `crates/doiget-mcp/tests/fetch_paper_e2e.rs`; the publisher PDF leg
//! is out of scope here (and per `tests/fixtures/real_world/README.md`
//! we deliberately don't redistribute PDFs).

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::print_stdout
)]

use std::sync::Arc;

use camino::Utf8PathBuf;
use doiget_core::http::HttpClient;
use doiget_core::orchestrator::metadata_only;
use doiget_core::provenance::ProvenanceLog;
use doiget_core::rate_limiter::RateLimiter;
use doiget_core::source::FetchContext;
use doiget_core::{CapabilityProfile, RateLimits, Ref};
use serde::Deserialize;
use tempfile::TempDir;
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Workspace-relative path to the fixture root, resolved at compile time
/// against `CARGO_MANIFEST_DIR` so the test is portable across
/// `cargo test` invocations from any cwd.
const FIXTURES_ROOT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/real_world"
);

const TEST_SESSION_ID: &str = "01J0000000000000000000TEST";

#[derive(Debug, Deserialize)]
struct IndexFile {
    #[serde(default)]
    schema_version: Option<String>,
    #[serde(rename = "entry", default)]
    entries: Vec<Entry>,
}

#[derive(Debug, Deserialize)]
struct Entry {
    id: String,
    kind: String,
    #[serde(rename = "ref")]
    ref_str: String,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    provenance: Option<String>,
    #[serde(default)]
    last_refreshed_iso: Option<String>,
    #[serde(default)]
    crossref_response: Option<String>,
    #[serde(default)]
    unpaywall_response: Option<String>,
    #[serde(default)]
    atom_response: Option<String>,
    expected: String,
    #[serde(default)]
    disabled: bool,
}

#[derive(Debug, Deserialize)]
struct Expected {
    safekey: String,
    source: String,
    title: String,
    #[serde(default)]
    oa_url: Option<String>,
    #[serde(default)]
    license: Option<String>,
    #[serde(default)]
    expected_error_code: Option<String>,
}

/// RAII guard that snapshots and restores the env vars the orchestrator
/// dispatch reads. Mirrors the EnvGuard pattern in
/// `crates/doiget-mcp/tests/fetch_paper_e2e.rs::EnvGuard` (see Slice 5
/// refactor A5 in CHANGELOG).
struct EnvGuard {
    snapshot: Vec<(&'static str, Option<String>)>,
}

const ENV_KEYS: &[&str] = &[
    "DOIGET_CROSSREF_BASE",
    "DOIGET_UNPAYWALL_BASE",
    "DOIGET_ARXIV_BASE",
    "DOIGET_CONTACT_EMAIL",
    "DOIGET_UNPAYWALL_EMAIL",
];

impl EnvGuard {
    fn snapshot() -> Self {
        let snapshot = ENV_KEYS
            .iter()
            .map(|k| (*k, std::env::var(k).ok()))
            .collect();
        for k in ENV_KEYS {
            std::env::remove_var(k);
        }
        Self { snapshot }
    }
    fn set(&self, key: &str, val: &str) {
        std::env::set_var(key, val);
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (k, v) in &self.snapshot {
            match v {
                Some(val) => std::env::set_var(k, val),
                None => std::env::remove_var(k),
            }
        }
    }
}

fn load_index() -> IndexFile {
    let path = format!("{FIXTURES_ROOT}/index.toml");
    let s = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    toml::from_str(&s).unwrap_or_else(|e| panic!("parse {path}: {e}"))
}

fn load_expected(rel: &str) -> Expected {
    let path = format!("{FIXTURES_ROOT}/{rel}");
    let s = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    toml::from_str(&s).unwrap_or_else(|e| panic!("parse {path}: {e}"))
}

fn load_fixture_body(rel: &str) -> String {
    let path = format!("{FIXTURES_ROOT}/{rel}");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

fn host_of(uri: &str) -> String {
    uri.parse::<Url>()
        .expect("uri parses")
        .host_str()
        .expect("uri has host")
        .to_string()
}

/// Build a `FetchContext` whose HTTP client allowlists the given mock
/// `(source, host)` pairs. The `source` strings MUST match the
/// `Source::name()` values dispatched inside the orchestrator
/// (`"crossref"`, `"unpaywall"`, `"arxiv"`).
fn build_ctx(allow: &[(&str, &str)]) -> (TempDir, FetchContext) {
    let td = TempDir::new().expect("tempdir");
    let log_dir =
        Utf8PathBuf::try_from(td.path().to_path_buf()).expect("temp dir path must be UTF-8");
    let log_path = log_dir.join("real-world-e2e.jsonl");
    let http = Arc::new(HttpClient::new_for_tests_allow_http_multi(allow));
    let rate_limiter = Arc::new(RateLimiter::new(RateLimits::HARD_CODED));
    let session_id = TEST_SESSION_ID.to_string();
    let log =
        Arc::new(ProvenanceLog::open(log_path, session_id.clone()).expect("provenance log opens"));
    (
        td,
        FetchContext {
            http,
            rate_limiter,
            log,
            session_id,
            cache_root: None,
        },
    )
}

/// Drive a single DOI entry through `metadata_only` against a wiremock
/// origin and assert the outcome equals `expected`.
async fn run_doi_entry(entry: &Entry, expected: &Expected) {
    // Both Crossref and Unpaywall share one mock server; the orchestrator
    // dispatches them under distinct source keys so the multi-source HTTP
    // allowlist is what keeps them separate at the redirect-closure layer.
    let server = MockServer::start().await;
    let host = host_of(&server.uri());

    // Crossref's CrossrefSource::request_url uses `Url::join` on
    // `/works/<doi>` which leaves `/` in the DOI unencoded. Wiremock
    // sees the literal path with the slash. Unpaywall's
    // `path_segments_mut().push(<doi>)` URL-encodes the `/` to `%2F`,
    // so the wiremock matcher must mirror that encoding.
    let crossref_path = format!("/works/{}", entry.ref_str);
    let unpaywall_path = format!("/v2/{}", entry.ref_str.replace('/', "%2F"));

    // Crossref mount. Kind "doi-crossref-fail-unpaywall" mounts a 404
    // with the fixture body so the orchestrator falls back to Unpaywall.
    if let Some(rel) = entry.crossref_response.as_deref() {
        let body = load_fixture_body(rel);
        let status_code: u16 = if entry.kind == "doi-crossref-fail-unpaywall" {
            404
        } else {
            200
        };
        Mock::given(method("GET"))
            .and(path(crossref_path))
            .respond_with(ResponseTemplate::new(status_code).set_body_string(body))
            .mount(&server)
            .await;
    }

    // Unpaywall mount when an unpaywall_response fixture exists.
    if let Some(rel) = entry.unpaywall_response.as_deref() {
        let body = load_fixture_body(rel);
        Mock::given(method("GET"))
            .and(path(unpaywall_path))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;
    }

    let env = EnvGuard::snapshot();
    env.set("DOIGET_CROSSREF_BASE", &server.uri());
    // Unpaywall base must include the `/v2` segment so the source's
    // single-push DOI lands at `/v2/<doi>`. See
    // `crates/doiget-core/src/sources/unpaywall.rs::request_url`.
    let unpaywall_base = format!("{}/v2", server.uri());
    env.set("DOIGET_UNPAYWALL_BASE", &unpaywall_base);
    env.set("DOIGET_CONTACT_EMAIL", "doiget-fixture-test@example.org");
    env.set("DOIGET_UNPAYWALL_EMAIL", "doiget-fixture-test@example.org");

    let (_td, ctx) = build_ctx(&[("crossref", &host), ("unpaywall", &host)]);
    let profile = CapabilityProfile::from_env().expect("clean env");

    let r = Ref::parse(&entry.ref_str)
        .unwrap_or_else(|e| panic!("ref parse failed for entry {}: {e}", entry.id));

    // Safekey is a property of the Ref itself; assert it before any
    // network dispatch so a regression in `Ref::safekey()` shows up
    // independently of the orchestrator outcome.
    assert_eq!(
        r.safekey().as_str(),
        expected.safekey,
        "entry {} safekey mismatch",
        entry.id
    );

    let outcome = metadata_only(&r, &profile, &ctx).await;

    match (expected.expected_error_code.as_deref(), outcome) {
        (Some(_code), Ok(o)) => panic!(
            "entry {} expected error code {:?} but got Ok({:?})",
            entry.id, expected.expected_error_code, o
        ),
        (Some(code), Err(e)) => {
            // Cross-walk through ErrorCode collapse. The closed
            // ErrorCode enum is SCREAMING_SNAKE_CASE-serialized, so a
            // JSON round-trip gives us the wire form without needing a
            // helper method.
            let collapsed = doiget_core::ErrorCode::from(e);
            let wire = serde_json::to_value(collapsed)
                .ok()
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .unwrap_or_default();
            assert_eq!(wire, code, "entry {} error code mismatch", entry.id);
        }
        (None, Ok(o)) => {
            assert_eq!(o.source, expected.source, "entry {} source", entry.id);
            // Title is asserted against the JSON metadata payload. The
            // shape differs per source: Crossref puts the title in
            // `message.title[0]`; Unpaywall returns the bare string
            // under `.title`.
            let got_title = match o.source.as_str() {
                "crossref" => o
                    .metadata
                    .get("title")
                    .and_then(|v| v.as_array())
                    .and_then(|arr| arr.first())
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                "unpaywall" => o
                    .metadata
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                other => panic!("entry {} unexpected source {other}", entry.id),
            };
            assert_eq!(got_title, expected.title, "entry {} title", entry.id);
            if let Some(want_oa) = expected.oa_url.as_deref() {
                assert_eq!(
                    o.oa_url.as_deref(),
                    Some(want_oa),
                    "entry {} oa_url",
                    entry.id
                );
            }
            if let Some(want_license) = expected.license.as_deref() {
                assert_eq!(
                    o.license.as_deref(),
                    Some(want_license),
                    "entry {} license",
                    entry.id
                );
            }
        }
        (None, Err(e)) => panic!("entry {} expected Ok but got Err: {e:?}", entry.id),
    }
}

/// Drive a single arXiv entry through `metadata_only` against a
/// wiremock origin and assert the outcome.
async fn run_arxiv_entry(entry: &Entry, expected: &Expected) {
    let server = MockServer::start().await;
    let host = host_of(&server.uri());

    let atom_rel = entry.atom_response.as_deref().unwrap_or_else(|| {
        panic!(
            "entry {} kind {} requires atom_response",
            entry.id, entry.kind
        )
    });
    let body = load_fixture_body(atom_rel);
    Mock::given(method("GET"))
        .and(path("/api/query"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(&server)
        .await;

    let env = EnvGuard::snapshot();
    env.set("DOIGET_ARXIV_BASE", &server.uri());

    let (_td, ctx) = build_ctx(&[("arxiv", &host)]);
    let profile = CapabilityProfile::from_env().expect("clean env");

    let r = Ref::parse(&entry.ref_str)
        .unwrap_or_else(|e| panic!("ref parse failed for entry {}: {e}", entry.id));

    assert_eq!(
        r.safekey().as_str(),
        expected.safekey,
        "entry {} safekey mismatch",
        entry.id
    );

    let outcome = metadata_only(&r, &profile, &ctx)
        .await
        .unwrap_or_else(|e| panic!("entry {} metadata_only failed: {e:?}", entry.id));
    assert_eq!(outcome.source, expected.source, "entry {} source", entry.id);
    // arxiv source returns the parsed Atom feed; title is at the top
    // level of the JSON object (see
    // `crates/doiget-core/src/sources/arxiv.rs::parse_atom_feed`).
    let got_title = outcome
        .metadata
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    assert_eq!(got_title, expected.title, "entry {} title", entry.id);
    if let Some(want_license) = expected.license.as_deref() {
        assert_eq!(
            outcome.license.as_deref(),
            Some(want_license),
            "entry {} license",
            entry.id
        );
    }
}

/// Top-level reference test. Runs every enabled `[[entry]]` row in
/// `index.toml`. Each row's outcome is asserted by panicking on
/// mismatch; the cargo-test runner's per-`#[test]` isolation keeps the
/// whole-suite outcome on the file's single test boundary.
///
/// Adding a new entry to `index.toml` is sufficient — no edits to this
/// file are needed unless the entry kind itself is new (in which case
/// add an arm to the dispatch `match` below).
#[tokio::test]
#[serial_test::serial]
async fn real_world_fixtures_all_entries() {
    let index = load_index();
    assert_eq!(
        index.schema_version.as_deref(),
        Some("1"),
        "index.toml schema_version mismatch"
    );
    assert!(
        !index.entries.is_empty(),
        "index.toml must declare at least one [[entry]]"
    );

    let mut ran = 0usize;
    let mut skipped = 0usize;
    for entry in &index.entries {
        if entry.disabled {
            skipped += 1;
            println!("skip entry: id={} kind={}", entry.id, entry.kind);
            continue;
        }
        let expected = load_expected(&entry.expected);
        println!(
            "run entry: id={} kind={} ref={} provenance={:?} last_refreshed_iso={:?} notes={:?}",
            entry.id,
            entry.kind,
            entry.ref_str,
            entry.provenance,
            entry.last_refreshed_iso,
            entry.notes,
        );
        match entry.kind.as_str() {
            "doi-crossref"
            | "doi-no-oa"
            | "doi-crossref-fail-unpaywall"
            | "doi-long-suffix"
            | "doi-special-chars" => run_doi_entry(entry, &expected).await,
            "arxiv-new" | "arxiv-old" | "arxiv-versioned" => {
                run_arxiv_entry(entry, &expected).await
            }
            other => panic!("unknown entry kind {other} for id {}", entry.id),
        }
        ran += 1;
    }
    println!("real-world fixtures: ran={ran} skipped={skipped}");
    assert!(ran > 0, "no fixture entries actually ran");
}
