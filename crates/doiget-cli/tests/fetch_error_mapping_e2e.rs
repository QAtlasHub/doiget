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

// ---- #477: the contract is not "fetch has a code", it is "the CLI has" ---

/// Every ref-taking command emits an `error[CODE]:` first stderr line for
/// an unparseable input.
///
/// This is the assertion that matters. #119 gave `fetch` the contract and
/// nothing generalised it, so eight sibling commands spent four releases
/// printing a raw `anyhow` dump -- `Error:` plus a `Caused by:` chain that
/// leaks internal error types belonging to no contract -- while the docs
/// told callers they could key off `error[CODE]:`.
///
/// Table-driven on purpose: a new ref-taking subcommand is added to this
/// list or it is not covered, and the failure names which one regressed.
#[test]
fn every_ref_taking_command_emits_the_error_code_contract() {
    // `verify` is excluded: it takes a FILE PATH, not a ref, so an
    // unparseable value is a missing file rather than a bad ref. It gets
    // the `error:` misuse form instead -- see the test below.
    const COMMANDS: &[&[&str]] = &[
        &["fetch"],
        &["info"],
        &["link"],
        &["cite"],
        &["text"],
        &["bib"],
        &["csl"],
        // `source` needs `--out`; the ref is still the last positional.
        &["source", "--out", "."],
        &["graph"],
        &["tag"],
    ];

    let td = TempDir::new().expect("tempdir");
    let root = td.path().to_str().expect("utf-8");

    let mut broken: Vec<String> = Vec::new();
    for argv in COMMANDS {
        let mut cmd = Command::cargo_bin("doiget").expect("doiget binary built");
        cmd.env("DOIGET_STORE_ROOT", root)
            .env("DOIGET_LOG_PATH", "")
            .env("DOIGET_CONTACT_EMAIL", "test@example.com")
            .args(*argv)
            .arg("not a doi");
        let out = cmd.output().expect("run doiget");
        let stderr = String::from_utf8_lossy(&out.stderr);
        let first = stderr.lines().next().unwrap_or("");

        if !first.starts_with("error[") {
            broken.push(format!("{}: {first}", argv.join(" ")));
        }
        // The `Caused by:` chain is the anyhow dump the contract replaces.
        if stderr.contains("Caused by:") {
            broken.push(format!("{}: leaked a `Caused by:` chain", argv.join(" ")));
        }
    }

    assert!(
        broken.is_empty(),
        "these commands do not honour the error[CODE] contract for an invalid ref:\n  {}",
        broken.join("\n  ")
    );
}

/// `verify` takes a path, so its failure is a missing file. It still must
/// not dump anyhow: `docs/ERRORS.md` §4 classes an unusable argument as
/// misuse, and the closed `ErrorCode` set describes fetch outcomes, so
/// there is no code for "your input file is missing" (#477).
#[test]
fn verify_renders_a_missing_input_file_as_misuse_not_an_anyhow_dump() {
    let td = TempDir::new().expect("tempdir");
    Command::cargo_bin("doiget")
        .expect("doiget binary built")
        .env("DOIGET_STORE_ROOT", td.path().to_str().expect("utf-8"))
        .env("DOIGET_LOG_PATH", "")
        .args(["verify", "no-such-file.bib"])
        .assert()
        .code(2)
        .stderr(
            predicate::str::starts_with("error: failed to read reference file")
                .and(predicate::str::contains("Caused by:").not()),
        )
        .stdout(predicate::str::is_empty());
}

/// A mistyped DOI must not be told about arXiv.
///
/// `Ref::parse` falls through to the arXiv parser for anything without a
/// scheme or a `10.` prefix, and reported that parser's failure verbatim --
/// so `doiget fetch 10-1109-tsp` answered "input does not match any known
/// arXiv id shape" to someone who was clearly aiming at a DOI (#477).
#[test]
fn an_unrecognised_ref_names_neither_shape_rather_than_arxiv() {
    let td = TempDir::new().expect("tempdir");
    Command::cargo_bin("doiget")
        .expect("doiget binary built")
        .env("DOIGET_STORE_ROOT", td.path().to_str().expect("utf-8"))
        .env("DOIGET_LOG_PATH", "")
        .args(["fetch", "not-a-doi"])
        .assert()
        .stderr(
            predicate::str::contains("neither a DOI")
                .and(predicate::str::contains("nor an arXiv id")),
        );
}
