// allow: outbound-network
//! End-to-end tests for `doiget verify <path>`.
//!
//! The network-touching cases (`valid` / `unresolved`) point the arXiv
//! source at a wiremock origin via `DOIGET_ARXIV_BASE`, so no outbound
//! call is made. The classification cases (`illegal` / `unverifiable`)
//! need no network at all — a malformed id is rejected by `Ref::parse`
//! and an id-less entry never reaches a resolver.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Reusable synthetic Atom payload (mirrors the core arXiv e2e fixture).
const SAMPLE_ATOM_FEED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <entry>
    <id>http://arxiv.org/abs/2401.12345v1</id>
    <title>Example arXiv Paper Title</title>
    <summary>This is an example abstract.</summary>
    <author><name>Jane Doe</name></author>
    <published>2024-01-15T00:00:00Z</published>
  </entry>
</feed>"#;

/// Build a `doiget verify` command with a temp HOME / store / log so the
/// developer's real config and store are never touched.
fn doiget(dir: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("doiget").expect("locate doiget binary");
    let p = dir.path().to_str().expect("tempdir path utf-8");
    let log_path = format!("{p}/log.jsonl");
    cmd.env("HOME", p)
        .env("USERPROFILE", p)
        // Pin the config dir to the temp dir on both POSIX and Windows so
        // a written `<temp>/doiget/config.toml` is the one verify reads.
        .env("XDG_CONFIG_HOME", p)
        .env("APPDATA", p)
        .env("DOIGET_STORE_ROOT", p)
        .env("DOIGET_LOG_PATH", log_path)
        // Pin the resolver cache to the temp dir so a test never reads or
        // writes the developer's real ~/.cache/doiget.
        .env("DOIGET_CACHE_ROOT", p);
    cmd
}

/// Write `content` to `<dir>/<name>` and return the path string.
fn write_bib(dir: &TempDir, name: &str, content: &str) -> String {
    let path = dir.path().join(name);
    std::fs::write(&path, content).expect("write bib fixture");
    path.to_str().expect("utf-8 path").to_string()
}

/// Write a `<dir>/doiget/config.toml` so the temp HOME resolves a
/// `[verify]` section. `doiget()` sets `XDG_CONFIG_HOME` to the temp dir,
/// so the config dir is `<dir>/doiget/`.
fn write_config(dir: &TempDir, body: &str) {
    let cfg_dir = dir.path().join("doiget");
    std::fs::create_dir_all(&cfg_dir).expect("mkdir config dir");
    std::fs::write(cfg_dir.join("config.toml"), body).expect("write config.toml");
}

// ---------------------------------------------------------------------------
// Classification cases that need no network
// ---------------------------------------------------------------------------

#[test]
fn verify_typo_doi_is_illegal_and_fails() {
    // `1O.1234` uses letter O — `Ref::parse` rejects it, so it is a
    // definite source error regardless of the network.
    let dir = TempDir::new().expect("tempdir");
    let bib = write_bib(&dir, "refs.bib", "@article{x, doi = {1O.1234/typo}}");
    doiget(&dir)
        .args(["verify", &bib])
        .assert()
        .failure()
        .stdout(contains("\"status\":\"illegal\""));
}

#[test]
fn verify_missing_id_warns_by_default() {
    // An entry with no DOI / arXiv id is `unverifiable`; the default run
    // reports it but exits 0.
    let dir = TempDir::new().expect("tempdir");
    let bib = write_bib(&dir, "refs.bib", "@book{x, title = {A Book With No Id}}");
    doiget(&dir)
        .args(["verify", &bib])
        .assert()
        .success()
        .stdout(contains("\"status\":\"unverifiable\""));
}

#[test]
fn verify_missing_id_strict_fails() {
    let dir = TempDir::new().expect("tempdir");
    let bib = write_bib(&dir, "refs.bib", "@book{x, title = {A Book With No Id}}");
    doiget(&dir)
        .args(["verify", &bib, "--strict"])
        .assert()
        .failure()
        .stdout(contains("\"status\":\"unverifiable\""));
}

#[test]
fn verify_config_on_missing_id_error_fails_without_strict() {
    // `[verify] on_missing_id = "error"` makes an id-less entry fail the
    // run even without the `--strict` CLI flag.
    let dir = TempDir::new().expect("tempdir");
    write_config(&dir, "[verify]\non_missing_id = \"error\"\n");
    let bib = write_bib(&dir, "refs.bib", "@book{x, title = {No Id}}");
    doiget(&dir)
        .args(["verify", &bib])
        .assert()
        .failure()
        .stdout(contains("\"status\":\"unverifiable\""));
}

#[test]
fn verify_config_on_missing_id_skip_drops_entry() {
    // `skip` drops the id-less entry: no record emitted, run exits 0.
    let dir = TempDir::new().expect("tempdir");
    write_config(&dir, "[verify]\non_missing_id = \"skip\"\n");
    let bib = write_bib(&dir, "refs.bib", "@book{x, title = {No Id}}");
    doiget(&dir)
        .args(["verify", &bib])
        .assert()
        .success()
        .stdout(contains("unverifiable").not());
}

#[test]
fn verify_config_strict_does_not_let_skip_drop_idless_entries() {
    // `[verify] strict = true` + `on_missing_id = "skip"`: a strict run
    // must not silently drop id-less entries — they surface (as a warning)
    // so the summary stays honest. The run still exits 0 (skip→warn, not
    // error), but the record IS emitted.
    let dir = TempDir::new().expect("tempdir");
    write_config(&dir, "[verify]\nstrict = true\non_missing_id = \"skip\"\n");
    let bib = write_bib(&dir, "refs.bib", "@book{x, title = {No Id}}");
    doiget(&dir)
        .args(["verify", &bib])
        .assert()
        .success()
        .stdout(contains("\"status\":\"unverifiable\""));
}

#[test]
fn verify_unknown_format_flag_errors() {
    let dir = TempDir::new().expect("tempdir");
    let bib = write_bib(&dir, "refs.bib", "@article{x, doi = {10.1234/foo}}");
    doiget(&dir)
        .args(["verify", &bib, "--format", "ris"])
        .assert()
        .failure();
}

// ---------------------------------------------------------------------------
// Network-touching cases (wiremock arXiv)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn verify_valid_arxiv_entry_passes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/query"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_ATOM_FEED))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("tempdir");
    let bib = write_bib(
        &dir,
        "refs.bib",
        "@article{x, eprint = {2401.12345}, archivePrefix = {arXiv}}",
    );
    doiget(&dir)
        .args(["verify", &bib])
        .env("DOIGET_ARXIV_BASE", server.uri())
        .assert()
        .success()
        .stdout(contains("\"status\":\"valid\""));
}

#[tokio::test]
async fn verify_absent_arxiv_404_fails_by_default_and_under_strict() {
    // An HTTP 404 from the metadata source is an authoritative "this id
    // does not exist" → status "absent" + error code NOT_FOUND. Being
    // network-independent, it fails the run with exit code 1 (one failing
    // entry) WITHOUT --strict, and equally under --strict.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/query"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let content = "@article{x, eprint = {2401.99999}, archivePrefix = {arXiv}}";

    let dir = TempDir::new().expect("tempdir");
    let bib = write_bib(&dir, "refs.bib", content);
    doiget(&dir)
        .args(["verify", &bib])
        .env("DOIGET_ARXIV_BASE", server.uri())
        .assert()
        .code(1)
        .stdout(contains("\"status\":\"absent\"").and(contains("\"code\":\"NOT_FOUND\"")));

    // --strict must NOT downgrade or change the verdict: absent always fails.
    let dir2 = TempDir::new().expect("tempdir");
    let bib2 = write_bib(&dir2, "refs.bib", content);
    doiget(&dir2)
        .args(["verify", &bib2, "--strict"])
        .env("DOIGET_ARXIV_BASE", server.uri())
        .assert()
        .code(1)
        .stdout(contains("\"status\":\"absent\""));
}

#[tokio::test]
async fn verify_absent_arxiv_empty_feed_fails_by_default() {
    // The REALISTIC arXiv "unknown id" case: HTTP 200 with an empty
    // `<feed>` (no `<entry>`), not a 404. This must still classify as
    // `absent` (NOT_FOUND) and fail the default run — otherwise a dead
    // arXiv reference would silently pass.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/query"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom"></feed>"#,
        ))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("tempdir");
    let bib = write_bib(
        &dir,
        "refs.bib",
        "@article{x, eprint = {2401.99999}, archivePrefix = {arXiv}}",
    );
    doiget(&dir)
        .args(["verify", &bib])
        .env("DOIGET_ARXIV_BASE", server.uri())
        .assert()
        .code(1)
        .stdout(contains("\"status\":\"absent\"").and(contains("\"code\":\"NOT_FOUND\"")));
}

#[tokio::test]
async fn verify_unreachable_arxiv_5xx_warns_by_default_fails_strict() {
    // A transient upstream failure (503) is "unreachable": it does NOT
    // prove the id is dead, so it is tolerated by default (a flaky
    // network must not fail a build over a real id) and fails only under
    // --strict (the network-stable lane).
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/query"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;

    let bib_content = "@article{x, eprint = {2401.99999}, archivePrefix = {arXiv}}";

    // Default: unreachable is a warning → exit 0.
    let dir1 = TempDir::new().expect("tempdir");
    let bib1 = write_bib(&dir1, "refs.bib", bib_content);
    doiget(&dir1)
        .args(["verify", &bib1])
        .env("DOIGET_ARXIV_BASE", server.uri())
        .assert()
        .success()
        .stdout(contains("\"status\":\"unreachable\"").and(contains("\"code\":\"NETWORK_ERROR\"")));

    // Strict: unreachable fails the run.
    let dir2 = TempDir::new().expect("tempdir");
    let bib2 = write_bib(&dir2, "refs.bib", bib_content);
    doiget(&dir2)
        .args(["verify", &bib2, "--strict"])
        .env("DOIGET_ARXIV_BASE", server.uri())
        .assert()
        .failure()
        .stdout(contains("\"status\":\"unreachable\""));
}

#[tokio::test]
async fn verify_with_base_override_does_not_write_cache() {
    // Regression guard: when a DOIGET_*_BASE override is set (wiremock),
    // the resolver cache MUST be bypassed so a wiremock-fabricated entry
    // never poisons the real production cache for the same ref.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/query"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_ATOM_FEED))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("tempdir");
    let bib = write_bib(
        &dir,
        "refs.bib",
        "@article{x, eprint = {2401.12345}, archivePrefix = {arXiv}}",
    );
    // doiget() sets DOIGET_CACHE_ROOT to the temp dir; the cache, if it
    // were written, would land in <temp>/resolver/.
    doiget(&dir)
        .args(["verify", &bib])
        .env("DOIGET_ARXIV_BASE", server.uri())
        .assert()
        .success()
        .stdout(contains("\"status\":\"valid\""));

    let resolver_dir = dir.path().join("resolver");
    let entries = std::fs::read_dir(&resolver_dir)
        .map(|rd| rd.count())
        .unwrap_or(0);
    assert_eq!(
        entries, 0,
        "base override must bypass the cache; found {entries} cache entries in {resolver_dir:?}"
    );
}

#[tokio::test]
async fn verify_mixed_illegal_and_absent_emits_both_records_and_fails() {
    // A two-entry file exercises the per-entry loop and counter
    // accumulation: a malformed id (illegal, no network) plus a 404'd id
    // (absent). Both must be emitted and the run must fail (exit 2).
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/query"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("tempdir");
    let bib = write_bib(
        &dir,
        "refs.bib",
        "@article{bad, doi = {1O.1234/typo}}\n\
         @article{gone, eprint = {2401.99999}, archivePrefix = {arXiv}}",
    );
    doiget(&dir)
        .args(["verify", &bib])
        .env("DOIGET_ARXIV_BASE", server.uri())
        .assert()
        .code(2)
        .stdout(contains("\"status\":\"illegal\"").and(contains("\"status\":\"absent\"")));
}
