//! End-to-end tests for `doiget bib`.
//!
//! Strategy mirrors `tests/info_list_recent_e2e.rs`: seed a `FsStore` rooted
//! at a per-test `tempfile::TempDir`, then invoke the freshly-built `doiget`
//! binary as a subprocess with `DOIGET_STORE_ROOT` pointing at that
//! tempdir. `HOME` and `USERPROFILE` are also overridden — belt-and-
//! suspenders against any fallback codepath leaking the developer's real
//! `~/papers` into a test.
//!
//! Each test owns its own tempdir, so the suite runs in parallel without
//! `serial_test` coordination. No assertion is made about the byte-for-
//! byte stdout layout outside of the field lines emitted by `bib::run` —
//! that lets the underlying serializer evolve without breaking these
//! tests.

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
use doiget_core::{Doi, Ref, SCHEMA_VERSION};

/// Convert a `TempDir`'s path to a `Utf8PathBuf` so it can drive
/// `FsStore::new` (which is camino-only). Panics if the temp path is not
/// UTF-8 — on every platform we test, the system temp dir is ASCII.
fn utf8_path(dir: &TempDir) -> Utf8PathBuf {
    Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("temp dir path must be UTF-8")
}

/// Build a fixture for `10.1234/example` with the given Crossref `type_`.
///
/// All other binding fields match the spec block in the implementation
/// task: title `"Quantum Stuff"`, two authors, year 2026, journal
/// `"Phys Rev X"`, publisher `"APS"`, ISSN `"2160-3308"`. The `[doiget]`
/// extension table is populated with a fixed RFC3339-shaped fetched_at so
/// the metadata round-trips cleanly through `FsStore::write` /
/// `Store::read`.
fn fixture(type_: Option<&str>) -> Metadata {
    Metadata {
        schema_version: SCHEMA_VERSION.to_string(),
        title: "Quantum Stuff".to_string(),
        authors: vec!["Alice Researcher".to_string(), "Bob Coauthor".to_string()],
        year: Some(2026),
        doi: Some(Doi::parse("10.1234/example").expect("valid DOI")),
        arxiv_id: None,
        abstract_: None,
        venue: Some("Phys Rev X".to_string()),
        publisher: Some("APS".to_string()),
        issn: Some("2160-3308".to_string()),
        isbn: None,
        type_: type_.map(str::to_string),
        keywords: vec![],
        url: None,
        pdf_path: None,
        doiget: Some(DoigetExtension {
            fetched_at: chrono::Utc
                .with_ymd_and_hms(2026, 5, 6, 12, 0, 0)
                .single()
                .expect("valid timestamp"),
            source: "unpaywall".to_string(),
            license: "CC-BY-4.0".to_string(),
            size_bytes: 1234,
            mcp_call_id: None,
        }),
        other: BTreeMap::new(),
    }
}

/// Seed a temp store with a single `10.1234/example` entry of the given
/// Crossref `type_`. Returns the (TempDir guard, store-root) pair; the
/// guard MUST outlive the test — dropping it deletes the tempdir.
fn seeded_store(type_: Option<&str>) -> (TempDir, Utf8PathBuf) {
    let dir = TempDir::new().expect("tempdir");
    let root = utf8_path(&dir).join("papers");
    let store = FsStore::new(root.clone()).expect("FsStore::new");

    let ref_ = Ref::Doi(Doi::parse("10.1234/example").expect("valid DOI"));
    let safekey = ref_.safekey();
    let metadata = fixture(type_);
    store
        .write(&safekey, &metadata, None)
        .expect("seed bib entry");

    (dir, root)
}

/// Configure a `doiget` subprocess to use `root` as its store. Sets
/// `DOIGET_STORE_ROOT` (the primary resolution hook) and clears
/// `HOME`/`USERPROFILE` so any fallback in `resolve_store_root` cannot
/// leak the developer's real home directory into the test.
fn doiget(root: &Utf8PathBuf) -> Command {
    let mut cmd = Command::cargo_bin("doiget").expect("locate doiget binary");
    cmd.env("DOIGET_STORE_ROOT", root.as_str())
        .env("HOME", root.as_str())
        .env("USERPROFILE", root.as_str());
    cmd
}

#[test]
fn bib_emits_article_for_journal_article_type() {
    let (_dir_guard, root) = seeded_store(Some("journal-article"));

    doiget(&root)
        .args(["bib", "doi:10.1234/example"])
        .assert()
        .success()
        .stdout(predicate::str::contains("@article{doi_10.1234_example,"))
        .stdout(predicate::str::contains("title      = {Quantum Stuff},"))
        .stdout(predicate::str::contains(
            "author     = {Alice Researcher and Bob Coauthor},",
        ))
        .stdout(predicate::str::contains("year       = {2026},"))
        .stdout(predicate::str::contains("doi        = {10.1234/example},"))
        .stdout(predicate::str::contains("journal    = {Phys Rev X},"))
        .stdout(predicate::str::contains("publisher  = {APS},"))
        .stdout(predicate::str::contains("issn       = {2160-3308},"))
        // Closing brace on its own line, terminated by a newline.
        .stdout(predicate::str::contains("\n}\n"));
}

#[test]
fn bib_fails_for_missing_entry() {
    // Empty store: no `.metadata/*.toml` files at all.
    let dir = TempDir::new().expect("tempdir");
    let root = utf8_path(&dir).join("papers");
    FsStore::new(root.clone()).expect("FsStore::new");

    doiget(&root)
        .args(["bib", "doi:10.9999/missing"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no entry for"));
}

#[test]
fn bib_emits_misc_for_missing_type() {
    let (_dir_guard, root) = seeded_store(None);

    doiget(&root)
        .args(["bib", "doi:10.1234/example"])
        .assert()
        .success()
        .stdout(predicate::str::contains("@misc{doi_10.1234_example,"));
}
