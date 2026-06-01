// allow: outbound-network
//! End-to-end tests for `doiget version [--check]`.
//!
//! The `--check` tests spin up a wiremock server and pass
//! `DOIGET_GITHUB_BASE=<mock-uri>` to the subprocess so no real GitHub
//! network calls are made.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use assert_cmd::Command;
use predicates::str::contains;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn doiget(dir: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("doiget").expect("locate doiget binary");
    let p = dir.path().to_str().expect("tempdir path utf-8");
    cmd.env("HOME", p)
        .env("USERPROFILE", p)
        // Non-TTY implicit-quiet override — without this the ADR-0017
        // resolver falls back to Quiet in captured-stdout contexts.
        .env("DOIGET_MODE", "human");
    cmd
}

fn releases_body(tag: &str, draft: bool, prerelease: bool) -> serde_json::Value {
    serde_json::json!([{
        "tag_name":  tag,
        "html_url":  format!("https://github.com/sotashimozono/doiget/releases/tag/{tag}"),
        "draft":     draft,
        "prerelease": prerelease,
    }])
}

// ---------------------------------------------------------------------------
// doiget version (no --check)
// ---------------------------------------------------------------------------

#[test]
fn version_no_flag_prints_human_line() {
    let dir = TempDir::new().expect("tempdir");
    doiget(&dir)
        .arg("version")
        .assert()
        .success()
        .stdout(contains("doiget "));
}

#[test]
fn version_json_mode_emits_version_key() {
    let dir = TempDir::new().expect("tempdir");
    let out = doiget(&dir)
        .args(["version", "--mode", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).expect("stdout is valid JSON");
    assert!(
        v.get("version").and_then(|v| v.as_str()).is_some(),
        "json output must have a `version` string field; got: {v}"
    );
}

#[test]
fn version_quiet_mode_produces_no_stdout() {
    let dir = TempDir::new().expect("tempdir");
    doiget(&dir)
        .args(["version", "--quiet"])
        .assert()
        .success()
        .stdout("");
}

// ---------------------------------------------------------------------------
// doiget version --check  (wiremock-driven)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn version_check_newer_available_human_output() {
    let server = MockServer::start().await;
    // v1.0.0 is numerically greater than any 0.x.y build.
    Mock::given(method("GET"))
        .and(path("/repos/sotashimozono/doiget/releases"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(releases_body("v1.0.0", false, false)),
        )
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("tempdir");
    let out = doiget(&dir)
        .args(["version", "--check"])
        .env("DOIGET_GITHUB_BASE", &server.uri())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let s = String::from_utf8(out).unwrap();
    assert!(
        s.contains("update available") || s.contains("1.0.0"),
        "expected update-available message; got: {s:?}"
    );
}

#[tokio::test]
async fn version_check_up_to_date_human_output() {
    let server = MockServer::start().await;
    // v0.1.0 is older than any current build (>= 0.4.x).
    Mock::given(method("GET"))
        .and(path("/repos/sotashimozono/doiget/releases"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(releases_body("v0.1.0", false, false)),
        )
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("tempdir");
    let out = doiget(&dir)
        .args(["version", "--check"])
        .env("DOIGET_GITHUB_BASE", &server.uri())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let s = String::from_utf8(out).unwrap();
    assert!(
        s.contains("up to date") || s.contains("0.1.0"),
        "expected up-to-date message; got: {s:?}"
    );
}

#[tokio::test]
async fn version_check_json_mode_emits_structured_object() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/sotashimozono/doiget/releases"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(releases_body("v1.0.0", false, false)),
        )
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("tempdir");
    let out = doiget(&dir)
        .args(["version", "--check", "--mode", "json"])
        .env("DOIGET_GITHUB_BASE", &server.uri())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value =
        serde_json::from_slice(&out).expect("--check --mode json stdout is valid JSON");
    assert!(v.get("current").is_some(), "must have `current`; got: {v}");
    assert!(v.get("latest").is_some(), "must have `latest`; got: {v}");
    assert!(
        v.get("newer_available").is_some(),
        "must have `newer_available`; got: {v}"
    );
    assert!(
        v.get("html_url").is_some(),
        "must have `html_url`; got: {v}"
    );
    assert_eq!(
        v["newer_available"],
        serde_json::json!(true),
        "v1.0.0 must be newer than current build; got: {v}"
    );
}

#[tokio::test]
async fn version_check_skips_draft_releases() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/sotashimozono/doiget/releases"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(releases_body("v9.0.0", true, false)),
        )
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("tempdir");
    let out = doiget(&dir)
        .args(["version", "--check", "--mode", "json"])
        .env("DOIGET_GITHUB_BASE", &server.uri())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert!(
        v["latest"].is_null(),
        "draft releases must not surface as latest; got: {v}"
    );
    assert!(v.get("error").is_some(), "must report an error; got: {v}");
}

#[tokio::test]
async fn version_check_skips_prerelease_flag_releases() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/sotashimozono/doiget/releases"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(releases_body("v9.0.0", false, true)),
        )
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("tempdir");
    let out = doiget(&dir)
        .args(["version", "--check", "--mode", "json"])
        .env("DOIGET_GITHUB_BASE", &server.uri())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert!(
        v["latest"].is_null(),
        "prerelease=true releases must not surface; got: {v}"
    );
}

#[tokio::test]
async fn version_check_skips_tags_with_beta_suffix() {
    let server = MockServer::start().await;
    // draft=false, prerelease=false, but tag has -beta — must be filtered.
    Mock::given(method("GET"))
        .and(path("/repos/sotashimozono/doiget/releases"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                "tag_name":   "v9.0.0-beta.1",
                "html_url":   "https://github.com/sotashimozono/doiget/releases/tag/v9.0.0-beta.1",
                "draft":      false,
                "prerelease": false,
            }])),
        )
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("tempdir");
    let out = doiget(&dir)
        .args(["version", "--check", "--mode", "json"])
        .env("DOIGET_GITHUB_BASE", &server.uri())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert!(
        v["latest"].is_null(),
        "-beta tag must be filtered even when prerelease=false; got: {v}"
    );
}

#[tokio::test]
async fn version_check_picks_first_stable_skipping_unstable() {
    let server = MockServer::start().await;
    // First entry is a draft; second is the stable release to pick.
    Mock::given(method("GET"))
        .and(path("/repos/sotashimozono/doiget/releases"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "tag_name":   "v10.0.0",
                "html_url":   "https://github.com/example/releases/tag/v10.0.0",
                "draft":      true,
                "prerelease": false,
            },
            {
                "tag_name":   "v0.9.0",
                "html_url":   "https://github.com/sotashimozono/doiget/releases/tag/v0.9.0",
                "draft":      false,
                "prerelease": false,
            }
        ])))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("tempdir");
    let out = doiget(&dir)
        .args(["version", "--check", "--mode", "json"])
        .env("DOIGET_GITHUB_BASE", &server.uri())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(
        v["latest"].as_str(),
        Some("0.9.0"),
        "must pick v0.9.0, skipping draft v10.0.0; got: {v}"
    );
}

#[tokio::test]
async fn version_check_rate_limited_json_reports_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/sotashimozono/doiget/releases"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("tempdir");
    let out = doiget(&dir)
        .args(["version", "--check", "--mode", "json"])
        .env("DOIGET_GITHUB_BASE", &server.uri())
        .assert()
        .success() // must never hard-fail on a network error
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert!(
        v["latest"].is_null(),
        "rate-limited must return null latest; got: {v}"
    );
    assert_eq!(
        v["error"].as_str(),
        Some("rate_limited"),
        "must report rate_limited; got: {v}"
    );
}

#[tokio::test]
async fn version_check_rate_limited_human_exits_zero() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/sotashimozono/doiget/releases"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("tempdir");
    doiget(&dir)
        .args(["version", "--check"])
        .env("DOIGET_GITHUB_BASE", &server.uri())
        .assert()
        .success()
        .stdout(contains("version check failed"));
}

#[tokio::test]
async fn version_check_empty_releases_reports_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/sotashimozono/doiget/releases"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("tempdir");
    let out = doiget(&dir)
        .args(["version", "--check", "--mode", "json"])
        .env("DOIGET_GITHUB_BASE", &server.uri())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert!(
        v.get("error").is_some(),
        "empty releases must report error; got: {v}"
    );
    assert!(v["latest"].is_null(), "latest must be null; got: {v}");
}
