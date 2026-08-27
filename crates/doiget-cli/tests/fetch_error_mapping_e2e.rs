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

/// #119 gave `fetch` the `error[CODE]:` line and asserted exit **1**,
/// which is what `cli_exit_code`'s catch-all produced rather than a
/// decision anyone made. `docs/ERRORS.md` §4 reserves 1 for "at least one
/// fetch failed", and an unparsable ref fetches nothing.
///
/// #492 / ADR-0049 moved it to 2 for every ref-taking command at once.
/// The name is part of the assertion, so it moved too.
#[test]
fn fetch_invalid_ref_emits_cargo_style_error_and_exit_2() {
    let td = TempDir::new().expect("tempdir");
    Command::cargo_bin("doiget")
        .expect("doiget binary built")
        // Hygiene only — the invalid ref fails before the store is
        // ever opened.
        .env("DOIGET_STORE_ROOT", td.path().to_str().expect("utf-8"))
        .env("DOIGET_LOG_PATH", "")
        .args(["fetch", "not a doi"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("error[INVALID_REF]: invalid ref"))
        // stdout MUST stay clean (ADR-0001 stdio rule).
        .stdout(predicate::str::is_empty());
}

// ---- #477: the contract is not "fetch has a code", it is "the CLI has" ---

/// Every ref-taking command emits an `error[CODE]:` first stderr line for
/// an unparsable input.
///
/// This is the assertion that matters. #119 gave `fetch` the contract and
/// nothing generalised it, so eight sibling commands spent four releases
/// printing a raw `anyhow` dump -- `Error:` plus a `Caused by:` chain that
/// leaks internal error types belonging to no contract -- while the docs
/// told callers they could key off `error[CODE]:`.
///
/// Table-driven on purpose: a new ref-taking subcommand is added to this
/// list or it is not covered, and the failure names which one regressed.
/// Every subcommand that takes a ref, read back from clap rather than
/// hand-listed.
///
/// This was a literal, and it silently omitted `tex-source`, `frontier`
/// and `annotate` — two of which then violated BOTH halves of the contract
/// these tests exist to enforce, throughout the very release that claimed
/// to have unified them. A second hand-maintained copy of the subcommand
/// set is the #454 / #504 shape; this reads the one clap actually built.
///
/// Excluded by name, each for a stated reason:
/// - `verify` takes a FILE PATH, so an unparsable value is a missing file.
/// - `batch` takes a file of refs and reports per row, not per process.
/// - `resolve-citation` takes free text, not a ref.
/// - the rest simply take no ref.
fn ref_taking_commands() -> Vec<Vec<String>> {
    const NOT_A_REF: &[&str] = &[
        "verify",
        "batch",
        "resolve-citation",
        // Reads pending citations from the store; takes no argument.
        "batch-resolve-citations",
        "version",
        "config",
        "serve",
        "search",
        "list-recent",
        "provenance",
        "audit-log",
        "lint",
        "capabilities",
        "help",
    ];

    let out = Command::cargo_bin("doiget")
        .expect("doiget binary built")
        .arg("--help")
        .output()
        .expect("run doiget --help");
    let help = String::from_utf8_lossy(&out.stdout);

    // clap lists subcommands indented, name first. Anything that is not a
    // flag and not on the exclusion list is expected to take a ref — so a
    // NEW ref-taking subcommand is covered by default, and a new non-ref
    // one fails here until it is named, which is the safe direction.
    let mut found: Vec<String> = help
        .lines()
        .filter(|l| l.starts_with("  "))
        // `split_whitespace` already skips the leading indent; trimming
        // first is what clippy's `trim_split_whitespace` flags.
        .filter_map(|l| l.split_whitespace().next())
        // Options share the two-space indent, so exclude anything that
        // starts with a dash before the lowercase check (which `--color`
        // would otherwise pass).
        .filter(|n| !n.is_empty() && !n.starts_with('-'))
        .filter(|n| n.chars().all(|c| c.is_ascii_lowercase() || c == '-'))
        .filter(|n| !NOT_A_REF.contains(n))
        .map(str::to_string)
        .collect();
    found.sort();
    found.dedup();
    assert!(
        found.len() >= 9,
        "parsed only {found:?} from --help — the parser has drifted from clap's output"
    );

    // Complete argv, bad ref included, because the ref is NOT the last
    // positional everywhere: `annotate` is `<ref> <text>`.
    found
        .into_iter()
        .map(|c| match c.as_str() {
            "source" => vec!["source", "--out", ".", BAD_REF]
                .into_iter()
                .map(str::to_string)
                .collect(),
            "annotate" => vec!["annotate", BAD_REF, "a note"]
                .into_iter()
                .map(str::to_string)
                .collect(),
            _ => vec![c, BAD_REF.to_string()],
        })
        .collect()
}

/// The value every command in the table is handed.
const BAD_REF: &str = "not a doi";

#[test]
fn every_ref_taking_command_emits_the_error_code_contract() {
    let commands = ref_taking_commands();

    let td = TempDir::new().expect("tempdir");
    let root = td.path().to_str().expect("utf-8");

    let mut broken: Vec<String> = Vec::new();
    for argv in &commands {
        let mut cmd = Command::cargo_bin("doiget").expect("doiget binary built");
        cmd.env("DOIGET_STORE_ROOT", root)
            .env("DOIGET_LOG_PATH", "")
            .env("DOIGET_CONTACT_EMAIL", "test@example.com")
            .args(argv);
        let out = cmd.output().expect("run doiget");
        let stderr = String::from_utf8_lossy(&out.stderr);
        let first = stderr.lines().next().unwrap_or("");

        // A feature-gated subcommand (`graph` needs `citation`) is absent
        // from a narrower build, and clap answers for it. That is clap's
        // business, not this contract's — and `--help` is parsed from a
        // binary that may have been built with a different feature set
        // than the one under test when they share a target directory.
        if first.starts_with("error: unrecognized subcommand") {
            continue;
        }
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

/// #492: the exit code is half the contract, and it was the half that
/// disagreed with itself.
///
/// `fetch` exited 1, `graph` exited 2, and `docs/ERRORS.md` §4 says 2 —
/// so one binary answered the same input two ways, and the comment at
/// each site asserted consistency with the other. A message contract that
/// every command honours and an exit code that half of them get wrong is
/// worse than neither, because a script keys off the exit code.
///
/// Same table as the message test above, deliberately: a new ref-taking
/// subcommand is covered by both or by neither.
#[test]
fn every_ref_taking_command_exits_2_for_an_invalid_ref() {
    let commands = ref_taking_commands();

    let td = TempDir::new().expect("tempdir");
    let root = td.path().to_str().expect("utf-8");

    let mut wrong: Vec<String> = Vec::new();
    for argv in &commands {
        let mut cmd = Command::cargo_bin("doiget").expect("doiget binary built");
        cmd.env("DOIGET_STORE_ROOT", root)
            .env("DOIGET_LOG_PATH", "")
            .env("DOIGET_CONTACT_EMAIL", "test@example.com")
            .args(argv);
        let out = cmd.output().expect("run doiget");
        // As above: a subcommand this build does not have is clap's answer,
        // not this contract's.
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.starts_with("error: unrecognized subcommand") {
            continue;
        }
        if out.status.code() != Some(2) {
            wrong.push(format!("{}: exit {:?}", argv.join(" "), out.status.code()));
        }
    }

    assert!(
        wrong.is_empty(),
        "docs/ERRORS.md §4: an unparsable ref is misuse (exit 2), not a failed fetch \
         (exit 1). These disagree:\n  {}",
        wrong.join("\n  ")
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
