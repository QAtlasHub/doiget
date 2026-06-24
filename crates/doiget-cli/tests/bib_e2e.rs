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
        arxiv_categories: vec![],
        abstract_: None,
        venue: Some("Phys Rev X".to_string()),
        volume: None,
        issue: None,
        pages: None,
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
            oa_status: None,
            size_bytes: 1234,
            mcp_call_id: None,
            tags: Vec::new(),
            collections: Vec::new(),
            annotation: None,
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

/// Seed a `journal-article` entry for `doi` with the given `title` into the
/// store at `root` (which must already exist).
fn seed(root: &Utf8PathBuf, doi: &str, title: &str) {
    let store = FsStore::new(root.clone()).expect("FsStore::new");
    let ref_ = Ref::Doi(Doi::parse(doi).expect("valid DOI"));
    let mut m = fixture(Some("journal-article"));
    m.doi = Some(Doi::parse(doi).expect("valid DOI"));
    m.title = title.to_string();
    store.write(&ref_.safekey(), &m, None).expect("seed entry");
}

#[test]
fn bib_all_exports_every_store_entry() {
    // Issue #305: `bib --all` is a one-pass offline exporter of the whole
    // store — no ref argument, no network.
    let dir = TempDir::new().expect("tempdir");
    let root = utf8_path(&dir).join("papers");
    FsStore::new(root.clone()).expect("FsStore::new");
    seed(&root, "10.1234/example", "First Paper");
    seed(&root, "10.5678/another", "Second Paper");

    doiget(&root)
        .args(["bib", "--all"])
        .assert()
        .success()
        .stdout(predicate::str::contains("@article{doi_10.1234_example,"))
        .stdout(predicate::str::contains("@article{doi_10.5678_another,"))
        .stdout(predicate::str::contains("First Paper"))
        .stdout(predicate::str::contains("Second Paper"));
}

#[test]
fn bib_all_on_empty_store_succeeds_with_note() {
    // An empty store is "nothing to export", not a failure (no ref was
    // requested) — exit 0 with a stderr note, empty stdout.
    let dir = TempDir::new().expect("tempdir");
    let root = utf8_path(&dir).join("papers");
    FsStore::new(root.clone()).expect("FsStore::new");

    doiget(&root)
        .args(["bib", "--all"])
        .assert()
        .success()
        .stderr(predicate::str::contains("store is empty"));
}

#[test]
fn bib_from_file_renders_present_and_exits_nonzero_on_missing() {
    // Issue #305: `bib --from-file` renders the refs present in the store
    // and SKIPS the missing ones — but exits non-zero so a script can tell
    // a complete export from a partial one (the #304 failure-count rule).
    let dir = TempDir::new().expect("tempdir");
    let root = utf8_path(&dir).join("papers");
    FsStore::new(root.clone()).expect("FsStore::new");
    seed(&root, "10.1234/example", "Present Paper");

    let refs = utf8_path(&dir).join("refs.txt");
    std::fs::write(
        refs.as_std_path(),
        "doi:10.1234/example\ndoi:10.9999/missing\n",
    )
    .expect("write refs file");

    doiget(&root)
        .args(["bib", "--from-file", refs.as_str()])
        .assert()
        .failure() // one ref missing → non-zero exit
        .stdout(predicate::str::contains("Present Paper"))
        // Full summary line: 1 of 2 refs rendered, 1 skipped. Asserting both
        // counts (not just the loose "1 missing" substring) pins the digest
        // so a wording or count regression is caught (review #318).
        .stderr(predicate::str::contains(
            "bib --from-file: exported 1 entries, 1 missing",
        ));
}

#[test]
fn bib_no_selector_is_a_usage_error() {
    let dir = TempDir::new().expect("tempdir");
    let root = utf8_path(&dir).join("papers");
    FsStore::new(root.clone()).expect("FsStore::new");

    doiget(&root)
        .args(["bib"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("specify a ref"));
}

#[test]
fn bib_offline_via_cite_renders_from_store() {
    // `cite --offline <ref>` renders the stored entry with no network
    // (issue #305): an already-fetched ref always cites.
    let (_dir_guard, root) = seeded_store(Some("journal-article"));

    doiget(&root)
        .args(["cite", "--offline", "doi:10.1234/example"])
        .assert()
        .success()
        .stdout(predicate::str::contains("@article{doi_10.1234_example,"))
        .stdout(predicate::str::contains("title      = {Quantum Stuff},"));
}

#[test]
fn cite_offline_missing_entry_fails() {
    let dir = TempDir::new().expect("tempdir");
    let root = utf8_path(&dir).join("papers");
    FsStore::new(root.clone()).expect("FsStore::new");

    doiget(&root)
        .args(["cite", "--offline", "doi:10.9999/missing"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no local store entry"));
}

#[test]
fn cite_auto_falls_back_to_store_when_live_resolve_fails() {
    // Review #318 / #305: `cite <ref>` (NO --offline) whose live resolve
    // fails still cites from the local store, with a `note:` on stderr — an
    // already-fetched ref is never lost to a network flake.
    let (_dir_guard, root) = seeded_store(Some("journal-article"));

    doiget(&root)
        // Closed loopback for every resolver leg → live resolve fails fast.
        .env("DOIGET_CROSSREF_BASE", "http://127.0.0.1:1/")
        .env("DOIGET_UNPAYWALL_BASE", "http://127.0.0.1:1/")
        .env("DOIGET_CONTACT_EMAIL", "test@example.com")
        .args(["cite", "doi:10.1234/example"])
        .assert()
        .success()
        .stdout(predicate::str::contains("@article{doi_10.1234_example,"))
        .stderr(predicate::str::contains("note: live resolve failed"));
}
