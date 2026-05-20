//! End-to-end tests for `--mode json` bodies (#204).
//!
//! Each command that emits a JSON body MUST produce a single parseable
//! JSON value on stdout (NOT JSON-Lines; the batch JSONL contract is
//! a separate ERRORS.md §3 surface tracked in #205). Stderr remains
//! the human-error sink.
//!
//! The store-population-free commands are exercised here:
//! `audit-log --verify` on a missing log (empty 0-row report) and
//! `list-recent` on an empty store (empty array). The store-populated
//! `info` / `search` bodies and the mutation-requiring
//! `provenance migrate` JSON output round-trip the same `Serialize`
//! impls, so the in-code structure is covered by `cargo build` + the
//! integration shape here.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

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
        .env("DOIGET_STORE_ROOT", dir.path().join("store"));
    cmd
}

// ---- audit-log --verify -------------------------------------------------

#[test]
fn audit_log_json_emits_report_object() {
    let dir = TempDir::new().expect("tempdir");
    let out = doiget(&dir)
        .args(["--json", "audit-log", "--verify"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let s = String::from_utf8(out).expect("stdout utf-8");
    let v: Value = serde_json::from_str(&s).expect("audit-log JSON parses");
    assert_eq!(v["total_rows"], 0, "missing log → 0 rows");
    assert_eq!(v["total_ok"], 0);
    assert_eq!(v["total_issues"], 0);
    assert!(v["segments"].is_array(), "segments is an array");
    assert!(v["issues"].is_array(), "issues is an array");
}

#[test]
fn audit_log_json_via_mode_flag_parses() {
    let dir = TempDir::new().expect("tempdir");
    let out = doiget(&dir)
        .args(["--mode", "json", "audit-log", "--verify"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let s = String::from_utf8(out).expect("stdout utf-8");
    let _: Value = serde_json::from_str(&s).expect("audit-log JSON parses");
}

#[test]
fn audit_log_json_via_env_parses() {
    let dir = TempDir::new().expect("tempdir");
    let out = doiget(&dir)
        .env("DOIGET_MODE", "json")
        .args(["audit-log", "--verify"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let s = String::from_utf8(out).expect("stdout utf-8");
    let _: Value = serde_json::from_str(&s).expect("audit-log JSON parses");
}

// ---- list-recent -------------------------------------------------------

#[test]
fn list_recent_json_empty_store_emits_empty_array() {
    let dir = TempDir::new().expect("tempdir");
    let out = doiget(&dir)
        .args(["--json", "list-recent"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let s = String::from_utf8(out).expect("stdout utf-8");
    let v: Value = serde_json::from_str(&s).expect("list-recent JSON parses");
    assert!(v.is_array(), "list-recent emits an array");
    assert_eq!(v.as_array().unwrap().len(), 0, "empty store → []");
}

// ---- provenance migrate --json -----------------------------------------

#[test]
fn provenance_migrate_dry_run_json_emits_report_object() {
    // A missing log is the cleanest no-setup path: `migrate_v1_to_v2`
    // returns an empty `MigrationReport` in dry-run mode, which the
    // JSON branch wraps with `log_path` and emits.
    let dir = TempDir::new().expect("tempdir");
    let out = doiget(&dir)
        .args(["--json", "provenance", "migrate", "--dry-run"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let s = String::from_utf8(out).expect("stdout utf-8");
    let v: Value = serde_json::from_str(&s).expect("provenance migrate JSON parses");
    assert!(v["log_path"].is_string(), "log_path field present");
    assert_eq!(
        v["report"]["dry_run"],
        Value::Bool(true),
        "dry_run flag round-trips"
    );
    assert!(
        v["report"]["rows_rewritten"].is_number(),
        "rows_rewritten present"
    );
}

// ---- regression: human / quiet modes still behave as before ------------

#[test]
fn audit_log_human_is_not_json() {
    let dir = TempDir::new().expect("tempdir");
    let out = doiget(&dir)
        .env("DOIGET_MODE", "human")
        .args(["audit-log", "--verify"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let s = String::from_utf8(out).expect("stdout utf-8");
    assert!(
        serde_json::from_str::<Value>(&s).is_err(),
        "human stdout MUST NOT parse as JSON: {s}"
    );
}
