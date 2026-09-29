//! #507 through the real binary: one `doiget batch` run that lists a DOI
//! twice sends it once -- the second entry is a replay of the first answer --
//! and `--refetch` sends both. With spaced spawns (`--delay`) and without:
//! two copies of a DOI in the same concurrent window run one after the
//! other, so the second is still a replay (#649 review).
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use assert_cmd::Command;
use tempfile::TempDir;
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn run(refetch: bool, spaced: bool) -> (usize, Vec<serde_json::Value>) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex("^/works/"))
        // Slow enough that an unspaced duplicate starts while the first
        // copy is still waiting for its answer.
        .respond_with(ResponseTemplate::new(404).set_delay(std::time::Duration::from_millis(300)))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let td = TempDir::new().unwrap();
    let root = td.path().to_str().unwrap().to_string();
    std::fs::write(
        td.path().join("refs.txt"),
        "10.1234/nowhere\n10.1234/nowhere\n",
    )
    .unwrap();
    let mut cmd = Command::cargo_bin("doiget").unwrap();
    cmd.current_dir(td.path())
        .env("HOME", &root)
        .env("XDG_CONFIG_HOME", &root)
        .env("DOIGET_STORE_ROOT", format!("{root}/papers"))
        .env("DOIGET_LOG_PATH", format!("{root}/log.jsonl"))
        .env("DOIGET_CONTACT_EMAIL", "test@example.org")
        .env("DOIGET_CROSSREF_BASE", server.uri())
        .env("DOIGET_UNPAYWALL_BASE", format!("{}/v2", server.uri()))
        .args(["--mode", "json", "batch", "refs.txt"]);
    if spaced {
        cmd.args(["--delay", "1"]);
    }
    if refetch {
        cmd.arg("--refetch");
    }
    let out = tokio::task::spawn_blocking(move || cmd.output().unwrap())
        .await
        .unwrap();
    let rows = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let crossref = server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| r.url.path().starts_with("/works/"))
        .count();
    (crossref, rows)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_doi_listed_twice_in_one_batch_is_asked_once() {
    let (asked, rows) = run(false, true).await;
    assert_eq!(asked, 1, "the repeat must not reach the network: {rows:?}");
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(
        rows.iter().all(|r| r["error"]["code"] == "NOT_FOUND"),
        "{rows:?}"
    );
    assert!(
        rows.iter().any(|r| r["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("--refetch")),
        "the replay says how to ask anyway: {rows:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn refetch_asks_every_time() {
    let (asked, _rows) = run(true, true).await;
    assert_eq!(asked, 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_duplicate_in_the_same_concurrent_window_is_still_asked_once() {
    let (asked, rows) = run(false, false).await;
    assert_eq!(asked, 1, "both copies went out at once: {rows:?}");
    assert_eq!(rows.len(), 2, "{rows:?}");
}
