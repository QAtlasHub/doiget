//! Issue #119 — the `doiget fetch` human persona gets a
//! `docs/ERRORS.md` §3 cargo-style `error[CODE]:` line on stderr and
//! the §4 process exit code, instead of an opaque anyhow `{:?}` dump.
//!
//! Network-free: an invalid ref fails at `Ref::parse` before any
//! harness / store / network work, so this is a deterministic unit of
//! the §3/§4 wiring (the same `render_fetch_error` path handles every
//! `FetchError` variant — type-checked).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

#[test]
fn fetch_invalid_ref_emits_cargo_style_error_and_exit_1() {
    let td = TempDir::new().expect("tempdir");
    Command::cargo_bin("doiget")
        .expect("doiget binary built")
        // Hygiene only — the invalid ref fails before the store is
        // ever opened.
        .env("DOIGET_STORE_ROOT", td.path().to_str().expect("utf-8"))
        .env("DOIGET_LOG_PATH", "")
        .args(["fetch", "not a doi"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("error[INVALID_REF]: invalid ref"))
        // stdout MUST stay clean (ADR-0001 stdio rule).
        .stdout(predicate::str::is_empty());
}
