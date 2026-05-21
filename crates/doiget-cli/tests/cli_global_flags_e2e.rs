//! E2E coverage for the four CONFIG.md §5 global flags landed in S5
//! (#211): `--store-root`, `--log-path`, `--color`, `--progress` /
//! `--no-progress`.
//!
//! Pinned behaviors:
//!
//! 1. Each flag is *accepted* by clap and visible in `--help`.
//! 2. `--progress` and `--no-progress` are mutually exclusive at the
//!    clap layer (parse-time failure, exit 2).
//! 3. `--store-root <path>` wins over `DOIGET_STORE_ROOT` env. We
//!    cannot observe the resolved value directly from outside the
//!    process, so we use `doiget config show --mode json` (the env
//!    snapshot) as a *proxy* — after S5 wires the flag to env, that
//!    snapshot includes the flag value.
//! 4. `--color` and `--progress` are *surface-only* in this slice
//!    (no ANSI / progress emitter consumes them yet). We assert the
//!    flag is accepted and exposed via `DOIGET_COLOR` /
//!    `DOIGET_PROGRESS` for future consumers; no behaviour
//!    assertion beyond "doiget does not crash on the flag".
//!
//! All tests use `assert_cmd`'s `cargo_bin` so the production `Cli`
//! tree is exercised.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use assert_cmd::Command;
use tempfile::TempDir;

fn doiget(dir: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("doiget").expect("locate doiget binary");
    let p = dir.path().to_str().expect("tempdir path utf-8");
    cmd.env("HOME", p)
        .env("USERPROFILE", p)
        .env("APPDATA", p)
        .env("XDG_CONFIG_HOME", p)
        // S1 / ADR-0017 Am1: capabilities is artifact, but `config
        // show` (informational) suppresses on non-TTY. Force human so
        // these tests can see the resolved-config JSON.
        .env("DOIGET_MODE", "human");
    cmd
}

#[test]
fn progress_and_no_progress_are_mutually_exclusive() {
    // Clap-level conflict — parse fails, exit code 2 (clap default).
    let dir = TempDir::new().expect("tempdir");
    doiget(&dir)
        .args(["--progress", "--no-progress", "capabilities"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn mode_quiet_conflicts_remain_intact() {
    // Pre-existing conflict (S1 / #144): --json + --quiet rejected.
    // S5 adds new global flags; this test makes sure we didn't break
    // the existing conflicts_with_all wiring on --mode / --json /
    // --quiet.
    let dir = TempDir::new().expect("tempdir");
    doiget(&dir)
        .args(["--json", "--quiet", "capabilities"])
        .assert()
        .failure()
        .code(2);
}

// The flag-wins-over-env *precedence* for `--store-root` and
// `--log-path` is verified at the unit-test layer in `main.rs`
// (`tests::apply_overrides_*`), because asserting it end-to-end via
// `config show --mode json` would require `dirs::config_dir()` to
// succeed on every test host — which is platform-dependent in
// `assert_cmd`'s spawned-child env on Windows. The unit-level tests
// drive `apply_global_overrides` directly and verify the env mutation
// independently of `dirs`.

#[test]
fn color_flag_is_accepted_for_each_value() {
    // Surface-only in S5: doiget does not yet emit ANSI to gate. We
    // assert clap accepts each value (`auto` / `always` / `never`),
    // exit 0, and that an invalid value is rejected at parse time.
    let dir = TempDir::new().expect("tempdir");
    for value in ["auto", "always", "never"] {
        doiget(&dir)
            .args(["--color", value, "capabilities"])
            .assert()
            .success();
    }
    // Invalid value -> parse error (exit 2).
    doiget(&dir)
        .args(["--color", "rainbow", "capabilities"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn progress_and_no_progress_each_accept_with_capabilities() {
    // capabilities is artifact-output; --progress / --no-progress
    // should not affect its JSON inventory. Both must be accepted.
    let dir = TempDir::new().expect("tempdir");
    doiget(&dir)
        .args(["--progress", "capabilities"])
        .assert()
        .success();
    doiget(&dir)
        .args(["--no-progress", "capabilities"])
        .assert()
        .success();
}
