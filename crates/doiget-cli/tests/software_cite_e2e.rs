//! `doiget cite <github URL>` and `doiget verify` on a software entry (#614),
//! through the real binary against a mock GitHub.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use assert_cmd::Command;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn doiget(td: &TempDir, server: &MockServer) -> Command {
    let root = td.path().to_str().unwrap();
    let mut cmd = Command::cargo_bin("doiget").expect("binary");
    cmd.current_dir(td.path())
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root)
        .env("DOIGET_STORE_ROOT", format!("{root}/papers"))
        .env("DOIGET_LOG_PATH", format!("{root}/log.jsonl"))
        .env("DOIGET_GITHUB_API_BASE", server.uri())
        .env("DOIGET_GITHUB_RAW_BASE", server.uri());
    cmd
}

async fn github() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/srwhite59/HFDMRG.jl"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "name": "HFDMRG.jl", "owner": {"login": "srwhite59"}
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/srwhite59/HFDMRG.jl/releases/tags/v0.1.0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "tag_name": "v0.1.0", "published_at": "2023-05-02T10:00:00Z",
            "html_url": "https://github.com/srwhite59/HFDMRG.jl/releases/tag/v0.1.0"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/srwhite59/HFDMRG.jl/v0.1.0/CITATION.cff"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "title: HFDMRG.jl\ndoi: 10.5281/zenodo.100\nauthors:\n  - family-names: White\n    given-names: Steven R.\n",
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/someone/deleted"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/someone/private"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;
    server
}

#[tokio::test(flavor = "multi_thread")]
async fn a_github_release_is_cited_as_software_with_its_cff_authors() {
    let server = github().await;
    let td = TempDir::new().unwrap();
    let mut cmd = doiget(&td, &server);
    cmd.args([
        "cite",
        "https://github.com/srwhite59/HFDMRG.jl/releases/tag/v0.1.0",
    ]);
    let out = tokio::task::spawn_blocking(move || cmd.assert().success().get_output().clone())
        .await
        .unwrap();
    let bib = String::from_utf8(out.stdout).unwrap();
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(bib.starts_with("@software{HFDMRG_jl_v0_1_0,"), "{bib}");
    assert!(bib.contains("author     = {Steven R. White}"), "{bib}");
    assert!(bib.contains("year       = {2023}"), "{bib}");
    assert!(bib.contains("version    = {v0.1.0}"), "{bib}");
    assert!(bib.contains("doi        = {10.5281/zenodo.100}"), "{bib}");
    assert!(
        err.contains("CITATION.cff names DOI 10.5281/zenodo.100"),
        "{err}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn verify_checks_a_software_entry_resolves_on_github() {
    let server = github().await;
    let td = TempDir::new().unwrap();
    std::fs::write(
        td.path().join("refs.bib"),
        "@software{hf, title={HFDMRG}, url={https://github.com/srwhite59/HFDMRG.jl/releases/tag/v0.1.0}}\n\
         @software{gone, title={Gone}, url={https://github.com/someone/deleted}}\n\
         @software{private, title={Private}, url={https://github.com/someone/private}}\n",
    )
    .unwrap();
    let mut cmd = doiget(&td, &server);
    cmd.args(["--mode", "json", "verify", "refs.bib"]);
    let out = tokio::task::spawn_blocking(move || cmd.assert().get_output().clone())
        .await
        .unwrap();
    let rows: Vec<serde_json::Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .filter(|v: &serde_json::Value| v.get("entry_key").is_some())
        .collect();
    let status = |k: &str| {
        rows.iter()
            .find(|r| r["entry_key"] == k)
            .map(|r| r["status"].as_str().unwrap().to_string())
    };
    assert_eq!(status("hf").as_deref(), Some("valid"), "{rows:?}");
    assert_eq!(status("gone").as_deref(), Some("absent"), "{rows:?}");
    // A 403: not retryable, not absent -- and both causes are named.
    assert_eq!(
        status("private").as_deref(),
        Some("unreachable"),
        "{rows:?}"
    );
    let private = rows.iter().find(|r| r["entry_key"] == "private").unwrap();
    assert_eq!(private["error"]["code"], "CAPABILITY_DENIED", "{private}");
    assert!(
        private["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not public"),
        "{private}"
    );
    assert!(!out.status.success(), "an absent entry fails verify");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_key_applies_to_a_software_citation_and_offline_is_refused() {
    let server = github().await;
    let td = TempDir::new().unwrap();
    let url = "https://github.com/srwhite59/HFDMRG.jl/releases/tag/v0.1.0";
    let mut keyed = doiget(&td, &server);
    keyed.args(["cite", url, "--key", "white2023hfdmrg"]);
    let mut offline = doiget(&td, &server);
    offline.args(["cite", url, "--offline"]);
    let (keyed, offline) = tokio::task::spawn_blocking(move || {
        (
            keyed.assert().success().get_output().clone(),
            offline.assert().failure().get_output().clone(),
        )
    })
    .await
    .unwrap();
    let bib = String::from_utf8(keyed.stdout).unwrap();
    assert!(bib.starts_with("@software{white2023hfdmrg,"), "{bib}");
    let err = String::from_utf8(offline.stderr).unwrap();
    assert!(err.contains("no store entry to render"), "{err}");
}

/// batch and missing name a software entry as software, with the command
/// that cites it -- not as an entry missing an identifier.
#[tokio::test(flavor = "multi_thread")]
async fn batch_and_missing_name_a_software_entry() {
    let server = github().await;
    let td = TempDir::new().unwrap();
    std::fs::write(
        td.path().join("refs.bib"),
        "@software{hf, title={HFDMRG}, url={https://github.com/srwhite59/HFDMRG.jl}}\n",
    )
    .unwrap();
    let mut batch = doiget(&td, &server);
    batch.args(["--mode", "json", "batch", "refs.bib"]);
    let mut missing = doiget(&td, &server);
    missing.args(["--mode", "json", "missing", "refs.bib", "--offline"]);
    let (batch, missing) = tokio::task::spawn_blocking(move || {
        (
            batch.assert().get_output().clone(),
            missing.assert().get_output().clone(),
        )
    })
    .await
    .unwrap();
    let batch_out = String::from_utf8(batch.stdout).unwrap();
    assert!(batch_out.contains("NOT_IMPLEMENTED"), "{batch_out}");
    assert!(batch_out.contains("doiget cite"), "{batch_out}");
    let missing_out = String::from_utf8(missing.stdout).unwrap();
    assert!(
        missing_out.contains("software at https://github.com/srwhite59/HFDMRG.jl"),
        "{missing_out}"
    );
    assert!(
        missing_out.contains("\"entry_key\":\"hf\""),
        "{missing_out}"
    );
}
