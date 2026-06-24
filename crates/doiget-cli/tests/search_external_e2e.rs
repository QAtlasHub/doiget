//! End-to-end wiremock test for `doiget search` **external discovery**
//! (the default scope since ADR-0031).
//!
//! ## What is exercised
//!
//! - `doiget_cli::commands::search::run(.., local = false, ..)` end-to-end:
//!   `FetchHarness::from_env` → `discovery::paper_search` → wiremock →
//!   provenance bookends.
//! - The OpenAlex `/works?search=` call path reached via the
//!   `DOIGET_OPENALEX_BASE` override (no env-var capability gate — ADR-0031
//!   D1: discovery is Tier-1, always-on).
//! - The provenance contract: a `SessionStart`, one `Metadata`/`Fetch`
//!   row under `source = "openalex"`, and a `SessionEnd`.
//!
//! ## Network purity
//!
//! No outbound calls: all HTTP terminates at a `wiremock::MockServer` on
//! `127.0.0.1:N`, reached via `DOIGET_OPENALEX_BASE`.
//!
//! Output assertions use `OutputMode::Quiet` (no stdout to capture in
//! process); the result shaping is covered by `doiget-core`'s
//! `discovery` unit tests.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use camino::Utf8PathBuf;
use serial_test::serial;
use tempfile::TempDir;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use doiget_cli::commands::output::OutputMode;
use doiget_cli::commands::search::{run, ExternalArgs, SortArg};

mod common;
use common::env_guard::EnvGuard;

/// Env keys this test mutates (restored on `EnvGuard` drop).
const ENV_KEYS: &[&str] = &[
    "DOIGET_OPENALEX_BASE",
    "DOIGET_STORE_ROOT",
    "DOIGET_LOG_PATH",
    "DOIGET_CONTACT_EMAIL",
    "DOIGET_MODE",
    "HOME",
    "USERPROFILE",
];

/// Minimal synthetic OpenAlex `/works` search response (one result).
const SAMPLE: &str = r#"{
    "meta": { "count": 1, "per_page": 25 },
    "results": [
        {
            "id": "https://openalex.org/W777",
            "doi": "https://doi.org/10.1234/discovery",
            "title": "Discovered Paper",
            "publication_year": 2023,
            "cited_by_count": 5,
            "abstract_inverted_index": { "An": [0], "abstract": [1] },
            "authorships": [ { "author": { "display_name": "Grace Hopper" } } ],
            "open_access": { "oa_status": "gold" }
        }
    ]
}"#;

fn utf8(dir: &TempDir) -> Utf8PathBuf {
    Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("temp dir path must be UTF-8")
}

#[tokio::test]
#[serial]
async fn external_search_runs_and_logs_openalex_fetch() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/works"))
        // #290: the query is now a `title_and_abstract.search` FILTER clause,
        // not the loose top-level `search=` parameter.
        .and(query_param(
            "filter",
            "title_and_abstract.search:tropical tensor networks",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("tempdir");
    let root = utf8(&dir);
    let store_root = root.join("papers");
    let log_path = root.join("access.jsonl");

    let guard = EnvGuard::new(ENV_KEYS);
    guard.set("DOIGET_OPENALEX_BASE", &server.uri());
    guard.set("DOIGET_STORE_ROOT", store_root.as_str());
    guard.set("DOIGET_LOG_PATH", log_path.as_str());
    guard.set("DOIGET_CONTACT_EMAIL", "doiget@localhost");
    guard.set("DOIGET_MODE", "quiet");
    guard.set("HOME", root.as_str());
    guard.set("USERPROFILE", root.as_str());

    let ext = ExternalArgs {
        limit: 25,
        from_year: None,
        to_year: None,
        oa_only: false,
        min_citations: None,
        min_fwci: None,
        min_percentile: None,
        author: None,
        venue: None,
        publisher: None,
        sort: SortArg::Relevance,
    };

    let res = run(
        "tropical tensor networks".to_string(),
        false, // local = false → external discovery
        None,
        ext,
        OutputMode::Quiet,
        true,
    )
    .await;
    assert!(res.is_ok(), "external search run failed: {res:?}");

    // Provenance: a Metadata/Fetch row under source "openalex", bookended
    // by a SessionStart / SessionEnd.
    // `LogEvent` serializes `#[serde(rename_all = "snake_case")]`.
    let log = std::fs::read_to_string(log_path.as_std_path()).expect("read provenance log");
    assert!(
        log.contains("\"event\":\"session_start\""),
        "missing session_start row in:\n{log}"
    );
    assert!(
        log.contains("\"event\":\"fetch\"") && log.contains("\"source\":\"openalex\""),
        "missing openalex fetch row in:\n{log}"
    );
    assert!(
        log.contains("\"event\":\"session_end\""),
        "missing session_end row in:\n{log}"
    );
}

#[tokio::test]
#[serial]
async fn external_search_min_fwci_and_percentile_become_filter_clauses() {
    // Review #318 / #290: the `--min-fwci` / `--min-percentile` triage flags
    // must travel CLI args → ExternalArgs → PaperSearchQuery → the OpenAlex
    // `filter=` value as AND-joined clauses. Exercise the whole chain end to
    // end so a typo in the mapping cannot pass unnoticed. The mock only
    // answers if the exact composed filter arrives.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/works"))
        .and(query_param(
            "filter",
            "title_and_abstract.search:tropical tensor networks,fwci:>2.5,cited_by_percentile_year.min:75",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("tempdir");
    let root = utf8(&dir);
    let store_root = root.join("papers");
    let log_path = root.join("access.jsonl");

    let guard = EnvGuard::new(ENV_KEYS);
    guard.set("DOIGET_OPENALEX_BASE", &server.uri());
    guard.set("DOIGET_STORE_ROOT", store_root.as_str());
    guard.set("DOIGET_LOG_PATH", log_path.as_str());
    guard.set("DOIGET_CONTACT_EMAIL", "doiget@localhost");
    guard.set("DOIGET_MODE", "quiet");
    guard.set("HOME", root.as_str());
    guard.set("USERPROFILE", root.as_str());

    let ext = ExternalArgs {
        limit: 25,
        from_year: None,
        to_year: None,
        oa_only: false,
        min_citations: None,
        min_fwci: Some(2.5),
        min_percentile: Some(75),
        author: None,
        venue: None,
        publisher: None,
        sort: SortArg::Relevance,
    };

    let res = run(
        "tropical tensor networks".to_string(),
        false,
        None,
        ext,
        OutputMode::Quiet,
        true,
    )
    .await;
    // The mock only matches the fully-composed filter, so a successful run
    // proves both clauses were appended in the documented order.
    assert!(
        res.is_ok(),
        "external search with impact filters failed: {res:?}"
    );
}
