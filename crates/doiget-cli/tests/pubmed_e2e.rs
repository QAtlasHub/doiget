//! PubMed ids through the real binary (#500, ADR-0061), against a mock NCBI
//! E-utilities and Crossref. No test here reaches the real NCBI.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use assert_cmd::Command;
use tempfile::TempDir;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DOI: &str = "10.1176/ajp.155.7.895";

fn doiget(td: &TempDir, server: &MockServer) -> Command {
    let root = td.path().to_str().unwrap();
    let mut cmd = Command::cargo_bin("doiget").expect("binary");
    cmd.current_dir(td.path())
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root)
        .env("DOIGET_STORE_ROOT", format!("{root}/papers"))
        .env("DOIGET_LOG_PATH", format!("{root}/log.jsonl"))
        .env("DOIGET_CONTACT_EMAIL", "test@example.org")
        .env("DOIGET_NCBI_BASE", server.uri())
        .env("DOIGET_CROSSREF_BASE", server.uri())
        .env("DOIGET_UNPAYWALL_BASE", format!("{}/v2", server.uri()));
    cmd
}

fn esummary(uid: &str, body: serde_json::Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(serde_json::json!({
        "header": {"type": "esummary", "version": "0.3"},
        "result": {"uids": [uid], uid: body}
    }))
}

/// PMID 9659853 lists its DOI; PMID 1 lists none; PMCID 42 is not a record.
async fn ncbi_and_crossref() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/esummary.fcgi"))
        .and(query_param("db", "pubmed"))
        .and(query_param("id", "9659853"))
        .and(query_param("tool", "doiget"))
        .respond_with(esummary(
            "9659853",
            serde_json::json!({"articleids": [
                {"idtype": "pubmed", "value": "9659853"},
                {"idtype": "doi", "value": DOI}
            ]}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/esummary.fcgi"))
        .and(query_param("id", "1"))
        .respond_with(esummary(
            "1",
            serde_json::json!({"articleids": [{"idtype": "pubmed", "value": "1"}]}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/esummary.fcgi"))
        .and(query_param("db", "pmc"))
        .and(query_param("id", "42"))
        .respond_with(esummary(
            "42",
            serde_json::json!({"uid": "42", "error": "cannot get document summary"}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/works/{DOI}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "ok",
            "message": {
                "title": ["Lithium discontinuation and subsequent effectiveness"],
                "author": [{"family": "Coryell", "given": "W"}],
                "issued": {"date-parts": [[1998, 7]]},
                "container-title": ["The American journal of psychiatry"],
                "type": "journal-article", "DOI": DOI
            }
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v2/10.1176%2Fajp.155.7.895"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "doi": DOI, "is_oa": false, "oa_status": "closed",
            "best_oa_location": null, "oa_locations": []
        })))
        .mount(&server)
        .await;
    server
}

async fn run(mut cmd: Command) -> std::process::Output {
    tokio::task::spawn_blocking(move || cmd.output().expect("run"))
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pmid_is_cited_under_the_doi_pubmed_lists_for_it() {
    let server = ncbi_and_crossref().await;
    let td = TempDir::new().unwrap();
    let mut cmd = doiget(&td, &server);
    cmd.args(["cite", "pmid:9659853"]);
    let out = run(cmd).await;
    assert!(out.status.success(), "{out:?}");
    let bib = String::from_utf8(out.stdout).unwrap();
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(bib.starts_with("@article{"), "{bib}");
    assert!(bib.contains(&format!("doi        = {{{DOI}}}")), "{bib}");
    assert!(err.contains(&format!("PMID 9659853 is DOI {DOI}")), "{err}");
    // The lookup is on the provenance log, under its own source.
    let log = std::fs::read_to_string(td.path().join("log.jsonl")).unwrap();
    assert!(log.contains("\"source\":\"ncbi\""), "{log}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dry_run_makes_no_lookup_and_other_commands_name_the_ones_that_do() {
    let server = MockServer::start().await; // nothing mounted: any request 404s
    let td = TempDir::new().unwrap();
    let mut dry = doiget(&td, &server);
    dry.args(["fetch", "pmid:9659853", "--dry-run"]);
    let mut info = doiget(&td, &server);
    info.args(["info", "PMC3531190"]);
    let dry = run(dry).await;
    let info = run(info).await;
    assert_eq!(dry.status.code(), Some(2), "{dry:?}");
    assert!(
        String::from_utf8_lossy(&dry.stderr).contains("--dry-run makes no request"),
        "{dry:?}"
    );
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "a dry run must not ask NCBI"
    );
    assert_eq!(info.status.code(), Some(2), "{info:?}");
    assert!(
        String::from_utf8_lossy(&info.stderr).contains("is a PubMed id"),
        "{info:?}"
    );
}

/// Moved from `batch_jsonl_e2e.rs`: a PubMed-only `.bib` entry is not a
/// malformed one. Its record here lists no DOI, so it is NOT_IMPLEMENTED
/// with a message naming the id -- never INVALID_REF.
#[tokio::test(flavor = "multi_thread")]
async fn a_batch_pmid_without_a_doi_is_not_implemented_and_named() {
    let server = ncbi_and_crossref().await;
    let td = TempDir::new().unwrap();
    std::fs::write(
        td.path().join("refs.bib"),
        "@article{Smith2020, title = {A PubMed-only record}, pmid = {1}}\n",
    )
    .unwrap();
    let mut cmd = doiget(&td, &server);
    cmd.args(["batch", "refs.bib", "--mode", "json"]);
    let out = run(cmd).await;
    let stdout = String::from_utf8(out.stdout).unwrap();
    let line = stdout
        .lines()
        .find(|l| l.contains("\"ok\""))
        .unwrap_or_else(|| panic!("no JSONL record in: {stdout}"));
    let v: serde_json::Value = serde_json::from_str(line).unwrap();
    assert_eq!(v["ok"], false, "{v}");
    assert_eq!(v["error"]["code"], "NOT_IMPLEMENTED", "{v}");
    let message = v["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("PMID 1") && message.contains("lists no DOI"),
        "{message}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn verify_resolves_a_pmid_and_calls_an_unknown_pmcid_absent() {
    let server = ncbi_and_crossref().await;
    let td = TempDir::new().unwrap();
    std::fs::write(
        td.path().join("refs.bib"),
        "@article{coryell, title = {Lithium}, pmid = {9659853}}\n\
         @article{ghost, title = {Nothing}, pmcid = {PMC42}}\n",
    )
    .unwrap();
    let mut cmd = doiget(&td, &server);
    cmd.args(["--mode", "json", "verify", "refs.bib"]);
    let out = run(cmd).await;
    let rows: Vec<serde_json::Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let row = |k: &str| {
        rows.iter()
            .find(|r| r["entry_key"] == k)
            .unwrap_or_else(|| panic!("{k} in {rows:?}"))
            .clone()
    };
    assert_eq!(row("coryell")["status"], "valid", "{rows:?}");
    assert_eq!(row("coryell")["ref"], DOI, "{rows:?}");
    assert_eq!(row("ghost")["status"], "absent", "{rows:?}");
    assert_eq!(row("ghost")["ref"], "PMCID PMC42", "{rows:?}");
}
