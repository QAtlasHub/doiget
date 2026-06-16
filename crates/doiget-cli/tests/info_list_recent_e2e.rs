//! End-to-end tests for `doiget info` and `doiget list-recent`.
//!
//! Strategy: seed a `FsStore` rooted at a per-test `tempfile::TempDir`,
//! then invoke the freshly-built `doiget` binary as a subprocess with
//! `DOIGET_STORE_ROOT` pointing at that tempdir. This guarantees no test
//! ever touches the real `~/papers/`, and because env mutation happens
//! ONLY on the child process (via `assert_cmd::Command::env`), the tests
//! are safe to run in parallel without `serial_test` coordination.
//!
//! The two assertions per subcommand:
//!
//! 1. Exit status is success.
//! 2. Stdout contains an expected substring (paper title for `info`,
//!    safekey + title for `list-recent`).
//!
//! No assertions are made about the exact byte-for-byte stdout layout;
//! that lets the underlying serializer evolve (e.g. toml-rs upgrades)
//! without breaking these tests.

// Tests are panic-on-failure by design; relax the workspace-wide lints
// that ban `expect`/`unwrap`/`panic` in production code.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeMap;

use assert_cmd::Command;
use camino::Utf8PathBuf;
use chrono::TimeZone;
use predicates::prelude::*;
use tempfile::TempDir;

use doiget_core::store::{DoigetExtension, FsStore, Metadata, Store};
use doiget_core::{Doi, Safekey, SCHEMA_VERSION};

/// Convert a `TempDir`'s path to a `Utf8PathBuf` so it can drive
/// `FsStore::new` (which is camino-only). Panics if the temp path is not
/// UTF-8 — on every platform we test, the system temp dir is ASCII.
fn utf8_path(dir: &TempDir) -> Utf8PathBuf {
    Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("temp dir path must be UTF-8")
}

/// Build a small `Metadata` fixture with the given DOI suffix, title, and
/// fetched-at year. All other fields are constant.
fn fixture(doi_suffix: &str, title: &str, year: i32, fetched_year: i32) -> (Safekey, Metadata) {
    let doi = format!("10.1234/{doi_suffix}");
    let ref_ = doiget_core::Ref::Doi(Doi::parse(&doi).expect("valid DOI"));
    let safekey = ref_.safekey();
    let m = Metadata {
        schema_version: SCHEMA_VERSION.to_string(),
        title: title.to_string(),
        authors: vec!["Alice Researcher".to_string()],
        year: Some(year),
        doi: Some(Doi::parse(&doi).expect("valid DOI")),
        arxiv_id: None,
        arxiv_categories: vec![],
        abstract_: None,
        venue: Some("Phys. Rev. X".to_string()),
        volume: None,
        issue: None,
        pages: None,
        publisher: None,
        issn: None,
        isbn: None,
        type_: Some("journal-article".to_string()),
        keywords: vec![],
        url: None,
        pdf_path: None,
        doiget: Some(DoigetExtension {
            fetched_at: chrono::Utc
                .with_ymd_and_hms(fetched_year, 5, 6, 12, 0, 0)
                .single()
                .expect("valid timestamp"),
            source: "unpaywall".to_string(),
            license: "CC-BY-4.0".to_string(),
            oa_status: None,
            size_bytes: 1234,
            mcp_call_id: None,
        }),
        other: BTreeMap::new(),
    };
    (safekey, m)
}

/// Seed a temp store with two distinct entries and return the (TempDir
/// guard, store-root) pair. The guard MUST be kept alive for the duration
/// of the test — dropping it deletes the tempdir.
fn seeded_store() -> (TempDir, Utf8PathBuf) {
    let dir = TempDir::new().expect("tempdir");
    let root = utf8_path(&dir).join("papers");
    let store = FsStore::new(root.clone()).expect("FsStore::new");

    let (k1, m1) = fixture("alpha", "First Quantum Result", 2024, 2024);
    let (k2, m2) = fixture("beta", "Second Quantum Result", 2026, 2026);
    store.write(&k1, &m1, None).expect("seed entry 1");
    store.write(&k2, &m2, None).expect("seed entry 2");

    (dir, root)
}

/// Configure a `doiget` subprocess to use `root` as its store. Sets
/// `DOIGET_STORE_ROOT` (the primary resolution hook) and clears
/// `HOME` / `USERPROFILE` to belt-and-suspenders against any fallback
/// codepath leaking the developer's real home directory into the test.
fn doiget(root: &Utf8PathBuf) -> Command {
    let mut cmd = Command::cargo_bin("doiget").expect("locate doiget binary");
    cmd.env("DOIGET_STORE_ROOT", root.as_str())
        .env("HOME", root.as_str())
        .env("USERPROFILE", root.as_str())
        // #203: opt into human stdout — assert_cmd's captured stdout is
        // non-TTY, which defaults to Quiet after #203 honoring.
        .env("DOIGET_MODE", "human");
    cmd
}

#[test]
fn info_prints_metadata_for_stored_entry() {
    let (_dir_guard, root) = seeded_store();

    doiget(&root)
        .args(["info", "10.1234/alpha"])
        .assert()
        .success()
        .stdout(predicate::str::contains("First Quantum Result"));
}

#[test]
fn info_fails_for_missing_entry() {
    let (_dir_guard, root) = seeded_store();

    doiget(&root)
        .args(["info", "10.9999/missing"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no entry for"));
}

#[test]
fn list_recent_prints_seeded_entries_in_recency_order() {
    let (_dir_guard, root) = seeded_store();

    let assert = doiget(&root).args(["list-recent"]).assert().success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone())
        .expect("doiget list-recent stdout was not UTF-8");

    // Header line.
    assert!(
        stdout.starts_with("safekey\tyear\ttitle\tfetched_at"),
        "expected header line, got:\n{stdout}"
    );
    // Both seeded titles appear.
    assert!(
        stdout.contains("First Quantum Result"),
        "missing seeded title 'First Quantum Result' in stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("Second Quantum Result"),
        "missing seeded title 'Second Quantum Result' in stdout:\n{stdout}"
    );

    // Recency order: 2026 entry must appear before 2024 entry.
    let pos_2026 = stdout
        .find("Second Quantum Result")
        .expect("2026 entry present");
    let pos_2024 = stdout
        .find("First Quantum Result")
        .expect("2024 entry present");
    assert!(
        pos_2026 < pos_2024,
        "expected 2026 entry before 2024 entry; stdout:\n{stdout}"
    );

    // Safekeys derived from `Ref::safekey()` for the seeded DOIs.
    assert!(
        stdout.contains("doi_10.1234_alpha"),
        "safekey 'doi_10.1234_alpha' missing from stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("doi_10.1234_beta"),
        "safekey 'doi_10.1234_beta' missing from stdout:\n{stdout}"
    );
}

#[test]
fn list_recent_with_explicit_limit_truncates() {
    let (_dir_guard, root) = seeded_store();

    let assert = doiget(&root).args(["list-recent", "1"]).assert().success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone())
        .expect("doiget list-recent stdout was not UTF-8");

    // Header + exactly 1 data row → 2 newline-terminated lines total.
    let line_count = stdout.lines().count();
    assert_eq!(
        line_count, 2,
        "with --limit=1 expected header + 1 row, got {line_count} lines:\n{stdout}"
    );

    // The single row must be the most-recent entry (2026).
    assert!(
        stdout.contains("Second Quantum Result"),
        "expected most-recent (2026) entry in 1-row output:\n{stdout}"
    );
    assert!(
        !stdout.contains("First Quantum Result"),
        "did not expect older (2024) entry with --limit=1; stdout:\n{stdout}"
    );
}

#[test]
fn list_recent_on_empty_store_prints_only_header() {
    // Empty store: no .metadata files at all. We still expect a successful
    // exit and a header-only stdout, so callers can pipe `| tail -n +2`
    // without a special-case for emptiness.
    let dir = TempDir::new().expect("tempdir");
    let root = utf8_path(&dir).join("papers");
    // FsStore::new creates the dirs; no entries are seeded.
    FsStore::new(root.clone()).expect("FsStore::new");

    let assert = doiget(&root).args(["list-recent"]).assert().success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone())
        .expect("doiget list-recent stdout was not UTF-8");
    assert_eq!(
        stdout, "safekey\tyear\ttitle\tfetched_at\n",
        "empty store should produce header-only stdout, got:\n{stdout}"
    );
}

// ---- #204 JSON-mode coverage --------------------------------------------

#[test]
fn info_json_emits_metadata_object() {
    let (_dir_guard, root) = seeded_store();

    let out = doiget(&root)
        // The #203 helper sets DOIGET_MODE=human; override per-test for
        // the JSON case (env_clear is too invasive — we still need
        // DOIGET_STORE_ROOT / HOME for the resolver).
        .env("DOIGET_MODE", "json")
        .args(["info", "10.1234/alpha"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let s = String::from_utf8(out).expect("info JSON stdout utf-8");
    let v: serde_json::Value = serde_json::from_str(&s).expect("info JSON parses");
    // The Metadata struct's `title` field is the only stable assertion
    // surface at this level (the rest is the on-disk TOML schema).
    assert_eq!(v["title"], "First Quantum Result");
}

#[test]
fn list_recent_json_emits_array_of_entries() {
    let (_dir_guard, root) = seeded_store();

    let out = doiget(&root)
        .env("DOIGET_MODE", "json")
        .args(["list-recent"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let s = String::from_utf8(out).expect("list-recent JSON stdout utf-8");
    let v: serde_json::Value = serde_json::from_str(&s).expect("list-recent JSON parses");
    let arr = v.as_array().expect("list-recent JSON is an array");
    assert_eq!(arr.len(), 2, "seeded store has 2 entries");
    // EntryInfo schema (#204): {safekey, title, year, fetched_at}.
    for entry in arr {
        assert!(entry["safekey"].is_string(), "safekey is a string");
        assert!(entry["title"].is_string(), "title is a string");
    }
}

// ---- #301 / ADR-0017 Amendment 2: artifact-class honors only EXPLICIT Quiet

/// `doiget` rooted at `root` WITHOUT forcing `DOIGET_MODE`, so the
/// resolver sees a non-TTY captured stdout (assert_cmd pipes it) and
/// falls through to the *implicit* Quiet branch. Pre-#301 this silenced
/// `info` / `list-recent`; post-#301 they are artifact-class and emit.
/// `DOIGET_MODE` is explicitly removed in case the parent environment
/// has it set.
fn doiget_implicit(root: &Utf8PathBuf) -> Command {
    let mut cmd = Command::cargo_bin("doiget").expect("locate doiget binary");
    cmd.env("DOIGET_STORE_ROOT", root.as_str())
        .env("HOME", root.as_str())
        .env("USERPROFILE", root.as_str())
        .env_remove("DOIGET_MODE");
    cmd
}

#[test]
fn info_emits_under_implicit_non_tty_quiet() {
    // The #301 repro: agent / pipe / ssh caller (non-TTY, no flag, no
    // DOIGET_MODE). `info` MUST still print its metadata.
    let (_dir_guard, root) = seeded_store();
    doiget_implicit(&root)
        .args(["info", "10.1234/alpha"])
        .assert()
        .success()
        .stdout(predicate::str::contains("First Quantum Result"));
}

#[test]
fn list_recent_emits_under_implicit_non_tty_quiet() {
    let (_dir_guard, root) = seeded_store();
    doiget_implicit(&root)
        .args(["list-recent"])
        .assert()
        .success()
        .stdout(predicate::str::contains("First Quantum Result"));
}

#[test]
fn info_explicit_quiet_flag_suppresses_stdout() {
    // Explicit `--quiet` still silences (the entry exists → exit 0); the
    // not-found contract is unaffected.
    let (_dir_guard, root) = seeded_store();
    doiget_implicit(&root)
        .args(["--quiet", "info", "10.1234/alpha"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn list_recent_explicit_quiet_flag_suppresses_stdout() {
    let (_dir_guard, root) = seeded_store();
    doiget_implicit(&root)
        .args(["-q", "list-recent"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn info_explicit_quiet_env_suppresses_stdout() {
    // `DOIGET_MODE=quiet` is an explicit Quiet signal → suppress.
    let (_dir_guard, root) = seeded_store();
    doiget_implicit(&root)
        .env("DOIGET_MODE", "quiet")
        .args(["info", "10.1234/alpha"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}
