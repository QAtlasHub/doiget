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

use std::fs;

use assert_cmd::Command;
use camino::Utf8PathBuf;
use doiget_core::provenance::{Capability, LogEvent, LogResult, ProvenanceLog, RowInput};
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
    // #212: list-recent --json now emits {ok, count, entries} envelope.
    assert_eq!(v["ok"], true, "envelope ok");
    assert!(
        v["entries"].is_array(),
        "list-recent emits an entries array"
    );
    assert_eq!(
        v["entries"].as_array().unwrap().len(),
        0,
        "empty store → []"
    );
    assert_eq!(v["count"], 0, "count matches entries length");
}

// ---- audit-log issue-rendering JSON path -------------------------------

/// Seed a 2-row chain at `path`, then tamper row 2's `this_hash` to an
/// all-zero (valid 64-hex, impossible-SHA-256) value. Mirrors the
/// helper in `audit_log_e2e.rs` for the multi-segment test.
fn seed_then_tamper(path: &Utf8PathBuf) {
    let log = ProvenanceLog::open(path.clone(), "01JCKZ7Q0000000000000000AB".to_string())
        .expect("open provenance log");
    for _ in 0..2 {
        log.append(RowInput {
            event: LogEvent::Fetch,
            result: LogResult::Ok,
            capability: Capability::Oa,
            ref_: None,
            source: None,
            error_code: None,
            size_bytes: None,
            license: None,
            store_path: None,
            canonical_digest: None,
        })
        .expect("append seed row");
    }
    drop(log);
    let raw = fs::read_to_string(path).expect("read log");
    let mut lines: Vec<String> = raw.lines().map(str::to_string).collect();
    let needle = "\"this_hash\":\"";
    let target = &lines[1];
    let start = target.find(needle).expect("this_hash field") + needle.len();
    let end = start + target[start..].find('"').expect("closing quote");
    let mut new_line = String::with_capacity(target.len());
    new_line.push_str(&target[..start]);
    new_line.push_str("0000000000000000000000000000000000000000000000000000000000000000");
    new_line.push_str(&target[end..]);
    lines[1] = new_line;
    let mut out = lines.join("\n");
    out.push('\n');
    fs::write(path, out).expect("write tampered log");
}

#[test]
fn audit_log_json_with_tampered_log_emits_issue_records() {
    let dir = TempDir::new().expect("tempdir");
    let log_path =
        Utf8PathBuf::from_path_buf(dir.path().join("access.jsonl")).expect("utf-8 log path");
    seed_then_tamper(&log_path);

    let p = dir.path().to_str().expect("tempdir path is UTF-8");
    let out = Command::cargo_bin("doiget")
        .expect("locate doiget binary")
        .env("HOME", p)
        .env("USERPROFILE", p)
        .env("APPDATA", p)
        .env("XDG_CONFIG_HOME", p)
        .env("DOIGET_LOG_PATH", log_path.as_str())
        .args(["--json", "audit-log", "--verify"])
        .assert()
        .failure() // tampered → non-zero exit, but stdout is still JSON
        .get_output()
        .stdout
        .clone();
    let s = String::from_utf8(out).expect("stdout utf-8");
    let v: Value = serde_json::from_str(&s).expect("audit-log JSON parses");
    assert_eq!(v["total_rows"], 2);
    assert_eq!(v["total_issues"], 1, "exactly one tampered row");
    let issues = v["issues"].as_array().expect("issues array");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0]["line"], 2);
    assert_eq!(issues[0]["kind"], "this-hash");
    assert!(issues[0]["message"].is_string());
    let segs = v["segments"].as_array().expect("segments array");
    assert_eq!(segs.len(), 1, "single segment (no rotation)");
    assert_eq!(segs[0]["rows"], 2);
    assert_eq!(segs[0]["ok"], 1);
    assert_eq!(segs[0]["issues"], 1);
}

// ---- config show / config path --json ----------------------------------
//
// These rely on `dirs::config_dir()` resolving from `XDG_CONFIG_HOME`,
// which works on the Linux CI runners that drive codecov. The earlier
// local Windows fail (Known-Folder API ignores env overrides) is a
// platform quirk, not a contract gap; the tests are gated to non-Windows
// at the runtime layer so a contributor on Windows isn't surprised.

#[cfg(not(target_os = "windows"))]
#[test]
fn config_show_json_emits_resolved_config_object() {
    let dir = TempDir::new().expect("tempdir");
    let out = doiget(&dir)
        .env("DOIGET_CONTACT_EMAIL", "test@example.com")
        .args(["--json", "config", "show"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let s = String::from_utf8(out).expect("config show JSON stdout utf-8");
    let v: Value = serde_json::from_str(&s).expect("config show JSON parses");
    // ResolvedConfig schema (#204): store_root / log_path / config_path
    // are always populated.
    assert!(v["store_root"].is_string(), "store_root present");
    assert!(v["log_path"].is_string(), "log_path present");
    assert!(v["config_path"].is_string(), "config_path present");
    assert_eq!(v["contact_email"], "test@example.com");
}

#[cfg(not(target_os = "windows"))]
#[test]
fn config_path_json_emits_config_path_object() {
    let dir = TempDir::new().expect("tempdir");
    let out = doiget(&dir)
        .env("DOIGET_CONTACT_EMAIL", "test@example.com")
        .args(["--json", "config", "path"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let s = String::from_utf8(out).expect("config path JSON stdout utf-8");
    let v: Value = serde_json::from_str(&s).expect("config path JSON parses");
    assert!(v["config_path"].is_string(), "config_path field present");
    assert!(
        v["config_path"].as_str().unwrap().ends_with("config.toml"),
        "config_path points at config.toml: {}",
        v["config_path"]
    );
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
