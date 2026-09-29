//! #608, through the real binary: what a human sees on `fetch`, and the
//! `metadata_quality` a `verify` row carries, for a Crossref record that
//! lost characters to U+FFFD. (Review of #619: both surfaces were wired and
//! untested.)
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DOI: &str = "10.1007/BF01340294";

async fn mock() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/works/{DOI}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "ok",
            "message": {
                "title": ["N\u{FFFD}herungsmethode zur L\u{FFFD}sung"],
                "author": [{"family": "Fock", "given": "V."}],
                "issued": {"date-parts": [[1930]]},
                "container-title": ["Zeitschrift f\u{FFFD}r Physik"]
            }
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v2/10.1007%2FBF01340294"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "doi": DOI, "is_oa": false, "oa_status": "closed",
            "best_oa_location": null, "oa_locations": []
        })))
        .mount(&server)
        .await;
    server
}

fn doiget(td: &TempDir, server: &MockServer) -> Command {
    let root = td.path().to_str().unwrap();
    let mut cmd = Command::cargo_bin("doiget").expect("binary");
    cmd.current_dir(td.path())
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root)
        .env("DOIGET_STORE_ROOT", format!("{root}/papers"))
        .env("DOIGET_LOG_PATH", format!("{root}/log.jsonl"))
        .env("DOIGET_CONTACT_EMAIL", "test@example.org")
        .env("DOIGET_CROSSREF_BASE", server.uri())
        .env("DOIGET_UNPAYWALL_BASE", format!("{}/v2", server.uri()))
        .env("DOIGET_MODE", "human")
        .env_remove("DOIGET_ENABLE_S2")
        .env_remove("DOIGET_ENABLE_OPENALEX");
    cmd
}

#[tokio::test(flavor = "multi_thread")]
async fn fetch_names_the_damaged_fields_and_the_switch_that_would_repair_them() {
    let server = mock().await;
    let td = TempDir::new().expect("tempdir");
    let mut cmd = doiget(&td, &server);
    cmd.args(["fetch", DOI]);
    tokio::task::spawn_blocking(move || {
        cmd.assert()
            .success()
            .stderr(predicate::str::contains(
                "warning: title, venue still carry U+FFFD",
            ))
            .stderr(predicate::str::contains("set DOIGET_ENABLE_S2=1"));
    })
    .await
    .expect("join");
}

#[tokio::test(flavor = "multi_thread")]
async fn verify_flags_a_valid_record_that_lost_characters_without_failing_it() {
    let server = mock().await;
    let td = TempDir::new().expect("tempdir");
    std::fs::write(
        td.path().join("refs.bib"),
        format!(
            "@article{{fock1930, author={{Fock}}, title={{x}}, doi={{{DOI}}}, year={{1930}}}}\n"
        ),
    )
    .expect("bib");
    let mut cmd = doiget(&td, &server);
    cmd.args(["verify", "refs.bib"]);
    let out =
        tokio::task::spawn_blocking(move || cmd.assert().success().get_output().stdout.clone())
            .await
            .expect("join");
    let row: serde_json::Value =
        serde_json::from_str(String::from_utf8(out).unwrap().lines().next().unwrap()).unwrap();
    assert_eq!(row["status"], "valid");
    assert_eq!(
        row["metadata_quality"],
        serde_json::json!(["replacement_char:title", "replacement_char:venue"])
    );
}
