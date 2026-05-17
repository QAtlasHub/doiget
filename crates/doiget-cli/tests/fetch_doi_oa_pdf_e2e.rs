//! End-to-end wiremock-driven tests for `doiget fetch <DOI>` with the OA
//! PDF leg enabled (Phase 1 success criterion — see
//! [`docs/PHASES.md`](../../../docs/PHASES.md) §4 and
//! [`docs/REDIRECT_ALLOWLIST.md`](../../../docs/REDIRECT_ALLOWLIST.md) §3.4).
//!
//! ## What is exercised
//!
//! - `doiget_cli::commands::fetch::run_with_options` end-to-end on a DOI input.
//! - Crossref + Unpaywall fan-out to the wiremock origin.
//! - The synthetic `oa-publisher` source key with its OA URL host check
//!   pulled from `HttpClient::new_for_tests_allow_http_multi(...)` over
//!   the same wiremock host (`127.0.0.1`).
//! - `HttpClient::fetch_pdf` magic-byte enforcement (the OA endpoint
//!   serves a body starting with `%PDF-`).
//! - `FsStore::write` atomic-rename code path for PDF + metadata.
//! - `ProvenanceLog::append` writing the expected row sequence
//!   (`SessionStart` -> 3 x `Fetch ok` -> `StoreWrite ok` -> `SessionEnd`).
//!
//! ## Network purity
//!
//! Per the network-purity guard, this test makes NO outbound calls. All
//! HTTP traffic terminates at a `wiremock::MockServer` on `127.0.0.1:N`,
//! reached via `DOIGET_*_BASE` env-var overrides.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use camino::{Utf8Path, Utf8PathBuf};
use doiget_cli::commands::fetch;
use doiget_core::provenance::{LogEvent, LogResult, LogRow};
use doiget_core::store::Metadata;
use serial_test::serial;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;
use common::env_guard::EnvGuard;

/// Env-var keys mutated by the tests in this file. Wired through the
/// `EnvGuard` above so each test's setup is hermetic.
const ENV_KEYS: &[&str] = &[
    "DOIGET_STORE_ROOT",
    "DOIGET_LOG_PATH",
    "DOIGET_ARXIV_BASE",
    "DOIGET_CROSSREF_BASE",
    "DOIGET_UNPAYWALL_BASE",
    "DOIGET_OA_PUBLISHER_BASE",
    "DOIGET_CONTACT_EMAIL",
    "DOIGET_UNPAYWALL_EMAIL",
];

const TEST_DOI: &str = "10.1234/test";
/// Percent-encoded form of `TEST_DOI` as it appears on the wire after
/// `path_segments_mut().push(...)`. Wiremock matches the encoded path.
const TEST_DOI_ENCODED: &str = "10.1234%2Ftest";

fn read_log_rows(path: &Utf8PathBuf) -> Vec<LogRow> {
    let raw = std::fs::read_to_string(path.as_std_path()).expect("read log");
    raw.lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str::<LogRow>(l).expect("valid LogRow"))
        .collect()
}

/// Crossref envelope returned by the `/works/<doi>` mock — minimal Phase 1
/// shape (title + authors + issued year). The orchestrator extracts these
/// via `extract_crossref_fields`.
fn crossref_body() -> serde_json::Value {
    serde_json::json!({
        "status": "ok",
        "message": {
            "title": ["E2E OA test paper"],
            "author": [{ "family": "Doe", "given": "Jane" }],
            "issued": { "date-parts": [[2026, 1, 1]] },
            "container-title": ["Synthetic Journal"],
            "type": "journal-article"
        }
    })
}

/// Unpaywall envelope returned by the `/v2/<doi>` mock with a
/// `best_oa_location.url_for_pdf` pointing at the same wiremock origin's
/// `/oa/file.pdf` path.
fn unpaywall_body(oa_url_for_pdf: &str) -> serde_json::Value {
    serde_json::json!({
        "doi": TEST_DOI,
        "is_oa": true,
        "title": "E2E OA test paper",
        "best_oa_location": {
            "url": oa_url_for_pdf,
            "url_for_pdf": oa_url_for_pdf,
            "license": "cc-by"
        }
    })
}

#[tokio::test]
#[serial]
async fn fetch_doi_oa_pdf_happy_path() {
    // Step 1: spin up ONE wiremock server and mount three paths on it
    // (Crossref `/works/<doi>`, Unpaywall `/v2/<doi>`, OA PDF
    // `/oa/file.pdf`). Per the design note: "Spin up TWO wiremock servers
    // (or one with multiple paths — simpler)" — we go with the one-server
    // option so a single host is on the redirect allowlist.
    let server = MockServer::start().await;
    let base_uri = server.uri();
    let oa_url = format!("{}/oa/file.pdf", base_uri);

    // Crossref uses `Url::join("/works/<doi>")` which does NOT URL-encode
    // the embedded `/` in the DOI suffix; so wiremock matches on the raw
    // form (`/works/10.1234/test`). Unpaywall, in contrast, uses
    // `path_segments_mut().push()` which DOES percent-encode (`%2F`).
    Mock::given(method("GET"))
        .and(path(format!("/works/{}", TEST_DOI)))
        .respond_with(ResponseTemplate::new(200).set_body_json(crossref_body()))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path(format!("/v2/{}", TEST_DOI_ENCODED)))
        .respond_with(ResponseTemplate::new(200).set_body_json(unpaywall_body(&oa_url)))
        .mount(&server)
        .await;

    let pdf_body = b"%PDF-fake-bytes\n".to_vec();
    Mock::given(method("GET"))
        .and(path("/oa/file.pdf"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(pdf_body.clone()))
        .mount(&server)
        .await;

    // Step 2: stage a temp dir for store + log artifacts.
    let td = TempDir::new().expect("tempdir");
    let temp_root: Utf8PathBuf = Utf8Path::from_path(td.path())
        .expect("temp dir is utf-8")
        .to_path_buf();
    let store_root = temp_root.join("papers");
    let log_path = temp_root.join("log.jsonl");

    let env = EnvGuard::new(ENV_KEYS);
    env.set("DOIGET_STORE_ROOT", store_root.as_str());
    env.set("DOIGET_LOG_PATH", log_path.as_str());
    // The CrossrefSource hits `<base>/works/<DOI>`; pass the bare server
    // URI as base so the orchestrator's URL builder lands at `/works/...`.
    env.set("DOIGET_CROSSREF_BASE", &base_uri);
    // The UnpaywallSource hits `<base>/<DOI>`; we want `/v2/<DOI>`, so
    // include the `/v2` prefix in the base.
    env.set("DOIGET_UNPAYWALL_BASE", &format!("{}/v2", base_uri));
    // Register the OA publisher allowlist host for the test client (same
    // wiremock host as the others).
    env.set("DOIGET_OA_PUBLISHER_BASE", &base_uri);

    // Step 3: run the orchestrator end-to-end. No real network traffic.
    fetch::run_with_options(format!("doi:{}", TEST_DOI), false)
        .await
        .expect("fetch::run_with_options succeeds");

    // Step 4: assert the on-disk PDF exists and starts with `%PDF-`.
    let pdf_path = store_root.join("doi_10.1234_test.pdf");
    assert!(
        pdf_path.exists(),
        "expected PDF at {pdf_path}; tree: {:?}",
        std::fs::read_dir(temp_root.as_std_path())
            .map(|d| d.flatten().map(|e| e.path()).collect::<Vec<_>>())
    );
    let pdf_bytes = std::fs::read(pdf_path.as_std_path()).expect("read pdf");
    assert_eq!(pdf_bytes, pdf_body, "stored PDF must match wiremock body");
    assert!(
        pdf_bytes.starts_with(b"%PDF-"),
        "PDF must start with magic bytes"
    );

    // Step 5: metadata TOML round-trips and has [doiget].source = "oa-publisher".
    let meta_path = store_root.join(".metadata").join("doi_10.1234_test.toml");
    let meta_raw = std::fs::read_to_string(meta_path.as_std_path()).expect("read metadata toml");
    let metadata: Metadata = toml::from_str(&meta_raw).expect("metadata round-trips");
    assert_eq!(metadata.schema_version, "1.0");
    let doiget = metadata.doiget.expect("[doiget] table present");
    assert_eq!(doiget.source, "oa-publisher");
    assert_eq!(doiget.size_bytes, pdf_body.len() as u64);
    assert_eq!(doiget.license, "cc-by");
    assert_eq!(
        metadata.doi.map(|d| d.as_str().to_string()),
        Some(TEST_DOI.to_string())
    );

    // Step 6: provenance log has at least three `Fetch ok` rows
    // (Crossref, Unpaywall, oa-publisher) plus the bookend rows.
    let rows = read_log_rows(&log_path);
    let fetch_ok_rows: Vec<&LogRow> = rows
        .iter()
        .filter(|r| r.event == LogEvent::Fetch && r.result == LogResult::Ok)
        .collect();
    assert!(
        fetch_ok_rows.len() >= 3,
        "expected >=3 Fetch ok rows (crossref, unpaywall, oa-publisher); got {}: {:?}",
        fetch_ok_rows.len(),
        fetch_ok_rows
            .iter()
            .map(|r| r.source.as_deref().unwrap_or("?"))
            .collect::<Vec<_>>()
    );
    let sources: Vec<&str> = fetch_ok_rows
        .iter()
        .filter_map(|r| r.source.as_deref())
        .collect();
    assert!(
        sources.contains(&"crossref"),
        "expected a crossref Fetch ok row; got {:?}",
        sources
    );
    assert!(
        sources.contains(&"unpaywall"),
        "expected an unpaywall Fetch ok row; got {:?}",
        sources
    );
    assert!(
        sources.contains(&"oa-publisher"),
        "expected an oa-publisher Fetch ok row; got {:?}",
        sources
    );

    // Sanity: hash chain links rows in file order.
    assert_eq!(rows[0].prev_hash, "GENESIS");
    for i in 1..rows.len() {
        assert_eq!(
            rows[i].prev_hash,
            rows[i - 1].this_hash,
            "hash chain break at row {i}"
        );
    }

    drop(env);
    drop(td);
}

#[tokio::test]
#[serial]
async fn fetch_doi_oa_pdf_falls_back_to_metadata_when_host_off_allowlist() {
    // Failure-fallback path: Unpaywall hands back an OA URL whose host is
    // NOT registered in the test client's `oa-publisher` allowlist. The
    // orchestrator MUST log a `Fetch err / source=oa-publisher` row and
    // SKIP writing a PDF while still writing the metadata TOML (the
    // `informed-best-effort` posture in `docs/REDIRECT_ALLOWLIST.md` §3
    // keeps the metadata).
    //
    // Issue #145 / `docs/ERRORS.md` §3 + §6: the CLI persona must NOT
    // treat this blocked PDF leg as a clean `Ok(())`. The metadata is
    // still written (and pointed at), but `run_with_options` returns an
    // `Err` carrying a `CliExit` so the process exits non-zero — a
    // blocked PDF is no longer a silent success.
    let server = MockServer::start().await;
    let base_uri = server.uri();

    // The OA URL points at an `https://` host that is NOT one of our
    // registered allowlist entries. NOTE (issue #145 investigation): the
    // per-source host allowlist is enforced ONLY inside
    // `reqwest::redirect::Policy::custom`, which `reqwest` invokes ONLY on
    // redirect hops — there is NO initial-URL host pre-check in
    // `doiget_core::http::HttpClient::fetch_inner`. This mock mounts NO
    // redirect and `attacker.test` is unroutable, so the OA leg fails at
    // connect/DNS on the FIRST request: a genuine `HttpError::Network`
    // transport fault with NO wrapped `RedirectDenied` and therefore NO
    // `DenialContext`. It is NOT distinguishable from a flaky network at
    // the doiget-cli layer, so it correctly remains `NETWORK_ERROR` /
    // exit 1 (see the assertion + the §6.1 caveat below).
    let off_allowlist_oa_url = "https://attacker.test/file.pdf".to_string();

    // Crossref uses `Url::join("/works/<doi>")` which does NOT URL-encode
    // the embedded `/` in the DOI suffix; so wiremock matches on the raw
    // form (`/works/10.1234/test`). Unpaywall, in contrast, uses
    // `path_segments_mut().push()` which DOES percent-encode (`%2F`).
    Mock::given(method("GET"))
        .and(path(format!("/works/{}", TEST_DOI)))
        .respond_with(ResponseTemplate::new(200).set_body_json(crossref_body()))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path(format!("/v2/{}", TEST_DOI_ENCODED)))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(unpaywall_body(&off_allowlist_oa_url)),
        )
        .mount(&server)
        .await;

    let td = TempDir::new().expect("tempdir");
    let temp_root: Utf8PathBuf = Utf8Path::from_path(td.path())
        .expect("temp dir is utf-8")
        .to_path_buf();
    let store_root = temp_root.join("papers");
    let log_path = temp_root.join("log.jsonl");

    let env = EnvGuard::new(ENV_KEYS);
    env.set("DOIGET_STORE_ROOT", store_root.as_str());
    env.set("DOIGET_LOG_PATH", log_path.as_str());
    env.set("DOIGET_CROSSREF_BASE", &base_uri);
    env.set("DOIGET_UNPAYWALL_BASE", &format!("{}/v2", base_uri));
    // Register the oa-publisher source with the wiremock host only;
    // `attacker.test` will not match and the OA leg will be denied at the
    // initial-URL host check (the same closure runs on the first leg as
    // on every redirect hop).
    env.set("DOIGET_OA_PUBLISHER_BASE", &base_uri);

    // Issue #145: the blocked PDF leg must surface as a non-zero exit,
    // NOT a silent `Ok(())`. The metadata is still written (asserted
    // below) but the CLI persona gets an `error[CODE]:` line + a
    // `CliExit` carrying the `docs/ERRORS.md` §4 process code.
    //
    // Issue #145 (Option B, approved) — scope caveat. The approved
    // decision is: an off-allowlist / redirect-denied / insecure-scheme
    // OA-PDF block is a DELIBERATE policy denial and MUST surface as
    // `CAPABILITY_DENIED` / exit 3, not `NETWORK_ERROR` / exit 1
    // (`docs/ERRORS.md` §2 + §6.1). That reclassification IS implemented
    // at the doiget-cli layer in `effective_blocked_code`
    // (`crates/doiget-cli/src/commands/fetch.rs`): whenever the
    // orchestrator surfaces a `DenialContext` whose `reason` is
    // `redirect_not_in_allowlist` / `insecure_scheme` /
    // `host_in_block_list`, the CLI promotes the code to
    // `CapabilityDenied` and returns `CliExit(3)`.
    //
    // THIS test case, however, does NOT exercise that path. The core's
    // host allowlist is enforced only inside the redirect-policy closure
    // (`reqwest::redirect::Policy::custom`), which runs ONLY on redirect
    // hops; there is no initial-URL host pre-check in
    // `doiget_core::http::fetch_inner`. With an unroutable first-leg host
    // and no redirect, the OA leg fails at connect — a real
    // `HttpError::Network` with NO wrapped `RedirectDenied`, hence
    // `denial == None`. Per `docs/ERRORS.md` §6.1 a genuine transport
    // fault with no `denial_context` correctly stays `NETWORK_ERROR` /
    // exit 1. Closing the "initial OA URL host off-allowlist with no
    // redirect" gap requires an initial-URL host pre-check in
    // `crates/doiget-core/src/http.rs`, which is out of scope for this
    // CLI-only branch/PR (tracked under #145; see the PR description /
    // ERRORS.md §6.1). The metadata-still-written / PDF-not-written and
    // `error_code == NETWORK_ERROR` provenance assertions below remain.
    let err = fetch::run_with_options(format!("doi:{}", TEST_DOI), false)
        .await
        .expect_err("a blocked OA PDF leg must NOT be a silent success (issue #145)");
    let cli_exit = err
        .downcast_ref::<doiget_cli::commands::fetch::CliExit>()
        .expect("blocked PDF leg must carry a CliExit so main maps it to a §4 exit code");
    assert_eq!(
        cli_exit.0, 1,
        "first-leg connect failure to an unroutable off-allowlist host \
         has NO DenialContext (no redirect hop fired the allowlist \
         closure) → genuine NETWORK_ERROR → exit 1, per docs/ERRORS.md \
         §6.1. The policy-block → CAPABILITY_DENIED/exit-3 reclassification \
         (issue #145) is unit-covered in fetch.rs::effective_blocked_code \
         for the redirect/insecure-scheme cases that DO carry a \
         DenialContext."
    );

    // PDF MUST NOT be written.
    let pdf_path = store_root.join("doi_10.1234_test.pdf");
    assert!(
        !pdf_path.exists(),
        "PDF must NOT be written on off-allowlist host; found: {pdf_path}"
    );

    // Metadata TOML MUST be written; source falls back to the metadata
    // source label (here `unpaywall` because the license came back).
    let meta_path = store_root.join(".metadata").join("doi_10.1234_test.toml");
    assert!(
        meta_path.exists(),
        "metadata TOML must be written even when the PDF leg is denied; meta_path: {meta_path}"
    );
    let meta_raw = std::fs::read_to_string(meta_path.as_std_path()).expect("read metadata toml");
    let metadata: Metadata = toml::from_str(&meta_raw).expect("metadata round-trips");
    let doiget = metadata.doiget.expect("[doiget] table present");
    assert_ne!(
        doiget.source, "oa-publisher",
        "source must NOT be oa-publisher when the OA leg failed; got {:?}",
        doiget.source
    );
    assert_eq!(
        doiget.size_bytes, 0,
        "metadata-only fallback must report size_bytes = 0"
    );
    assert!(metadata.pdf_path.is_none(), "pdf_path must be unset");

    // Provenance log MUST have a `Fetch err` row whose source is
    // `oa-publisher`.
    let rows = read_log_rows(&log_path);
    let oa_err_rows: Vec<&LogRow> = rows
        .iter()
        .filter(|r| {
            r.event == LogEvent::Fetch
                && r.result == LogResult::Err
                && r.source.as_deref() == Some("oa-publisher")
        })
        .collect();
    assert_eq!(
        oa_err_rows.len(),
        1,
        "expected exactly one Fetch err row for oa-publisher; got {:?}",
        rows.iter()
            .map(|r| (r.event, r.result, r.source.clone()))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        oa_err_rows[0].error_code.as_deref(),
        Some("NETWORK_ERROR"),
        "fallback row must set error_code = NETWORK_ERROR"
    );

    drop(env);
    drop(td);
}

/// Issue #120: a Crossref failure must NOT abort the DOI fetch when
/// Unpaywall alone can still deliver the OA PDF. Mount Unpaywall +
/// OA-publisher normally but DO NOT mount `/works/<doi>` (wiremock
/// 404 → `CrossrefSource` returns `Err`). The PDF must still land on
/// disk; metadata title falls back to the DOI (Crossref gave nothing).
#[tokio::test]
#[serial]
async fn fetch_doi_crossref_down_unpaywall_oa_still_yields_pdf() {
    let server = MockServer::start().await;
    let base_uri = server.uri();
    let oa_url = format!("{}/oa/file.pdf", base_uri);

    // NO `/works/<doi>` mock — Crossref gets 404 and fails.
    Mock::given(method("GET"))
        .and(path(format!("/v2/{}", TEST_DOI_ENCODED)))
        .respond_with(ResponseTemplate::new(200).set_body_json(unpaywall_body(&oa_url)))
        .mount(&server)
        .await;
    let pdf_body = b"%PDF-fake-bytes\n".to_vec();
    Mock::given(method("GET"))
        .and(path("/oa/file.pdf"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(pdf_body.clone()))
        .mount(&server)
        .await;

    let td = TempDir::new().expect("tempdir");
    let temp_root: Utf8PathBuf = Utf8Path::from_path(td.path())
        .expect("temp dir is utf-8")
        .to_path_buf();
    let store_root = temp_root.join("papers");
    let log_path = temp_root.join("log.jsonl");

    let env = EnvGuard::new(ENV_KEYS);
    env.set("DOIGET_STORE_ROOT", store_root.as_str());
    env.set("DOIGET_LOG_PATH", log_path.as_str());
    env.set("DOIGET_CROSSREF_BASE", &base_uri);
    env.set("DOIGET_UNPAYWALL_BASE", &format!("{}/v2", base_uri));
    env.set("DOIGET_OA_PUBLISHER_BASE", &base_uri);

    fetch::run_with_options(format!("doi:{}", TEST_DOI), false)
        .await
        .expect("fetch must succeed via Unpaywall even though Crossref failed");

    let pdf_path = store_root.join("doi_10.1234_test.pdf");
    assert!(
        pdf_path.exists(),
        "PDF must be written even though Crossref failed; tree: {:?}",
        std::fs::read_dir(temp_root.as_std_path())
            .map(|d| d.flatten().map(|e| e.path()).collect::<Vec<_>>())
    );
    let pdf_bytes = std::fs::read(pdf_path.as_std_path()).expect("read pdf");
    assert_eq!(pdf_bytes, pdf_body);

    let meta_path = store_root.join(".metadata").join("doi_10.1234_test.toml");
    let meta_raw = std::fs::read_to_string(meta_path.as_std_path()).expect("read metadata toml");
    let metadata: Metadata = toml::from_str(&meta_raw).expect("metadata round-trips");
    let doiget = metadata.doiget.expect("[doiget] table present");
    assert_eq!(doiget.source, "oa-publisher");
    // Crossref produced nothing, so the title falls back to the DOI.
    assert_eq!(metadata.title, TEST_DOI);

    drop(env);
    drop(td);
}
