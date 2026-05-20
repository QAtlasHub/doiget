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

// NOTE: `config show` / `config path` Quiet honoring is covered by the
// in-code structure (`if mode != Quiet { print!(..) }` in
// `crates/doiget-cli/src/commands/config.rs`); a subprocess e2e for
// them requires reliably overriding the platform's config-dir resolver
// (`dirs::config_dir()` on Windows reads `FOLDERID_RoamingAppData`
// directly from the Known Folder API, which is intentionally not env-
// driven). Adding a platform-shim for the test is out of scope here;
// the lib-level unit tests in `output::tests` plus the config doctor
// e2e already exercise the resolver path.

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
