//! End-to-end tests for the ADR-0017 / #144 global output-mode flags.
//!
//! These exercise the **resolver as a black box** through the freshly-built
//! `doiget` binary: clap's parse acceptance of `--mode` / `--json` /
//! `--quiet`, the mutual-exclusion enforcement, and `DOIGET_MODE` env
//! plumbing. Per-command output bodies are tracked in follow-up issues
//! #203 / #204 / #205.
//!
//! The unit-tested ladder lives in `commands::output::tests` (12 cases);
//! this file proves the wiring through clap reaches the resolver
//! correctly.

// Tests panic on failure by design; the workspace deny-lints for
// `expect`/`unwrap`/`panic` are scoped to production code.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use assert_cmd::Command;
use predicates::prelude::*;

/// Build an `assert_cmd::Command` for the freshly-built `doiget` binary,
/// with `HOME` / `USERPROFILE` overridden so the home-dir resolver in
/// `commands::*` never touches the developer's real `~/.config/doiget/`.
fn doiget() -> Command {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let mut cmd = Command::cargo_bin("doiget").expect("locate doiget binary");
    cmd.env("HOME", dir.path()).env("USERPROFILE", dir.path());
    // Leak the TempDir so it survives until the child exits.
    Box::leak(Box::new(dir));
    cmd
}

// ---- `--mode` / `--json` / `--quiet` parse acceptance ---------------------

#[test]
fn mode_flag_accepts_all_four_values_on_help() {
    // `--help` short-circuits without needing the subcommand, so it is a
    // safe vehicle for confirming clap accepts each `--mode <m>` value.
    for value in ["human", "json", "quiet", "mcp"] {
        doiget()
            .args(["--mode", value, "--help"])
            .assert()
            .success();
    }
}

#[test]
fn json_short_flag_is_accepted() {
    doiget().args(["--json", "--help"]).assert().success();
}

#[test]
fn quiet_short_flag_is_accepted_long_form() {
    doiget().args(["--quiet", "--help"]).assert().success();
}

#[test]
fn q_short_flag_is_accepted_short_form() {
    doiget().args(["-q", "--help"]).assert().success();
}

// ---- mutual exclusion (clap conflicts_with_all) ---------------------------

// NOTE: these conflict-check probes intentionally omit `--help`. Clap
// processes `--help` (a `DisplayHelp` short-circuit) BEFORE running the
// `conflicts_with_all` validator, so `doiget --mode x --json --help`
// shows help and exits 0. Targeting a real subcommand (`audit-log
// --verify`, which would otherwise exit 0 on a clean log here) forces
// clap to validate the global-flag conflicts before dispatch.

#[test]
fn mode_and_json_conflict() {
    doiget()
        .args(["--mode", "human", "--json", "audit-log", "--verify"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used"));
}

#[test]
fn mode_and_quiet_conflict() {
    doiget()
        .args(["--mode", "human", "--quiet", "audit-log", "--verify"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used"));
}

#[test]
fn json_and_quiet_conflict() {
    doiget()
        .args(["--json", "--quiet", "audit-log", "--verify"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used"));
}

// ---- unknown `--mode` value rejected ---------------------------------------

#[test]
fn unknown_mode_value_is_rejected() {
    doiget()
        .args(["--mode", "garbage", "--help"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid value"));
}

// ---- DOIGET_MODE env passes through clap untouched -------------------------

#[test]
fn doiget_mode_env_does_not_break_clap_parsing() {
    // `DOIGET_MODE` is consumed by the resolver, not clap; setting it MUST
    // not change argument parsing. Confirm by combining it with `--help`.
    for value in ["human", "json", "quiet", "mcp", "garbage", ""] {
        doiget()
            .env("DOIGET_MODE", value)
            .args(["--help"])
            .assert()
            .success();
    }
}

// ---- flags are global (appear after the subcommand) ------------------------

#[test]
fn mode_flag_is_global_after_subcommand() {
    // `global = true` on the Cli fields means `--mode` may appear on the
    // subcommand line: `doiget config show --mode human` is equivalent to
    // `doiget --mode human config show`. We probe with `--help` on a
    // subcommand to avoid touching the store / network.
    doiget()
        .args(["audit-log", "--mode", "human", "--help"])
        .assert()
        .success();
}
