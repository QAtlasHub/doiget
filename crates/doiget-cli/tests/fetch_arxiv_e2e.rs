//! End-to-end wiremock-driven test for `doiget fetch <arxiv-id>`.
//!
//! This is the binding success criterion for Phase 1 per
//! [`docs/PHASES.md`](../../../docs/PHASES.md) §4: the arXiv path produces a
//! complete on-disk artifact (PDF + metadata TOML) and a hash-chained
//! provenance log with the expected event sequence.
//!
//! ## What is exercised
//!
//! - `doiget_cli::commands::fetch::run` end-to-end (no child-process spawn).
//! - `ArxivSource::with_base` substitution via `DOIGET_ARXIV_BASE`.
//! - `HttpClient::new_for_tests_allow_http_multi` (selected by the
//!   orchestrator when `DOIGET_*_BASE` env vars are present).
//! - `FsStore::write` atomic-rename code path for PDF + metadata.
//! - `ProvenanceLog::append` writing the four expected rows
//!   (`SessionStart`, `Fetch`, `StoreWrite`, `SessionEnd`).
//!
//! ## Network purity
//!
//! Per the network-purity guard, this test makes NO outbound calls. All
//! HTTP traffic terminates at a `wiremock::MockServer` on `127.0.0.1:N`,
//! reached via the env-var override.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use camino::{Utf8Path, Utf8PathBuf};
use doiget_cli::commands::fetch;
use doiget_cli::commands::output::OutputMode;
use doiget_core::provenance::{LogEvent, LogResult, LogRow};
use doiget_core::store::Metadata;
use serial_test::serial;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;
use common::env_guard::EnvGuard;

fn read_log_rows(path: &Utf8PathBuf) -> Vec<LogRow> {
    let raw = std::fs::read_to_string(path.as_std_path()).expect("read log");
    raw.lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str::<LogRow>(l).expect("valid LogRow"))
        .collect()
}

#[tokio::test]
#[serial]
async fn arxiv_2401_12345_end_to_end() {
    // Step 1: spin up a wiremock that serves the canonical arXiv PDF path
    // with a body whose first 5 bytes pass `HttpClient::fetch_pdf`'s magic-
    // byte check (`%PDF-`).
    let server = MockServer::start().await;
    let body = b"%PDF-1.7\n%fixture-bytes\n".to_vec();
    Mock::given(method("GET"))
        .and(path("/pdf/2401.12345.pdf"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
        .mount(&server)
        .await;

    // Step 2: stage a temp dir for store + log artifacts.
    let td = TempDir::new().expect("tempdir");
    let temp_root: Utf8PathBuf = Utf8Path::from_path(td.path())
        .expect("temp dir is utf-8")
        .to_path_buf();
    let store_root = temp_root.join("papers");
    let log_path = temp_root.join("log.jsonl");

    let env = EnvGuard::new(&[
        "DOIGET_STORE_ROOT",
        "DOIGET_LOG_PATH",
        "DOIGET_ARXIV_BASE",
        "DOIGET_CROSSREF_BASE",
        "DOIGET_UNPAYWALL_BASE",
        "DOIGET_CONTACT_EMAIL",
        "DOIGET_UNPAYWALL_EMAIL",
    ]);
    env.set("DOIGET_STORE_ROOT", store_root.as_str());
    env.set("DOIGET_LOG_PATH", log_path.as_str());
    env.set("DOIGET_ARXIV_BASE", &server.uri());

    // Step 3: run the orchestrator end-to-end. No child binary; no real
    // network traffic.
    fetch::run_with_options("arxiv:2401.12345".to_string(), false, OutputMode::Human)
        .await
        .expect("fetch::run_with_options succeeds");

    // Step 4: assert the on-disk PDF.
    let pdf_path = store_root.join("arxiv_2401.12345.pdf");
    assert!(
        pdf_path.exists(),
        "expected PDF at {pdf_path}; tree: {:?}",
        std::fs::read_dir(temp_root.as_std_path())
            .map(|d| d.flatten().map(|e| e.path()).collect::<Vec<_>>())
    );
    let pdf_bytes = std::fs::read(pdf_path.as_std_path()).expect("read pdf");
    assert_eq!(pdf_bytes, body, "stored PDF must match wiremock body");

    // Step 5: assert the metadata TOML round-trips and has the expected
    // [doiget].source value.
    let meta_path = store_root.join(".metadata").join("arxiv_2401.12345.toml");
    let meta_raw = std::fs::read_to_string(meta_path.as_std_path()).expect("read metadata toml");
    let metadata: Metadata = toml::from_str(&meta_raw).expect("metadata round-trips");
    assert_eq!(metadata.schema_version, "1.0");
    let doiget = metadata.doiget.expect("[doiget] table present");
    assert_eq!(doiget.source, "arxiv");
    assert_eq!(doiget.size_bytes, body.len() as u64);
    assert_eq!(doiget.license, "arxiv-default");
    assert_eq!(
        metadata.arxiv_id.map(|a| a.as_str().to_string()),
        Some("2401.12345".to_string())
    );

    // Step 6: assert the provenance log has the expected event sequence:
    // SessionStart -> Fetch (ok) -> StoreWrite (ok) -> SessionEnd (ok).
    let rows = read_log_rows(&log_path);
    assert_eq!(
        rows.len(),
        4,
        "expected 4 rows (start/fetch/store/end), got {}: {:?}",
        rows.len(),
        rows.iter().map(|r| (r.event, r.result)).collect::<Vec<_>>()
    );

    assert_eq!(rows[0].event, LogEvent::SessionStart);
    assert_eq!(rows[0].result, LogResult::Ok);
    assert_eq!(rows[0].ref_.as_deref(), Some("2401.12345"));

    assert_eq!(rows[1].event, LogEvent::Fetch);
    assert_eq!(rows[1].result, LogResult::Ok);
    assert_eq!(rows[1].source.as_deref(), Some("arxiv"));
    assert_eq!(rows[1].size_bytes, Some(body.len() as u64));

    assert_eq!(rows[2].event, LogEvent::StoreWrite);
    assert_eq!(rows[2].result, LogResult::Ok);
    assert_eq!(rows[2].source.as_deref(), Some("arxiv"));

    assert_eq!(rows[3].event, LogEvent::SessionEnd);
    assert_eq!(rows[3].result, LogResult::Ok);

    // Sanity: the hash chain links rows in file order.
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

/// Issue #303: when the Atom feed is available, the PDF-fetch path stores
/// the REAL title / authors / year / categories (not the `arxiv:<id>`
/// placeholder), so a later `bib` / `info` shows a usable entry. The arXiv
/// source fetches the Atom feed from the same `DOIGET_ARXIV_BASE`, so one
/// mock serves both `/api/query` and `/pdf/...`.
#[tokio::test]
#[serial]
async fn arxiv_fetch_stores_real_atom_metadata() {
    const ATOM: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <entry>
    <id>http://arxiv.org/abs/2401.12345v1</id>
    <title>A Real Paper Title</title>
    <summary>Abstract text.</summary>
    <author><name>Jane Doe</name></author>
    <author><name>John Roe</name></author>
    <published>2024-01-15T00:00:00Z</published>
    <category term="cond-mat.str-el" scheme="http://arxiv.org/schemas/atom"/>
    <category term="cond-mat.dis-nn" scheme="http://arxiv.org/schemas/atom"/>
  </entry>
</feed>"#;

    let server = MockServer::start().await;
    let body = b"%PDF-1.7\n%fixture-bytes\n".to_vec();
    Mock::given(method("GET"))
        .and(path("/pdf/2401.12345.pdf"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/query"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ATOM))
        .mount(&server)
        .await;

    let td = TempDir::new().expect("tempdir");
    let temp_root: Utf8PathBuf = Utf8Path::from_path(td.path())
        .expect("temp dir is utf-8")
        .to_path_buf();
    let store_root = temp_root.join("papers");

    let env = EnvGuard::new(&[
        "DOIGET_STORE_ROOT",
        "DOIGET_LOG_PATH",
        "DOIGET_ARXIV_BASE",
        "DOIGET_CONTACT_EMAIL",
    ]);
    env.set("DOIGET_STORE_ROOT", store_root.as_str());
    env.set("DOIGET_LOG_PATH", temp_root.join("log.jsonl").as_str());
    env.set("DOIGET_ARXIV_BASE", &server.uri());

    fetch::run_with_options("arxiv:2401.12345".to_string(), false, OutputMode::Human)
        .await
        .expect("fetch succeeds");

    let meta_path = store_root.join(".metadata").join("arxiv_2401.12345.toml");
    let meta_raw = std::fs::read_to_string(meta_path.as_std_path()).expect("read metadata toml");
    let metadata: Metadata = toml::from_str(&meta_raw).expect("metadata round-trips");

    // The placeholder `arxiv:<id>` is gone; real Atom fields landed.
    assert_eq!(metadata.title, "A Real Paper Title");
    assert_eq!(
        metadata.authors,
        vec!["Jane Doe".to_string(), "John Roe".to_string()]
    );
    assert_eq!(metadata.year, Some(2024));
    assert_eq!(
        metadata.arxiv_categories,
        vec!["cond-mat.str-el".to_string(), "cond-mat.dis-nn".to_string()]
    );

    drop(env);
    drop(td);
}
