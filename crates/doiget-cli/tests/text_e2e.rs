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

use assert_cmd::Command;
use camino::Utf8PathBuf;
use predicates::str::contains;
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
        true, // DOIGET_MODE=quiet → explicit Quiet, output suppressed
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
        true,
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
        true,
    )
    .await
    .expect_err("a DOI must error (no full-text source)");
    // The command exits via `CliExit` carrying the NO_OA_AVAILABLE code.
    let exit = err
        .downcast_ref::<doiget_cli::commands::fetch::CliExit>()
        .expect("DOI path must yield a CliExit");
    assert_ne!(exit.0, 0, "exit code must be non-zero for a DOI");
}

#[tokio::test]
#[serial]
async fn text_unconverted_render_exits_non_zero_never_silent() {
    // Issue #302: when ar5iv returns a 200 with no readable prose (the paper
    // was never converted to HTML), `text` MUST NOT exit 0 with empty output
    // — that is the silent-success that agents misread as a bad identifier.
    // It must surface as a non-zero `CliExit` (the `TEXT_UNAVAILABLE` code is
    // asserted at the core/MCP layers; here we pin the never-exit-0 contract).
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/html/2012.03644"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string("<html><head></head><body></body></html>"),
        )
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("tempdir");
    let root = utf8(&dir);

    let guard = EnvGuard::new(ENV_KEYS);
    guard.set("DOIGET_AR5IV_BASE", &server.uri());
    guard.set("DOIGET_CACHE_ROOT", root.join("cache").as_str());
    guard.set("DOIGET_STORE_ROOT", root.join("papers").as_str());
    guard.set("DOIGET_LOG_PATH", root.join("access.jsonl").as_str());
    guard.set("DOIGET_MODE", "quiet");
    guard.set("HOME", root.as_str());
    guard.set("USERPROFILE", root.as_str());

    let err = run(
        "arxiv:2012.03644".to_string(),
        None,
        true, // no_cache: exercise the live render path, not a cache hit
        OutputMode::Quiet,
        true,
    )
    .await
    .expect_err("an unconverted render must error, never silently succeed");
    let exit = err
        .downcast_ref::<doiget_cli::commands::fetch::CliExit>()
        .expect("unavailable text must yield a CliExit");
    assert_ne!(
        exit.0, 0,
        "exit code must be non-zero when no text is produced"
    );
}

/// Build env for a subprocess `doiget` run against `server`, isolated to
/// `root`. Deliberately does NOT set `DOIGET_MODE`, so the piped (non-TTY)
/// child resolves to *implicit* Quiet — the exact condition the artifact
/// rule must override.
fn doiget_subprocess(root: &Utf8PathBuf, server_uri: &str) -> Command {
    let mut cmd = Command::cargo_bin("doiget").expect("locate doiget binary");
    let p = root.as_str();
    cmd.env("DOIGET_AR5IV_BASE", server_uri)
        .env("DOIGET_CACHE_ROOT", root.join("cache").as_str())
        .env("DOIGET_STORE_ROOT", root.join("papers").as_str())
        .env("DOIGET_LOG_PATH", root.join("access.jsonl").as_str())
        .env("HOME", p)
        .env("USERPROFILE", p);
    cmd
}

#[tokio::test]
#[serial]
async fn text_piped_non_tty_still_emits_prose() {
    // Review #318: extracted paper prose IS the artifact. Piped without an
    // explicit `--quiet`, the mode is *implicit* Quiet; the artifact rule
    // (ADR-0017 Amendment 2, extended to `text`) must still emit, or
    // `doiget text arxiv:… > paper.txt` would silently write an empty file.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/html/2401.12345"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_AR5IV))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("tempdir");
    let root = utf8(&dir);
    doiget_subprocess(&root, &server.uri())
        .args(["text", "arxiv:2401.12345"])
        .assert()
        .success()
        // The rendered prose reaches stdout — not blanked by implicit Quiet.
        .stdout(contains("Extracted Paper"))
        .stdout(contains("Intro body."));
}

#[tokio::test]
#[serial]
async fn text_explicit_quiet_still_suppresses_prose() {
    // The flip side: an *explicit* `--quiet` DOES suppress the artifact
    // (exit 0, empty stdout) — the artifact rule only overrides the implicit
    // non-TTY fallback, not a deliberate quiet request.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/html/2401.12345"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_AR5IV))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("tempdir");
    let root = utf8(&dir);
    doiget_subprocess(&root, &server.uri())
        .args(["text", "arxiv:2401.12345", "--quiet"])
        .assert()
        .success()
        .stdout(predicates::str::is_empty());
}

#[tokio::test]
#[serial]
async fn text_unavailable_prints_actionable_fetch_note() {
    // Review #318 / #302: an ar5iv 200 with no extractable prose must fail
    // with the ACTIONABLE `= note:` naming the exact fetch command, not just
    // a bare non-zero exit. Pins the note string a human / agent acts on.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/html/2012.03644"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string("<html><head></head><body></body></html>"),
        )
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("tempdir");
    let root = utf8(&dir);
    doiget_subprocess(&root, &server.uri())
        .args(["text", "arxiv:2012.03644"])
        .assert()
        .failure()
        .stderr(contains(
            "fetch the PDF instead: `doiget fetch arxiv:2012.03644`",
        ));
}
