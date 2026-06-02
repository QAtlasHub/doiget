//! End-to-end tests for `doiget search <query>`.
//!
//! Strategy mirrors `tests/info_list_recent_e2e.rs`: seed a `FsStore`
//! rooted at a per-test `tempfile::TempDir`, then invoke the freshly-built
//! `doiget` binary as a subprocess with `DOIGET_STORE_ROOT` pointing at
//! that tempdir. This guarantees no test ever touches the real
//! `~/papers/`, and because env mutation happens ONLY on the child
//! process (via `assert_cmd::Command::env`), the tests are safe to run
//! in parallel without `serial_test` coordination.
//!
//! Coverage:
//!
//! 1. Title-substring hit (`search_finds_by_title_substring`).
//! 2. Authors-substring hit (`search_finds_by_authors_substring`).
//! 3. No-match → header-only stdout (`search_returns_no_match_for_unknown_query`).
//! 4. Empty query → non-zero exit (`search_rejects_empty_query`).
//!
//! No assertion is made on the exact byte-for-byte stdout layout beyond
//! the header line; the underlying serializer is free to evolve without
//! breaking these tests.

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

/// Build a `Metadata` fixture with caller-supplied `title` and `authors`.
/// All other fields are constant. The DOI suffix doubles as the per-entry
/// uniqueness handle.
fn fixture(doi_suffix: &str, title: &str, authors: Vec<String>) -> (Safekey, Metadata) {
    let doi = format!("10.1234/{doi_suffix}");
    let ref_ = doiget_core::Ref::Doi(Doi::parse(&doi).expect("valid DOI"));
    let safekey = ref_.safekey();
    let m = Metadata {
        schema_version: SCHEMA_VERSION.to_string(),
        title: title.to_string(),
        authors,
        year: Some(2025),
        doi: Some(Doi::parse(&doi).expect("valid DOI")),
        arxiv_id: None,
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
                .with_ymd_and_hms(2025, 5, 6, 12, 0, 0)
                .single()
                .expect("valid timestamp"),
            source: "unpaywall".to_string(),
            license: "CC-BY-4.0".to_string(),
            size_bytes: 1234,
            mcp_call_id: None,
        }),
        other: BTreeMap::new(),
    };
    (safekey, m)
}

/// Seed a temp store with two distinct entries that exercise both the
/// `title` and `authors` haystacks of `Store::search`. Returns the
/// (TempDir guard, store-root) pair. The guard MUST be kept alive for the
/// duration of the test — dropping it deletes the tempdir.
fn seeded_store() -> (TempDir, Utf8PathBuf) {
    let dir = TempDir::new().expect("tempdir");
    let root = utf8_path(&dir).join("papers");
    let store = FsStore::new(root.clone()).expect("FsStore::new");

    // Entry 1: distinctive title ("Quantum Stuff"), generic author.
    let (k1, m1) = fixture("quantum", "Quantum Stuff", vec!["Bob Generic".to_string()]);
    // Entry 2: generic title, distinctive author ("Alice Researcher").
    let (k2, m2) = fixture(
        "researcher",
        "An Unrelated Title",
        vec!["Alice Researcher".to_string()],
    );
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
fn search_finds_by_title_substring() {
    let (_dir_guard, root) = seeded_store();

    doiget(&root)
        .args(["search", "quantum"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Quantum Stuff"))
        .stdout(predicate::str::contains("doi_10.1234_quantum"));
}

#[test]
fn search_finds_by_authors_substring() {
    let (_dir_guard, root) = seeded_store();

    doiget(&root)
        .args(["search", "Researcher"])
        .assert()
        .success()
        .stdout(predicate::str::contains("An Unrelated Title"))
        .stdout(predicate::str::contains("doi_10.1234_researcher"));
}

#[test]
fn search_returns_no_match_for_unknown_query() {
    let (_dir_guard, root) = seeded_store();

    let assert = doiget(&root)
        .args(["search", "ZZZ-no-such-token"])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone())
        .expect("doiget search stdout was not UTF-8");
    // Header only, no data rows.
    assert_eq!(
        stdout, "safekey\tyear\ttitle\tfetched_at\n",
        "no-match search should produce header-only stdout, got:\n{stdout}"
    );
}

#[test]
fn search_rejects_empty_query() {
    let (_dir_guard, root) = seeded_store();

    doiget(&root)
        .args(["search", ""])
        .assert()
        .failure()
        .stderr(predicate::str::contains("search query is empty"));
}

// ---- #204 JSON-mode coverage --------------------------------------------

#[test]
fn search_json_emits_array_of_entries() {
    let (_dir_guard, root) = seeded_store();

    let out = doiget(&root)
        // The #203 helper pins DOIGET_MODE=human; override per-test.
        .env("DOIGET_MODE", "json")
        .args(["search", "quantum"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let s = String::from_utf8(out).expect("search JSON stdout utf-8");
    let v: serde_json::Value = serde_json::from_str(&s).expect("search JSON parses");
    let arr = v.as_array().expect("search JSON is an array");
    assert!(!arr.is_empty(), "seeded query should match >=1 entry");
    for entry in arr {
        // EntryInfo schema (#204): {safekey, title, year, fetched_at}.
        assert!(entry["safekey"].is_string(), "safekey is a string");
        assert!(entry["title"].is_string(), "title is a string");
    }
}
