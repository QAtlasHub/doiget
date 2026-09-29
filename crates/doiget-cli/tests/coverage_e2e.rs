//! `doiget coverage` / `doiget sources` (#605) through the real binary.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn doiget(td: &TempDir) -> Command {
    let root = td.path().to_str().unwrap();
    let mut cmd = Command::cargo_bin("doiget").expect("binary");
    cmd.current_dir(td.path())
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root)
        .env("DOIGET_STORE_ROOT", format!("{root}/papers"))
        .env("DOIGET_LOG_PATH", format!("{root}/log.jsonl"))
        .env("DOIGET_CONTACT_EMAIL", "test@example.org");
    cmd
}

fn json(cmd: &mut Command) -> serde_json::Value {
    let out = cmd.assert().success().get_output().stdout.clone();
    serde_json::from_slice(&out).expect("json report")
}

#[test]
fn sources_names_every_source_and_scopes_by_publisher_offline() {
    let td = TempDir::new().unwrap();
    let all = json(doiget(&td).args(["--mode", "json", "sources"]));
    let names: Vec<&str> = all["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["source"].as_str().unwrap())
        .collect();
    for n in [
        "crossref",
        "unpaywall",
        "arxiv",
        "europe-pmc",
        "tdm-aps",
        "tdm-ieee",
    ] {
        assert!(names.contains(&n), "{n} missing from {names:?}");
    }
    let aps = json(doiget(&td).args(["--mode", "json", "sources", "--publisher", "10.1103"]));
    let tdm: Vec<&str> = aps["sources"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["tier"] == 3)
        .map(|s| s["source"].as_str().unwrap())
        .collect();
    assert_eq!(tdm, vec!["tdm-aps"]);
    let springer = json(doiget(&td).args(["--mode", "json", "sources", "--publisher", "springer"]));
    let tdm: Vec<&str> = springer["sources"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["tier"] == 3)
        .map(|s| s["source"].as_str().unwrap())
        .collect();
    assert_eq!(tdm, vec!["tdm-springer"], "matched by publisher name");
    // A default build has no Tier-3 code (ADR-0002) and says how to get it.
    let row = &aps["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["source"] == "tdm-aps")
        .unwrap();
    if row["status"] == "not_built" {
        assert_eq!(row["enable"], "build with --features tdm-aps");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn coverage_with_no_open_copy_gives_the_landing_page_and_what_was_not_asked() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/works/10.1103/PhysRevB.48.10345"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "ok",
            "message": {
                "title": ["Density-matrix algorithms for quantum renormalization groups"],
                "publisher": "American Physical Society (APS)",
                "container-title": ["Physical Review B"],
                "type": "journal-article",
                "resource": {"primary": {"URL": "http://link.aps.org/doi/10.1103/PhysRevB.48.10345"}}
            }
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v2/10.1103%2FPhysRevB.48.10345"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "doi": "10.1103/PhysRevB.48.10345", "is_oa": false, "oa_status": "closed",
            "best_oa_location": null, "oa_locations": []
        })))
        .mount(&server)
        .await;
    let td = TempDir::new().unwrap();
    let mut cmd = doiget(&td);
    cmd.env("DOIGET_CROSSREF_BASE", server.uri())
        .env("DOIGET_UNPAYWALL_BASE", format!("{}/v2", server.uri()))
        .args(["--mode", "json", "coverage", "10.1103/PhysRevB.48.10345"]);
    let r = tokio::task::spawn_blocking(move || json(&mut cmd))
        .await
        .unwrap();
    assert_eq!(r["publisher"], "American Physical Society (APS)");
    assert_eq!(r["journal"], "Physical Review B");
    assert_eq!(r["oa_status"], "closed");
    assert!(r["oa_url"].is_null());
    assert_eq!(
        r["landing_url"],
        "http://link.aps.org/doi/10.1103/PhysRevB.48.10345"
    );
    let not_asked: Vec<&str> = r["verdict"]["not_asked"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["source"].as_str().unwrap())
        .collect();
    assert!(not_asked.contains(&"tdm-aps"), "{not_asked:?}");
    assert!(!not_asked.contains(&"tdm-springer"), "{not_asked:?}");
}

#[test]
fn sources_prints_a_table_by_default_on_a_non_tty() {
    let td = TempDir::new().unwrap();
    doiget(&td)
        .args(["sources"])
        .assert()
        .success()
        .stdout(predicates::str::contains("crossref"))
        .stdout(predicates::str::contains("tdm-aps"))
        .stdout(predicates::str::contains("{").not());
}

#[tokio::test(flavor = "multi_thread")]
async fn coverage_of_a_doi_crossref_does_not_know_fails_with_its_code() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let td = TempDir::new().unwrap();
    let mut cmd = doiget(&td);
    cmd.env("DOIGET_CROSSREF_BASE", server.uri())
        .env("DOIGET_UNPAYWALL_BASE", format!("{}/v2", server.uri()))
        .args(["coverage", "10.1234/nowhere"]);
    let out = tokio::task::spawn_blocking(move || cmd.output().unwrap())
        .await
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("error[NOT_FOUND]"), "{err}");
    assert!(
        out.stdout.is_empty(),
        "no report for a failed resolve: {out:?}"
    );
}
