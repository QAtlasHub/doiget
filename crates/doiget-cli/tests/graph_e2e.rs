//! Slice 16 — e2e for `doiget graph <ref>` CLI subcommand.
//!
//! Whole file gated by `#[cfg(feature = "citation")]`. Runs the
//! binary as a subprocess via `assert_cmd`, routes OpenAlex
//! through a wiremock origin via `DOIGET_OPENALEX_BASE`, and
//! asserts the stdout JSON envelope shape.
//
// allow: outbound-network

#![cfg(feature = "citation")]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use assert_cmd::Command;

const SEED_WORK: &str = r#"{
    "id": "https://openalex.org/W0001",
    "doi": "https://doi.org/10.1234/seed",
    "display_name": "Seed Paper",
    "referenced_works": [
        "https://openalex.org/W0002",
        "https://openalex.org/W0003"
    ]
}"#;
const LEAF_W0002: &str = r#"{"id":"https://openalex.org/W0002","referenced_works":[]}"#;
const LEAF_W0003: &str = r#"{"id":"https://openalex.org/W0003","referenced_works":[]}"#;

#[tokio::test]
#[serial_test::serial]
async fn graph_subcommand_emits_pretty_json_envelope() -> anyhow::Result<()> {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/works/10.1234/seed"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SEED_WORK))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/works/W0002"))
        .respond_with(ResponseTemplate::new(200).set_body_string(LEAF_W0002))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/works/W0003"))
        .respond_with(ResponseTemplate::new(200).set_body_string(LEAF_W0003))
        .mount(&server)
        .await;

    let td = tempfile::TempDir::new().expect("tempdir");
    let log_path = camino::Utf8Path::from_path(td.path())
        .expect("utf-8 tempdir")
        .join("graph.jsonl");

    let assert = Command::cargo_bin("doiget")
        .expect("doiget binary built")
        .env("DOIGET_OPENALEX_BASE", server.uri())
        .env("DOIGET_LOG_PATH", log_path.as_str())
        .env(
            "DOIGET_STORE_ROOT",
            td.path().to_str().expect("utf-8 tempdir"),
        )
        .env("DOIGET_ENABLE_OPENALEX", "1")
        .env("DOIGET_CONTACT_EMAIL", "doiget@localhost")
        .args(["graph", "10.1234/seed", "--depth", "2"])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf-8 stdout");
    let json: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout not JSON: {e}; stdout={stdout}"));
    assert_eq!(json["seed_work_id"], "W0001");
    assert_eq!(json["total_visited"], 3);
    assert_eq!(json["nodes"].as_array().expect("nodes array").len(), 3);
    assert_eq!(json["edges"].as_array().expect("edges array").len(), 2);
    assert_eq!(json["truncated"], false);

    drop(td);
    Ok(())
}

#[test]
fn graph_subcommand_rejects_arxiv_seed() {
    let td = tempfile::TempDir::new().expect("tempdir");
    Command::cargo_bin("doiget")
        .expect("doiget binary built")
        .env(
            "DOIGET_STORE_ROOT",
            td.path().to_str().expect("utf-8 tempdir"),
        )
        .env("DOIGET_LOG_PATH", "")
        .env("DOIGET_ENABLE_OPENALEX", "1")
        .args(["graph", "2401.12345"])
        .assert()
        .failure();
}
