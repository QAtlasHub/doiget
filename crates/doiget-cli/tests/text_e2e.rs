//! End-to-end wiremock test for `doiget text` — full-text extraction from
//! ar5iv (the #281 "read" step; ADR-0032).
//!
//! ## What is exercised
//!
//! - `doiget_cli::commands::text::run` end-to-end: `build_resolve_context`
//!   → `paper_text::paper_text` → wiremock ar5iv → on-disk text cache.
//! - The ar5iv `/html/<id>` call path reached via the `DOIGET_AR5IV_BASE`
//!   override (no env-var capability gate — ADR-0032 D2: full-text
//!   extraction is Tier-1, always-on).
//! - The provenance contract: one `Fetch` row under `source = "ar5iv"`.
//! - The cache contract: a `<cache_root>/text/<safekey>.json` entry is
//!   written, and a second run is served from it (the wiremock mock is
//!   single-shot).
//!
//! ## Network purity
//!
//! No outbound calls: all HTTP terminates at a `wiremock::MockServer` on
//! `127.0.0.1:N`, reached via `DOIGET_AR5IV_BASE`.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use camino::Utf8PathBuf;
use serial_test::serial;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use doiget_cli::commands::output::OutputMode;
use doiget_cli::commands::text::run;

mod common;
use common::env_guard::EnvGuard;

/// Env keys this test mutates (restored on `EnvGuard` drop).
const ENV_KEYS: &[&str] = &[
    "DOIGET_AR5IV_BASE",
    "DOIGET_CACHE_ROOT",
    "DOIGET_STORE_ROOT",
    "DOIGET_LOG_PATH",
    "DOIGET_MODE",
    "HOME",
    "USERPROFILE",
];

/// Minimal synthetic ar5iv XHTML (title + one headed section).
const SAMPLE_AR5IV: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml">
<head><title>Extracted Paper</title></head>
<body>
  <p>Lead matter.</p>
  <section><h2>1 Introduction</h2><p>Intro body.</p></section>
</body>
</html>"#;

fn utf8(dir: &TempDir) -> Utf8PathBuf {
    Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("temp dir path must be UTF-8")
}

#[tokio::test]
#[serial]
async fn text_extracts_logs_and_caches() {
    // Single-shot mock: a second network fetch would fail, so a second
    // `run` must be served from the on-disk cache.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/html/2401.12345"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_AR5IV))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("tempdir");
    let root = utf8(&dir);
    let cache_root = root.join("cache");
    let log_path = root.join("access.jsonl");

    let guard = EnvGuard::new(ENV_KEYS);
    guard.set("DOIGET_AR5IV_BASE", &server.uri());
    guard.set("DOIGET_CACHE_ROOT", cache_root.as_str());
    guard.set("DOIGET_STORE_ROOT", root.join("papers").as_str());
    guard.set("DOIGET_LOG_PATH", log_path.as_str());
    guard.set("DOIGET_MODE", "quiet");
    guard.set("HOME", root.as_str());
    guard.set("USERPROFILE", root.as_str());

    // First run: hits wiremock, writes the cache, logs one ar5iv fetch.
    let res = run(
        "arxiv:2401.12345".to_string(),
        None,
        false, // no_cache = false → cache enabled
        OutputMode::Quiet,
    )
    .await;
    assert!(res.is_ok(), "text run failed: {res:?}");

    // Provenance: one Fetch row under source "ar5iv".
    let log = std::fs::read_to_string(log_path.as_std_path()).expect("read provenance log");
    assert!(
        log.contains("\"event\":\"fetch\"") && log.contains("\"source\":\"ar5iv\""),
        "missing ar5iv fetch row in:\n{log}"
    );

    // Cache: a `<cache_root>/text/<safekey>.json` entry was written.
    let text_dir = cache_root.join("text");
    let entries: Vec<_> = std::fs::read_dir(text_dir.as_std_path())
        .expect("text cache dir exists")
        .filter_map(Result::ok)
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|x| x.to_str())
                .map(|x| x == "json")
                .unwrap_or(false)
        })
        .collect();
    assert_eq!(entries.len(), 1, "exactly one cached text entry expected");

    // Second run: the single-shot mock is exhausted, so success here proves
    // the result was served from the cache.
    let res2 = run(
        "arxiv:2401.12345".to_string(),
        None,
        false,
        OutputMode::Quiet,
    )
    .await;
    assert!(res2.is_ok(), "second (cached) text run failed: {res2:?}");
}

#[tokio::test]
#[serial]
async fn text_for_doi_reports_no_oa_available() {
    // A bare DOI has no full-text source in PR4 (ADR-0032 D5): the command
    // must error with a NO_OA_AVAILABLE exit, not silently succeed. No
    // network is touched.
    let dir = TempDir::new().expect("tempdir");
    let root = utf8(&dir);

    let guard = EnvGuard::new(ENV_KEYS);
    guard.set("DOIGET_CACHE_ROOT", root.join("cache").as_str());
    guard.set("DOIGET_STORE_ROOT", root.join("papers").as_str());
    guard.set("DOIGET_LOG_PATH", root.join("access.jsonl").as_str());
    guard.set("DOIGET_MODE", "quiet");
    guard.set("HOME", root.as_str());
    guard.set("USERPROFILE", root.as_str());

    let err = run(
        "10.1234/example".to_string(),
        None,
        false,
        OutputMode::Quiet,
    )
    .await
    .expect_err("a DOI must error (no full-text source)");
    // The command exits via `CliExit` carrying the NO_OA_AVAILABLE code.
    let exit = err
        .downcast_ref::<doiget_cli::commands::fetch::CliExit>()
        .expect("DOI path must yield a CliExit");
    assert_ne!(exit.0, 0, "exit code must be non-zero for a DOI");
}
