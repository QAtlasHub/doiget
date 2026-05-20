//! End-to-end tests for `batch --mode json` (#205, ERRORS.md §3 CI persona).
//!
//! Validates the per-ref JSON-Lines wire shape: one record per input
//! line, success `{"ok":true,"ref":"..."}` or failure
//! `{"ok":false,"ref":"...","error":{"code":"...","message":"..."}}`.
//! The exit code is the failure count (capped at 255, ERRORS.md §4).
//!
//! Only the parse-failure path is exercised here — it requires no
//! network mocking and exercises both the JSONL emit and the exit
//! code. A network-mocked success path lands together with the
//! `fetch_one` outcome-plumbing follow-up.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::io::Write;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

fn doiget(dir: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("doiget").expect("locate doiget binary");
    let p = dir.path().to_str().expect("tempdir path is UTF-8");
    cmd.env("HOME", p)
        .env("USERPROFILE", p)
        .env("APPDATA", p)
        .env("XDG_CONFIG_HOME", p)
        .env("DOIGET_LOG_PATH", dir.path().join("access.jsonl"))
        .env("DOIGET_STORE_ROOT", dir.path().join("store"))
        .env("DOIGET_CONTACT_EMAIL", "test@example.com");
    cmd
}

#[test]
fn batch_json_parse_failure_emits_invalid_ref_jsonl() {
    let dir = TempDir::new().expect("tempdir");
    let refs = dir.path().join("refs.txt");
    {
        let mut f = std::fs::File::create(&refs).expect("create refs file");
        // One malformed line — must NOT parse as a DOI / arXiv id. We
        // also include a comment + blank to confirm those are skipped
        // (they should not produce JSONL records).
        f.write_all(b"# comment\nnot-a-doi\n\n")
            .expect("write refs");
    }

    let output = doiget(&dir)
        .args(["--json", "batch", refs.to_str().unwrap()])
        .assert()
        .failure() // parse_errors > 0 → CliExit(1)
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).expect("stdout utf-8");

    // Filter to non-empty lines so a stray trailing newline doesn't
    // break the count assertion.
    let lines: Vec<&str> = stdout.lines().filter(|s| !s.trim().is_empty()).collect();
    assert_eq!(lines.len(), 1, "exactly one JSONL record, got: {stdout}");

    let v: Value = serde_json::from_str(lines[0]).expect("line parses as JSON");
    assert_eq!(v["ok"], Value::Bool(false));
    assert_eq!(v["ref"], "not-a-doi");
    assert_eq!(
        v["error"]["code"], "INVALID_REF",
        "ERRORS.md §3 INVALID_REF on parse failure"
    );
    assert!(
        v["error"]["message"].is_string() && !v["error"]["message"].as_str().unwrap().is_empty(),
        "error.message MUST be a non-empty string"
    );
}

#[test]
fn batch_json_fetch_failure_emits_fetch_error_jsonl() {
    // Point the arxiv resolver at a closed loopback port so a parseable
    // ref deterministically fails at the transport layer. This exercises
    // the JoinSet drain's `Err(e)` branch and `emit_jsonl_failure` with
    // FETCH_ERROR — the previously-uncovered emit path.
    let dir = TempDir::new().expect("tempdir");
    let refs = dir.path().join("refs.txt");
    std::fs::File::create(&refs)
        .expect("create refs file")
        .write_all(b"arxiv:2401.99999\n")
        .expect("write refs");

    let output = doiget(&dir)
        // Closed port → connect-refused → fetch_one returns Err →
        // FETCH_ERROR JSONL.
        .env("DOIGET_ARXIV_BASE", "http://127.0.0.1:1/")
        .args(["--json", "batch", refs.to_str().unwrap()])
        .assert()
        .failure()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).expect("stdout utf-8");
    let lines: Vec<&str> = stdout.lines().filter(|s| !s.trim().is_empty()).collect();
    assert_eq!(lines.len(), 1, "exactly one JSONL record, got: {stdout}");
    let v: Value = serde_json::from_str(lines[0]).expect("line parses as JSON");
    assert_eq!(v["ok"], Value::Bool(false));
    assert_eq!(v["ref"], "arxiv:2401.99999");
    // Code is FETCH_ERROR (generic) per the #205 contract — the
    // structured `denial_context` / source-specific code lands with the
    // FetchPaperOutcome plumbing follow-up.
    assert_eq!(v["error"]["code"], "FETCH_ERROR");
    assert!(
        v["error"]["message"].is_string() && !v["error"]["message"].as_str().unwrap().is_empty(),
        "error.message MUST be a non-empty string"
    );
}

#[test]
fn batch_human_mode_remains_silent_on_stdout() {
    // ADR-0001 / pre-existing: batch in human mode emits its summary on
    // STDERR, not stdout. Regression-test that this is true even after
    // #205 wires the json branch onto stdout.
    let dir = TempDir::new().expect("tempdir");
    let refs = dir.path().join("refs.txt");
    std::fs::File::create(&refs)
        .expect("create refs file")
        .write_all(b"not-a-doi\n")
        .expect("write refs");

    let output = doiget(&dir)
        .env("DOIGET_MODE", "human")
        .args(["batch", refs.to_str().unwrap()])
        .assert()
        .failure()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).expect("stdout utf-8");
    assert!(
        stdout.is_empty(),
        "human-mode batch stdout MUST be empty (summary is stderr): {stdout:?}"
    );
}
