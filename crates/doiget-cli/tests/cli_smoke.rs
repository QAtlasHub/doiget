//! Smoke tests for the `doiget` CLI binary.
//!
//! These tests enforce the Phase 0 deliverable from `docs/PHASES.md` that
//! `doiget --help` runs and exits successfully even before any subcommand is
//! implemented. They use only `std::process::Command` so no extra dev
//! dependencies are required — Cargo populates the `CARGO_BIN_EXE_doiget`
//! environment variable for integration tests, pointing at the freshly built
//! binary.

// `expect`/`unwrap` are idiomatic in tests where panics double as assertions.
// The workspace lints deny them in production code; relax them for the test
// module only, matching the convention in `crates/doiget-core/src/lib.rs`.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::process::Command;

/// Path to the freshly built `doiget` binary, populated by Cargo for
/// integration tests in this crate.
fn doiget_bin() -> &'static str {
    env!("CARGO_BIN_EXE_doiget")
}

/// Returns true iff `s` contains a substring matching `\d+\.\d+\.\d+`.
///
/// Implemented manually to avoid pulling in the `regex` crate (which is not
/// in the workspace dependency set).
fn contains_semver_like(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Skip to the next ASCII digit.
        if !bytes[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        // Consume run of digits (group 1).
        let g1_start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'.' || i == g1_start {
            continue;
        }
        i += 1; // consume first '.'
                // Consume run of digits (group 2).
        let g2_start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'.' || i == g2_start {
            continue;
        }
        i += 1; // consume second '.'
                // Consume run of digits (group 3).
        let g3_start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i > g3_start {
            return true;
        }
    }
    false
}

#[test]
fn help_exits_successfully_with_usage_output() {
    let output = Command::new(doiget_bin())
        .arg("--help")
        .output()
        .expect("failed to spawn doiget --help");

    assert!(
        output.status.success(),
        "doiget --help exited with non-zero status: {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("doiget --help stdout was not UTF-8");
    assert!(
        !stdout.is_empty(),
        "doiget --help produced empty stdout (expected clap usage block)"
    );
    assert!(
        stdout.contains("doiget"),
        "doiget --help stdout missing binary name 'doiget':\n{}",
        stdout
    );
    // `fetch` is the canonical Phase 1 subcommand declared in
    // `crates/doiget-cli/src/main.rs`. Its appearance in `--help` is the
    // most stable way to confirm clap emitted the subcommand list.
    assert!(
        stdout.contains("fetch"),
        "doiget --help stdout missing 'fetch' subcommand:\n{}",
        stdout
    );
}

#[test]
fn version_exits_successfully_and_prints_semver() {
    let output = Command::new(doiget_bin())
        .arg("--version")
        .output()
        .expect("failed to spawn doiget --version");

    assert!(
        output.status.success(),
        "doiget --version exited with non-zero status: {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("doiget --version stdout was not UTF-8");
    assert!(!stdout.is_empty(), "doiget --version produced empty stdout");
    assert!(
        contains_semver_like(&stdout),
        "doiget --version stdout did not contain a semver-shaped token \
         (expected pattern: digit+.digit+.digit+):\n{}",
        stdout
    );
}
