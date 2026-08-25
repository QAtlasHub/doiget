//! End-to-end tests for `--mode quiet` honoring across commands (#203).
//!
//! Each command that emits informational stdout in human mode MUST emit
//! exactly zero bytes on stdout under any of the three Quiet triggers:
//! `--mode quiet`, `-q` / `--quiet`, or `DOIGET_MODE=quiet`. Exit codes
//! and on-disk side effects are unaffected. Stderr (errors, warnings)
//! is also unaffected.
//!
//! The commands covered here are the no-setup-needed ones:
//! `audit-log --verify` (missing log = clean 0 rows), `config show` /
//! `config path` (always work), `list-recent` (empty store works). The
//! store-populated commands (`info` / `search`) and the
//! mutation-requiring `provenance migrate` get their Quiet coverage
//! through the seeded e2e tests in their respective files (those e2e
//! helpers explicitly set `DOIGET_MODE=human`, so any Quiet-leak there
//! would surface as a diff against the asserted human output).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

/// Build a `doiget` command rooted in `dir`, with `HOME` / `USERPROFILE`
/// / `DOIGET_LOG_PATH` / `DOIGET_STORE_ROOT` all under the tempdir so
/// the test never touches the developer's real `~/.config/doiget/` and
/// produces deterministic output.
fn doiget(dir: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("doiget").expect("locate doiget binary");
    let p = dir.path().to_str().expect("tempdir path is UTF-8");
    cmd.env("HOME", p)
        .env("USERPROFILE", p)
        // Cover all platforms' config-dir resolution so `config show` /
        // `config path` resolve successfully even in CI.
        .env("APPDATA", p)
        .env("XDG_CONFIG_HOME", p)
        .env("DOIGET_LOG_PATH", dir.path().join("access.jsonl"))
        .env("DOIGET_STORE_ROOT", dir.path().join("store"));
    cmd
}

// ---- audit-log --verify -------------------------------------------------

#[test]
fn audit_log_quiet_via_mode_flag_produces_no_stdout() {
    let dir = TempDir::new().expect("tempdir");
    doiget(&dir)
        .args(["--mode", "quiet", "audit-log", "--verify"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn audit_log_quiet_via_short_q_flag_produces_no_stdout() {
    let dir = TempDir::new().expect("tempdir");
    doiget(&dir)
        .args(["-q", "audit-log", "--verify"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn audit_log_quiet_via_env_produces_no_stdout() {
    let dir = TempDir::new().expect("tempdir");
    doiget(&dir)
        .env("DOIGET_MODE", "quiet")
        .args(["audit-log", "--verify"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn audit_log_human_still_emits_header() {
    // Regression: human-mode output unchanged.
    let dir = TempDir::new().expect("tempdir");
    doiget(&dir)
        .env("DOIGET_MODE", "human")
        .args(["audit-log", "--verify"])
        .assert()
        .success()
        .stdout(predicate::str::contains("audit-log verify: 0 rows"));
}

// `config show` / `config path` Quiet honoring on Linux CI. The
// resolver reads `XDG_CONFIG_HOME` on Linux/macOS so the env override
// reaches `dirs::config_dir()`. On Windows the resolver reads the
// Known Folder API directly (env-immune), so the test is gated. The
// in-code suppression structure is mechanical and reviewable in
// `commands/config.rs` (self-review for #207 §3 / #208 §4).

#[cfg(not(target_os = "windows"))]
#[test]
fn config_show_quiet_produces_no_stdout() {
    let dir = TempDir::new().expect("tempdir");
    doiget(&dir)
        .env("DOIGET_CONTACT_EMAIL", "test@example.com")
        .args(["--mode", "quiet", "config", "show"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[cfg(not(target_os = "windows"))]
#[test]
fn config_path_quiet_produces_no_stdout() {
    let dir = TempDir::new().expect("tempdir");
    doiget(&dir)
        .env("DOIGET_CONTACT_EMAIL", "test@example.com")
        .args(["--mode", "quiet", "config", "path"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

// ---- #476: implicit (non-TTY) Quiet must NOT silence `config` ---------
//
// `assert_cmd` always gives the child a pipe, never a terminal, so these
// exercise exactly the state the bug was reported in: no mode flag, no
// `DOIGET_MODE`, stdout not a TTY. Before #476 both printed zero bytes and
// exited 0 -- the documented way to locate your config file answering a
// script, a CI step or an agent with silence AND success.
//
// The pair above (explicit `--mode quiet`) is what must keep working; these
// are what must stop being suppressed. Both halves matter: fixing this by
// making `config` unconditionally loud would break #203.

#[cfg(not(target_os = "windows"))]
#[test]
fn config_path_prints_when_stdout_is_not_a_tty() {
    let dir = TempDir::new().expect("tempdir");
    doiget(&dir)
        .env("DOIGET_CONTACT_EMAIL", "test@example.com")
        .args(["config", "path"])
        .assert()
        .success()
        .stdout(predicate::str::contains("config.toml"));
}

#[cfg(not(target_os = "windows"))]
#[test]
fn config_show_prints_when_stdout_is_not_a_tty() {
    let dir = TempDir::new().expect("tempdir");
    doiget(&dir)
        .env("DOIGET_CONTACT_EMAIL", "test@example.com")
        .args(["config", "show"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty().not());
}

// ---- audit-log Quiet + chain issues: exit non-zero, stdout empty ------
//
// Self-review for #207 §2: tampered-log under Quiet must still raise
// the non-zero exit (so CI pipelines see the failure via $?) while
// keeping stdout empty (so `2>/dev/null` redactors don't leak the
// header text). This locks the "Quiet is for stdout suppression, not
// for failure suppression" contract.

/// Seed a 2-row chain at `path`, then tamper row 2's `this_hash` to an
/// all-zero (impossible-SHA-256) value. Same fixture shape as the
/// tampered audit-log JSON test in `json_bodies_e2e`.
fn seed_then_tamper(path: &std::path::Path) {
    use doiget_core::provenance::{Capability, LogEvent, LogResult, ProvenanceLog, RowInput};
    let utf8 = camino::Utf8PathBuf::from_path_buf(path.to_path_buf()).expect("tempdir path utf-8");
    let log = ProvenanceLog::open(utf8.clone(), "01JCKZ7Q0000000000000000AB".to_string())
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
    let raw = std::fs::read_to_string(path).expect("read log");
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
    std::fs::write(path, out).expect("write tampered log");
}

#[test]
fn audit_log_quiet_with_tampered_log_exits_nonzero_and_silent() {
    let dir = TempDir::new().expect("tempdir");
    let log_path = dir.path().join("access.jsonl");
    seed_then_tamper(&log_path);

    let p = dir.path().to_str().expect("tempdir path utf-8");
    Command::cargo_bin("doiget")
        .expect("locate doiget binary")
        .env("HOME", p)
        .env("USERPROFILE", p)
        .env("APPDATA", p)
        .env("XDG_CONFIG_HOME", p)
        .env("DOIGET_LOG_PATH", &log_path)
        .args(["--quiet", "audit-log", "--verify"])
        .assert()
        .failure() // chain issues → bail!() → non-zero exit
        .stdout(predicate::str::is_empty());
}

// ---- list-recent --------------------------------------------------------

#[test]
fn list_recent_quiet_empty_store_produces_no_stdout() {
    let dir = TempDir::new().expect("tempdir");
    doiget(&dir)
        .args(["--mode", "quiet", "list-recent"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn list_recent_human_empty_store_still_emits_header() {
    // Regression: even on an empty store the header line is emitted in
    // human mode so `cut -f1 | tail -n +2` shell pipelines do not break.
    let dir = TempDir::new().expect("tempdir");
    doiget(&dir)
        .env("DOIGET_MODE", "human")
        .args(["list-recent"])
        .assert()
        .success()
        .stdout(predicate::str::contains("safekey\tyear\ttitle\tfetched_at"));
}
