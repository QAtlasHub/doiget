//! End-to-end wiremock tests for `doiget source` — arXiv source-bundle /
//! figures download to a directory (ADR-0034, issue #343).
//!
//! Subprocess (`assert_cmd`) tests so the on-disk effect (files materialised
//! under `--out`) is asserted directly. All HTTP terminates at a local
//! `wiremock::MockServer` (no outbound network), reached via
//! `DOIGET_ARXIV_BASE` / `DOIGET_ARXIV_SRC_BASE`.

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

fn utf8(dir: &TempDir) -> Utf8PathBuf {
    Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("temp dir path must be UTF-8")
}

/// gzip a tar built from `(name, bytes)` entries.
fn tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, data) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, name, std::io::Cursor::new(*data))
            .expect("tar append");
    }
    let tar_bytes = builder.into_inner().expect("tar finish");
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    enc.write_all(&tar_bytes).expect("gz write");
    enc.finish().expect("gz finish")
}

/// A multi-file submission: a main `.tex`, a `.bib`, and a nested figure.
fn sample_bundle() -> Vec<u8> {
    tar_gz(&[
        (
            "main.tex",
            b"\\documentclass{article}\\begin{document}Hi\\end{document}",
        ),
        ("refs.bib", b"@article{x,title={t}}"),
        ("figs/plot.png", b"\x89PNG\r\n\x1a\n"),
    ])
}

/// Subprocess `doiget` isolated to `root`, pointed at `server_uri`. Mirrors the
/// tex-source e2e helper; `DOIGET_MODE` is cleared so the child's output mode is
/// deterministic regardless of the parent test environment.
fn doiget_subprocess(root: &Utf8PathBuf, server_uri: &str) -> Command {
    let mut cmd = Command::cargo_bin("doiget").expect("locate doiget binary");
    cmd.env("DOIGET_ARXIV_BASE", server_uri)
        .env("DOIGET_ARXIV_SRC_BASE", server_uri)
        .env("DOIGET_CACHE_ROOT", root.join("cache").as_str())
        .env("DOIGET_STORE_ROOT", root.join("papers").as_str())
        .env("DOIGET_LOG_PATH", root.join("access.jsonl").as_str())
        .env("HOME", root.as_str())
        .env("USERPROFILE", root.as_str())
        .env_remove("DOIGET_MODE");
    cmd
}

#[tokio::test]
#[serial]
async fn source_bundle_writes_all_files() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/src/2401.12345"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(sample_bundle()))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("tempdir");
    let root = utf8(&dir);
    let out = root.join("out");

    doiget_subprocess(&root, &server.uri())
        .args(["source", "arxiv:2401.12345", "--out", out.as_str()])
        .assert()
        .success();

    assert!(out.join("main.tex").exists(), "main.tex written");
    assert!(out.join("refs.bib").exists(), "refs.bib written");
    assert!(
        out.join("figs").join("plot.png").exists(),
        "figs/plot.png written"
    );
}

#[tokio::test]
#[serial]
async fn source_figures_only_writes_only_images() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/src/2401.12345"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(sample_bundle()))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("tempdir");
    let root = utf8(&dir);
    let out = root.join("figs-out");

    doiget_subprocess(&root, &server.uri())
        .args([
            "source",
            "arxiv:2401.12345",
            "--out",
            out.as_str(),
            "--figures-only",
        ])
        .assert()
        .success();

    assert!(out.join("figs").join("plot.png").exists(), "figure written");
    assert!(
        !out.join("main.tex").exists(),
        "tex NOT written under --figures-only"
    );
    assert!(
        !out.join("refs.bib").exists(),
        "bib NOT written under --figures-only"
    );
}

#[tokio::test]
#[serial]
async fn source_for_doi_exits_non_zero() {
    let dir = TempDir::new().expect("tempdir");
    let root = utf8(&dir);
    let out = root.join("out");
    // A bare DOI is rejected before any fetch — the server is never contacted.
    doiget_subprocess(&root, "http://127.0.0.1:9")
        .args(["source", "10.1234/example", "--out", out.as_str()])
        .assert()
        .failure();
}

#[tokio::test]
#[serial]
async fn source_pdf_only_exits_non_zero_with_note() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/src/2012.03644"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"%PDF-1.4 fake".as_slice()))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("tempdir");
    let root = utf8(&dir);
    let out = root.join("out");
    doiget_subprocess(&root, &server.uri())
        .args(["source", "arxiv:2012.03644", "--out", out.as_str()])
        .assert()
        .failure()
        .stderr(contains("doiget fetch arxiv:2012.03644"));
}
