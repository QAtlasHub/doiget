//! `doiget fetch` of a closed DOI whose preprint Unpaywall does not name
//! (#636, ADR-0062), through the real binary against mocks: the success line
//! says it is the preprint, and which method found it.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use assert_cmd::Command;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TITLE: &str =
    "Environment-matrix-product operator for boundary-free large-scale quantum many-body simulations";

#[tokio::test(flavor = "multi_thread")]
async fn the_fetch_line_names_the_preprint_and_how_it_was_found() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/works/10.1103/bbnt-brjz"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "ok",
            "message": {"title": [TITLE], "author": [{"family": "Shimozono", "given": "Souta"}],
                        "issued": {"date-parts": [[2026, 6, 5]]}, "type": "journal-article"}
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v2/10.1103%2Fbbnt-brjz"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "doi": "10.1103/bbnt-brjz", "is_oa": false, "oa_status": "closed",
            "best_oa_location": null, "oa_locations": []
        })))
        .mount(&server)
        .await;
    let arxiv = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/query"))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            "<feed><entry><id>http://arxiv.org/abs/2512.07923v1</id><title>{TITLE}</title>\
             <published>2025-12-08T00:00:00Z</published>\
             <author><name>Souta Shimozono</name></author></entry></feed>"
        )))
        .mount(&arxiv)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"%PDF-1.4\npreprint\n".to_vec()))
        .mount(&arxiv)
        .await;

    let td = TempDir::new().unwrap();
    let root = td.path().to_str().unwrap().to_string();
    let mut cmd = Command::cargo_bin("doiget").expect("binary");
    cmd.current_dir(td.path())
        .env("HOME", &root)
        .env("XDG_CONFIG_HOME", &root)
        .env("DOIGET_STORE_ROOT", format!("{root}/papers"))
        .env("DOIGET_LOG_PATH", format!("{root}/log.jsonl"))
        .env("DOIGET_CROSSREF_BASE", server.uri())
        .env("DOIGET_UNPAYWALL_BASE", format!("{}/v2", server.uri()))
        .env("DOIGET_ARXIV_BASE", arxiv.uri())
        .args(["fetch", "10.1103/bbnt-brjz"]);
    let out = tokio::task::spawn_blocking(move || cmd.output().unwrap())
        .await
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.contains("via arXiv preprint arxiv:2512.07923 (found by arxiv title search)"),
        "{text}"
    );
    assert!(td.path().join("papers/doi_10.1103_bbnt-brjz.pdf").exists());
}
