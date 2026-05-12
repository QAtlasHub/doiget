//! End-to-end tests for `doiget audit-log --verify`.
//!
//! Strategy: build a small valid provenance log via the real
//! `doiget_core::provenance::ProvenanceLog` writer, point `DOIGET_LOG_PATH`
//! at it via a per-test `tempfile::TempDir`, then invoke the freshly-built
//! `doiget` binary as a subprocess. Tests assert exit status and the
//! human-readable stdout shape produced by `commands::audit_log::run`.
//!
//! Each test sets `DOIGET_LOG_PATH` ONLY on the child process (via
//! `assert_cmd::Command::env`), so they are safe to run in parallel and
//! don't need `serial_test` — same convention as `info_list_recent_e2e.rs`.
//! Other env vars touched by the resolver fallback (`HOME`, `USERPROFILE`)
//! are explicitly clobbered to a tempdir for belt-and-suspenders against
//! a fallback codepath leaking the developer's real
//! `~/.config/doiget/access.jsonl`.

// Tests panic on failure by design; the workspace deny-lints for
// `expect`/`unwrap`/`panic` are scoped to production code.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::fs;

use assert_cmd::Command;
use camino::Utf8PathBuf;
use predicates::prelude::*;
use tempfile::TempDir;

use doiget_core::provenance::{Capability, LogEvent, LogResult, ProvenanceLog, RowInput};

/// Convert a `TempDir`'s path to a `Utf8PathBuf`. CI temp dirs are ASCII;
/// panic if not (acceptable for an integration test).
fn utf8_path(dir: &TempDir) -> Utf8PathBuf {
    Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("temp dir path must be UTF-8")
}

/// Seed a temp provenance log with `n` valid rows. Returns
/// `(TempDir guard, log path)`. The guard MUST be kept alive for the
/// duration of the test — dropping it deletes the tempdir.
fn seed_log(n: usize) -> (TempDir, Utf8PathBuf) {
    let dir = TempDir::new().expect("tempdir");
    let path = utf8_path(&dir).join("access.jsonl");

    let log = ProvenanceLog::open(path.clone(), "01JCKZ7Q0000000000000000AB".to_string())
        .expect("open provenance log");
    for _ in 0..n {
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

    (dir, path)
}

/// Build an `assert_cmd::Command` for the freshly-built `doiget` binary,
/// scoping `DOIGET_LOG_PATH` and the home-dir fallbacks to `dir_root`. Env
/// mutation happens ONLY on the child process.
fn doiget(log_path: &Utf8PathBuf, dir_root: &Utf8PathBuf) -> Command {
    let mut cmd = Command::cargo_bin("doiget").expect("locate doiget binary");
    cmd.env("DOIGET_LOG_PATH", log_path.as_str())
        // Belt-and-suspenders: clobber the home-dir resolution so a
        // resolver bug can't accidentally point at the developer's real
        // `~/.config/doiget/access.jsonl`.
        .env("HOME", dir_root.as_str())
        .env("USERPROFILE", dir_root.as_str());
    cmd
}

#[test]
fn audit_log_verify_clean_chain_succeeds() {
    let (dir_guard, log_path) = seed_log(3);
    let dir_root = utf8_path(&dir_guard);

    let assert = doiget(&log_path, &dir_root)
        .args(["audit-log", "--verify"])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone())
        .expect("doiget audit-log stdout was not UTF-8");

    // Header line includes the row count we seeded.
    assert!(
        stdout.contains("audit-log verify: 3 rows"),
        "expected header with row count, got:\n{stdout}"
    );
    // All three rows accounted for as ok, zero issues.
    assert!(
        stdout.contains("ok:     3"),
        "expected ok count of 3, got:\n{stdout}"
    );
    assert!(
        stdout.contains("issues: 0"),
        "expected zero issues on a clean log, got:\n{stdout}"
    );
}

#[test]
fn audit_log_verify_missing_log_succeeds() {
    // No log file at all — spec: missing file is a clean log.
    let dir = TempDir::new().expect("tempdir");
    let dir_root = utf8_path(&dir);
    let log_path = dir_root.join("never-created.jsonl");
    assert!(!log_path.exists(), "precondition: log must not exist");

    doiget(&log_path, &dir_root)
        .args(["audit-log", "--verify"])
        .assert()
        .success()
        .stdout(predicate::str::contains("audit-log verify: 0 rows"))
        .stdout(predicate::str::contains("issues: 0"));
}

#[test]
fn audit_log_without_verify_flag_errors() {
    // Phase 1: --verify is required.
    let dir = TempDir::new().expect("tempdir");
    let dir_root = utf8_path(&dir);
    let log_path = dir_root.join("access.jsonl");

    doiget(&log_path, &dir_root)
        .args(["audit-log"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--verify is required"));
}

#[test]
fn audit_log_verify_detects_tampered_this_hash() {
    // Build a 2-row log, then corrupt the second row's `this_hash` to a
    // syntactically-valid (64 hex chars) but wrong value. The subcommand
    // must exit non-zero and print a `this-hash` issue line.
    let (dir_guard, log_path) = seed_log(2);
    let dir_root = utf8_path(&dir_guard);

    let raw = fs::read_to_string(&log_path).expect("read log");
    let mut lines: Vec<String> = raw.lines().map(str::to_string).collect();
    assert_eq!(lines.len(), 2, "seed_log should produce exactly 2 rows");

    // Locate `"this_hash":"<64 hex>"` on row 2 and overwrite the value.
    let needle = "\"this_hash\":\"";
    let target = &lines[1];
    let start = target
        .find(needle)
        .expect("this_hash field present in row 2")
        + needle.len();
    let end_rel = target[start..]
        .find('"')
        .expect("closing quote for this_hash present");
    let end = start + end_rel;
    let bogus = "0000000000000000000000000000000000000000000000000000000000000000";
    let mut new_line = String::with_capacity(target.len());
    new_line.push_str(&target[..start]);
    new_line.push_str(bogus);
    new_line.push_str(&target[end..]);
    lines[1] = new_line;
    let mut tampered = lines.join("\n");
    tampered.push('\n');
    fs::write(&log_path, tampered).expect("write tampered log");

    let assert = doiget(&log_path, &dir_root)
        .args(["audit-log", "--verify"])
        .assert()
        .failure();

    let stdout = String::from_utf8(assert.get_output().stdout.clone())
        .expect("doiget audit-log stdout was not UTF-8");

    // Header line still names the total row count.
    assert!(
        stdout.contains("audit-log verify: 2 rows"),
        "expected header with row count, got stdout:\n{stdout}"
    );
    // The tampered row surfaces as a `this-hash` issue on line 2.
    assert!(
        stdout.contains("this-hash"),
        "expected 'this-hash' issue marker in stdout, got:\n{stdout}"
    );
    assert!(
        stdout.contains("line 2"),
        "expected issue to be reported on line 2, got stdout:\n{stdout}"
    );
}
