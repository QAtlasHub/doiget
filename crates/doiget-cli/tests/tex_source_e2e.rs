//! End-to-end wiremock tests for `doiget tex-source` — raw LaTeX source
//! extraction from the arXiv source API.
//!
//! ## What is exercised
//!
//! - `doiget_cli::commands::tex_source::run` end-to-end: `build_resolve_context`
//!   → `paper_tex_source::paper_tex_source` → wiremock `export.arxiv.org/src`
//!   → on-disk tex-src cache.
//! - The `DOIGET_ARXIV_BASE` override gates both the arXiv src URL
//!   (`DOIGET_ARXIV_SRC_BASE`) and the HTTP allowlist. Both are set to the
//!   same wiremock origin so the client can reach the mock.
//! - The provenance contract: one `Fetch` row under `source = "arxiv-src"`.
//! - The cache contract: a `<cache_root>/tex-src/<safekey>.json` entry is
//!   written; a second call is served from it (single-shot mock).
//! - DOI rejection, PDF-only note, implicit vs explicit Quiet artifact rule.
//!
//! ## Network purity
//!
//! No outbound calls: all HTTP terminates at a `wiremock::MockServer` on
//! `127.0.0.1:N`, reached via `DOIGET_ARXIV_BASE` / `DOIGET_ARXIV_SRC_BASE`.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use assert_cmd::Command;
use camino::Utf8PathBuf;
use flate2::write::GzEncoder;
use flate2::Compression;
use predicates::str::contains;
use serial_test::serial;
use std::io::Write as _;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use doiget_cli::commands::output::OutputMode;
use doiget_cli::commands::tex_source::run;

mod common;
use common::env_guard::EnvGuard;

/// Env keys this test suite mutates (restored on `EnvGuard` drop).
const ENV_KEYS: &[&str] = &[
    "DOIGET_ARXIV_BASE",
    "DOIGET_ARXIV_SRC_BASE",
    "DOIGET_CACHE_ROOT",
    "DOIGET_STORE_ROOT",
    "DOIGET_LOG_PATH",
    "DOIGET_MODE",
    "HOME",
    "USERPROFILE",
];

/// Minimal synthetic LaTeX document.
const SAMPLE_TEX: &[u8] =
    b"\\documentclass{article}\n\\begin{document}\nHello from arXiv.\n\\end{document}";

fn utf8(dir: &TempDir) -> Utf8PathBuf {
    Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("temp dir path must be UTF-8")
}

/// Build a gzip-compressed tar archive containing a single `main.tex`.
fn single_tex_tar_gz() -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(SAMPLE_TEX.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder
        .append_data(&mut header, "main.tex", std::io::Cursor::new(SAMPLE_TEX))
        .expect("tar append");
    let tar_bytes = builder.into_inner().expect("tar finish");
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    enc.write_all(&tar_bytes).expect("gz write");
    enc.finish().expect("gz finish")
}

/// Build a subprocess `doiget tex-source` command isolated to `root` and
/// pointing at `server_uri`. Does NOT set `DOIGET_MODE`, so the piped
/// (non-TTY) child resolves to *implicit* Quiet — the condition the
/// artifact rule (ADR-0017 Amendment 2) must override.
fn doiget_subprocess(root: &Utf8PathBuf, server_uri: &str) -> Command {
    let mut cmd = Command::cargo_bin("doiget").expect("locate doiget binary");
    cmd.env("DOIGET_ARXIV_BASE", server_uri)
        .env("DOIGET_ARXIV_SRC_BASE", server_uri)
        .env("DOIGET_CACHE_ROOT", root.join("cache").as_str())
        .env("DOIGET_STORE_ROOT", root.join("papers").as_str())
        .env("DOIGET_LOG_PATH", root.join("access.jsonl").as_str())
        .env("HOME", root.as_str())
        .env("USERPROFILE", root.as_str());
    cmd
}

// ── lifecycle: fetch → cache → cache-hit ─────────────────────────────────────

#[tokio::test]
#[serial]
async fn tex_source_extracts_logs_and_caches() {
    // Single-shot mock: a second network call would fail, proving the second
    // `run` was served from the on-disk cache.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/src/2401.12345"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(single_tex_tar_gz()))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("tempdir");
    let root = utf8(&dir);
    let cache_root = root.join("cache");
    let log_path = root.join("access.jsonl");

    let guard = EnvGuard::new(ENV_KEYS);
    guard.set("DOIGET_ARXIV_BASE", &server.uri());
    guard.set("DOIGET_ARXIV_SRC_BASE", &server.uri());
    guard.set("DOIGET_CACHE_ROOT", cache_root.as_str());
    guard.set("DOIGET_STORE_ROOT", root.join("papers").as_str());
    guard.set("DOIGET_LOG_PATH", log_path.as_str());
    guard.set("DOIGET_MODE", "quiet");
    guard.set("HOME", root.as_str());
    guard.set("USERPROFILE", root.as_str());

    // First run: network fetch, cache write, provenance log.
    let res = run(
        "arxiv:2401.12345".to_string(),
        None,
        false,
        OutputMode::Quiet,
        true,
    )
    .await;
    assert!(res.is_ok(), "tex-source run failed: {res:?}");

    // Provenance: one Fetch row under source "arxiv-src".
    let log = std::fs::read_to_string(log_path.as_std_path()).expect("read log");
    assert!(
        log.contains("\"event\":\"fetch\"") && log.contains("\"source\":\"arxiv-src\""),
        "missing arxiv-src fetch row in:\n{log}"
    );

    // Cache: a `<cache_root>/tex-src/<safekey>.json` entry was written.
    let tex_dir = cache_root.join("tex-src");
    let entries: Vec<_> = std::fs::read_dir(tex_dir.as_std_path())
        .expect("tex-src cache dir exists")
        .filter_map(Result::ok)
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|x| x.to_str())
                .map(|x| x == "json")
                .unwrap_or(false)
        })
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "exactly one cached tex-src entry expected"
    );

    // Second run: single-shot mock exhausted; success proves cache hit.
    let res2 = run(
        "arxiv:2401.12345".to_string(),
        None,
        false,
        OutputMode::Quiet,
        true,
    )
    .await;
    assert!(
        res2.is_ok(),
        "second (cached) tex-source run failed: {res2:?}"
    );
}

// ── DOI rejection ─────────────────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn tex_source_for_doi_reports_no_oa_available() {
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
    .expect_err("a DOI must error (no TeX source path for bare DOIs)");
    let exit = err
        .downcast_ref::<doiget_cli::commands::fetch::CliExit>()
        .expect("DOI path must yield a CliExit");
    assert_ne!(exit.0, 0, "exit code must be non-zero for a bare DOI");
}

// ── PDF-only actionable note ───────────────────────────────────────────────────

#[tokio::test]
#[serial]
async fn tex_source_pdf_only_exits_non_zero_with_fetch_note() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/src/2012.03644"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"%PDF-1.4 fake-pdf".as_slice()))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("tempdir");
    let root = utf8(&dir);

    // Subprocess so we can assert stderr content.
    doiget_subprocess(&root, &server.uri())
        .args(["tex-source", "arxiv:2012.03644"])
        .assert()
        .failure()
        .stderr(contains("doiget fetch arxiv:2012.03644"));
}

// ── artifact-emission: implicit vs explicit Quiet ────────────────────────────

#[tokio::test]
#[serial]
async fn tex_source_piped_non_tty_still_emits_source() {
    // ADR-0017 Amendment 2: TeX source IS the artifact. Piped without an
    // explicit `--quiet`, implicit Quiet must NOT suppress it, or
    // `doiget tex-source arxiv:… > paper.tex` would silently produce an
    // empty file.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/src/2401.12345"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(single_tex_tar_gz()))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("tempdir");
    let root = utf8(&dir);
    doiget_subprocess(&root, &server.uri())
        .args(["tex-source", "arxiv:2401.12345"])
        .assert()
        .success()
        .stdout(contains("\\documentclass"))
        .stdout(contains("Hello from arXiv."));
}

#[tokio::test]
#[serial]
async fn tex_source_explicit_quiet_suppresses_output() {
    // An explicit `--quiet` DOES suppress the artifact (exit 0, empty stdout).
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/src/2401.12345"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(single_tex_tar_gz()))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("tempdir");
    let root = utf8(&dir);
    doiget_subprocess(&root, &server.uri())
        .args(["tex-source", "arxiv:2401.12345", "--quiet"])
        .assert()
        .success()
        .stdout(predicates::str::is_empty());
}
