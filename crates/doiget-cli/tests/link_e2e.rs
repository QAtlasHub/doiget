//! End-to-end wiremock test for `doiget link` — DOI → arXiv preprint
//! resolution over OpenAlex (#281 item 5).
//!
//! Exercises `doiget_cli::commands::link::run` end-to-end:
//! `build_resolve_context` → `discovery::resolve_links_for_doi` → wiremock
//! `/works?filter=doi:` → identity cluster. Tier-1 OA, always-on (no env
//! gate). All HTTP terminates at a `wiremock::MockServer` via
//! `DOIGET_OPENALEX_BASE`; no outbound network.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use camino::Utf8PathBuf;
use serial_test::serial;
use tempfile::TempDir;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use doiget_cli::commands::link::run;
use doiget_cli::commands::output::OutputMode;

mod common;
use common::env_guard::EnvGuard;

const ENV_KEYS: &[&str] = &[
    "DOIGET_OPENALEX_BASE",
    "DOIGET_CACHE_ROOT",
    "DOIGET_STORE_ROOT",
    "DOIGET_LOG_PATH",
    "DOIGET_CONTACT_EMAIL",
    "DOIGET_MODE",
    "HOME",
    "USERPROFILE",
];

const SAMPLE_WORK: &str = r#"{
    "meta": { "count": 1 },
    "results": [ {
        "id": "https://openalex.org/W55",
        "doi": "https://doi.org/10.1103/PhysRevB.1",
        "title": "Published Version",
        "locations": [
            { "landing_page_url": "https://journals.aps.org/prb/abstract/x" },
            { "pdf_url": "https://arxiv.org/abs/2101.54321v2" }
        ]
    } ]
}"#;

fn utf8(dir: &TempDir) -> Utf8PathBuf {
    Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("temp dir path must be UTF-8")
}

#[tokio::test]
#[serial]
async fn link_resolves_doi_to_arxiv_and_logs() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/works"))
        .and(query_param("filter", "doi:10.1103/physrevb.1"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_WORK))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("tempdir");
    let root = utf8(&dir);
    let log_path = root.join("access.jsonl");

    let guard = EnvGuard::new(ENV_KEYS);
    guard.set("DOIGET_OPENALEX_BASE", &server.uri());
    guard.set("DOIGET_CACHE_ROOT", root.join("cache").as_str());
    guard.set("DOIGET_STORE_ROOT", root.join("papers").as_str());
    guard.set("DOIGET_LOG_PATH", log_path.as_str());
    guard.set("DOIGET_MODE", "quiet");
    guard.set("HOME", root.as_str());
    guard.set("USERPROFILE", root.as_str());

    // `Doi::parse` lower-cases nothing, but OpenAlex is case-insensitive;
    // the mock matches the lower-cased filter the command sends.
    let res = run("10.1103/physrevb.1".to_string(), OutputMode::Quiet, true).await;
    assert!(res.is_ok(), "link run failed: {res:?}");

    // Provenance: one Metadata/Fetch row under source "openalex".
    let log = std::fs::read_to_string(log_path.as_std_path()).expect("read provenance log");
    assert!(
        log.contains("\"event\":\"fetch\"") && log.contains("\"source\":\"openalex\""),
        "missing openalex fetch row in:\n{log}"
    );
}

#[tokio::test]
#[serial]
async fn link_unknown_doi_exits_nonzero() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/works"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"{ "meta": { "count": 0 }, "results": [] }"#),
        )
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("tempdir");
    let root = utf8(&dir);

    let guard = EnvGuard::new(ENV_KEYS);
    guard.set("DOIGET_OPENALEX_BASE", &server.uri());
    guard.set("DOIGET_CACHE_ROOT", root.join("cache").as_str());
    guard.set("DOIGET_STORE_ROOT", root.join("papers").as_str());
    guard.set("DOIGET_LOG_PATH", root.join("access.jsonl").as_str());
    guard.set("DOIGET_MODE", "quiet");
    guard.set("HOME", root.as_str());
    guard.set("USERPROFILE", root.as_str());

    let err = run("10.0000/nope".to_string(), OutputMode::Quiet, true)
        .await
        .expect_err("an unmatched DOI must error");
    let exit = err
        .downcast_ref::<doiget_cli::commands::fetch::CliExit>()
        .expect("NOT_FOUND path must yield a CliExit");
    assert_ne!(exit.0, 0, "exit code must be non-zero for an unknown DOI");
}

#[tokio::test]
#[serial]
async fn link_rejects_arxiv_input_without_network() {
    // arXiv → DOI is a follow-up; an arXiv ref is a usage error and must
    // not touch the network.
    let guard = EnvGuard::new(ENV_KEYS);
    let _ = &guard;

    let err = run("arxiv:2401.12345".to_string(), OutputMode::Quiet, true)
        .await
        .expect_err("arXiv input must be a usage error");
    assert!(err.to_string().contains("Pass a DOI"), "got: {err}");
}
