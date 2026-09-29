//! `doiget cite` of a Zenodo version DOI (#614) through the real binary,
//! against mocked Crossref and DataCite: the concept DOI by default, the
//! version DOI with `--zenodo-version` (#649 review).
#![cfg(feature = "metadata")]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use assert_cmd::Command;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const VERSION: &str = "10.5281/zenodo.22053902";
const CONCEPT: &str = "10.5281/zenodo.22053901";

async fn registries() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/works/{VERSION}")))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/dois/{VERSION}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": {
                "id": VERSION,
                "type": "dois",
                "attributes": {
                    "doi": VERSION,
                    "titles": [{"title": "An Example Deposit"}],
                    "creators": [{"name": "Researcher, Alice"}],
                    "publicationYear": 2024,
                    "publisher": "Zenodo",
                    "version": "v1.2.0",
                    "types": {"resourceTypeGeneral": "Software"},
                    "relatedIdentifiers": [{
                        "relationType": "IsVersionOf",
                        "relatedIdentifierType": "DOI",
                        "relatedIdentifier": CONCEPT
                    }]
                }
            }
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    server
}

async fn cite(zenodo_version: bool) -> (String, String) {
    let server = registries().await;
    let (out, _) = run(&server, zenodo_version, true).await;
    assert!(out.status.success(), "{out:?}");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

async fn run(
    server: &MockServer,
    zenodo_version: bool,
    datacite: bool,
) -> (std::process::Output, TempDir) {
    let td = TempDir::new().unwrap();
    let root = td.path().to_str().unwrap().to_string();
    let mut cmd = Command::cargo_bin("doiget").expect("binary");
    cmd.current_dir(td.path())
        .env("HOME", &root)
        .env("XDG_CONFIG_HOME", &root)
        .env("DOIGET_STORE_ROOT", format!("{root}/papers"))
        .env("DOIGET_LOG_PATH", format!("{root}/log.jsonl"))
        .env("DOIGET_CONTACT_EMAIL", "test@example.org")
        .env("DOIGET_CROSSREF_BASE", server.uri())
        .env("DOIGET_UNPAYWALL_BASE", format!("{}/v2", server.uri()))
        .env("DOIGET_DATACITE_BASE", server.uri())
        .args(["cite", VERSION]);
    if datacite {
        cmd.env("DOIGET_ENABLE_DATACITE", "1");
    }
    if zenodo_version {
        cmd.arg("--zenodo-version");
    }
    let out = tokio::task::spawn_blocking(move || cmd.output().unwrap())
        .await
        .unwrap();
    (out, td)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_zenodo_version_doi_is_cited_as_its_concept_by_default() {
    let (bib, err) = cite(false).await;
    assert!(bib.contains(CONCEPT), "{bib}");
    assert!(!bib.contains(VERSION), "{bib}");
    assert!(err.contains("--zenodo-version"), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn zenodo_version_keeps_the_version_doi() {
    let (bib, err) = cite(true).await;
    assert!(bib.contains(VERSION), "{bib}");
    assert!(err.contains(CONCEPT), "the concept is still named: {err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_disabled_datacite_is_not_asked() {
    let server = registries().await;
    let (out, _td) = run(&server, false, false).await;
    assert!(!out.status.success(), "{out:?}");
    let asked = server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .any(|r| r.url.path().starts_with("/dois/"));
    assert!(!asked, "DataCite is opt-in (DOIGET_ENABLE_DATACITE)");
}
