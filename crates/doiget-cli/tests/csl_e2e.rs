//! End-to-end tests for `doiget csl <ref>`.
//!
//! Strategy mirrors `info_list_recent_e2e.rs`: seed an `FsStore` rooted at
//! a per-test `tempfile::TempDir`, then drive the freshly-built `doiget`
//! binary as a subprocess with `DOIGET_STORE_ROOT` pointing at that
//! tempdir. This keeps the real `~/papers/` untouched and lets the tests
//! run in parallel without `serial_test` coordination.
//!
//! The assertions are SHAPE-LEVEL — we parse stdout as `serde_json::Value`
//! and probe individual fields rather than doing a byte-for-byte string
//! match. That insulates us from cosmetic changes (whitespace, key order)
//! in `serde_json::to_string_pretty`.

// Tests are panic-on-failure by design; relax the workspace-wide lints
// that ban `expect`/`unwrap`/`panic` in production code.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeMap;

use assert_cmd::Command;
use camino::Utf8PathBuf;
use chrono::TimeZone;
use predicates::prelude::*;
use serde_json::Value;
use tempfile::TempDir;

use doiget_core::store::{DoigetExtension, FsStore, Metadata, Store};
use doiget_core::{Doi, Safekey, SCHEMA_VERSION};

/// Convert a `TempDir`'s path to a `Utf8PathBuf` so it can drive
/// `FsStore::new` (which is camino-only). Panics if the temp path is not
/// UTF-8 — on every platform we test, the system temp dir is ASCII.
fn utf8_path(dir: &TempDir) -> Utf8PathBuf {
    Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("temp dir path must be UTF-8")
}

/// Configure a `doiget` subprocess to use `root` as its store. Sets
/// `DOIGET_STORE_ROOT` (the primary resolution hook) and clears
/// `HOME` / `USERPROFILE` to belt-and-suspenders against any fallback
/// codepath leaking the developer's real home directory into the test.
fn doiget(root: &Utf8PathBuf) -> Command {
    let mut cmd = Command::cargo_bin("doiget").expect("locate doiget binary");
    cmd.env("DOIGET_STORE_ROOT", root.as_str())
        .env("HOME", root.as_str())
        .env("USERPROFILE", root.as_str());
    cmd
}

/// Build the journal-article fixture used by the happy-path test:
/// two authors in `Given Family` form, full venue / publisher / issn.
fn journal_article_fixture() -> (Safekey, Metadata) {
    let doi = "10.1234/example";
    let ref_ = doiget_core::Ref::Doi(Doi::parse(doi).expect("valid DOI"));
    let safekey = ref_.safekey();
    let m = Metadata {
        schema_version: SCHEMA_VERSION.to_string(),
        title: "Quantum Stuff".to_string(),
        authors: vec!["Alice Researcher".to_string(), "Bob Coauthor".to_string()],
        year: Some(2026),
        doi: Some(Doi::parse(doi).expect("valid DOI")),
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
        type_: Some("journal-article".to_string()),
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
    };
    (safekey, m)
}

/// Build a fixture with `Family, Given` form authors and no Crossref
/// type — exercises both the comma-form name parser branch and the
/// `manuscript` type fallback.
fn comma_form_fixture() -> (Safekey, Metadata) {
    let doi = "10.5678/comma";
    let ref_ = doiget_core::Ref::Doi(Doi::parse(doi).expect("valid DOI"));
    let safekey = ref_.safekey();
    let m = Metadata {
        schema_version: SCHEMA_VERSION.to_string(),
        title: "Comma Title".to_string(),
        authors: vec!["Smith, John".to_string()],
        year: Some(2025),
        doi: Some(Doi::parse(doi).expect("valid DOI")),
        arxiv_id: None,
        arxiv_categories: vec![],
        abstract_: None,
        venue: None,
        volume: None,
        issue: None,
        pages: None,
        publisher: None,
        issn: None,
        isbn: None,
        // No `type_` — expected CSL type falls back to "manuscript".
        type_: None,
        keywords: vec![],
        url: None,
        pdf_path: None,
        doiget: Some(DoigetExtension {
            fetched_at: chrono::Utc
                .with_ymd_and_hms(2025, 1, 1, 0, 0, 0)
                .single()
                .expect("valid timestamp"),
            source: "unpaywall".to_string(),
            license: "unknown".to_string(),
            oa_status: None,
            size_bytes: 0,
            mcp_call_id: None,
            tags: Vec::new(),
            collections: Vec::new(),
            annotation: None,
        }),
        other: BTreeMap::new(),
    };
    (safekey, m)
}

/// Seed a temp store with the journal-article fixture and return
/// `(TempDir, store_root)`. The `TempDir` MUST stay in scope for the
/// duration of the test — dropping it deletes the tempdir.
fn seeded_store_journal() -> (TempDir, Utf8PathBuf) {
    let dir = TempDir::new().expect("tempdir");
    let root = utf8_path(&dir).join("papers");
    let store = FsStore::new(root.clone()).expect("FsStore::new");
    let (k, m) = journal_article_fixture();
    store.write(&k, &m, None).expect("seed entry");
    (dir, root)
}

/// Seed a temp store with the comma-form fixture.
fn seeded_store_comma() -> (TempDir, Utf8PathBuf) {
    let dir = TempDir::new().expect("tempdir");
    let root = utf8_path(&dir).join("papers");
    let store = FsStore::new(root.clone()).expect("FsStore::new");
    let (k, m) = comma_form_fixture();
    store.write(&k, &m, None).expect("seed entry");
    (dir, root)
}

#[test]
fn csl_emits_array_with_expected_fields_for_journal_article() {
    let (_dir_guard, root) = seeded_store_journal();

    let assert = doiget(&root)
        .args(["csl", "doi:10.1234/example"])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone())
        .expect("doiget csl stdout was not UTF-8");
    let value: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("doiget csl stdout was not valid JSON: {e}\nstdout:\n{stdout}"));

    let array = value
        .as_array()
        .unwrap_or_else(|| panic!("expected top-level JSON array, got: {value}"));
    assert_eq!(array.len(), 1, "expected single-entry array, got: {value}");
    let item = &array[0];

    assert_eq!(item["id"], Value::String("doi_10.1234_example".into()));
    assert_eq!(item["type"], Value::String("article-journal".into()));
    assert_eq!(item["title"], Value::String("Quantum Stuff".into()));
    assert_eq!(item["DOI"], Value::String("10.1234/example".into()));
    assert_eq!(item["container-title"], Value::String("Phys Rev X".into()));
    assert_eq!(item["publisher"], Value::String("APS".into()));
    assert_eq!(item["ISSN"], Value::String("2160-3308".into()));

    // `issued.date-parts == [[2026]]`.
    let date_parts = &item["issued"]["date-parts"];
    assert_eq!(
        date_parts,
        &serde_json::json!([[2026]]),
        "issued.date-parts shape mismatch: {date_parts}"
    );

    // Authors: `Given Family` form splits on last whitespace.
    let authors = item["author"]
        .as_array()
        .unwrap_or_else(|| panic!("expected author array, got: {}", item["author"]));
    assert_eq!(authors.len(), 2, "expected 2 authors, got: {authors:?}");
    assert_eq!(
        authors[0],
        serde_json::json!({"family": "Researcher", "given": "Alice"}),
        "author[0] mismatch"
    );
    assert_eq!(
        authors[1],
        serde_json::json!({"family": "Coauthor", "given": "Bob"}),
        "author[1] mismatch"
    );
}

#[test]
fn csl_parses_comma_form_author_and_falls_back_to_manuscript_type() {
    let (_dir_guard, root) = seeded_store_comma();

    let assert = doiget(&root)
        .args(["csl", "doi:10.5678/comma"])
        .assert()
        .success();

    let stdout = String::from_utf8(assert.get_output().stdout.clone())
        .expect("doiget csl stdout was not UTF-8");
    let value: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("doiget csl stdout was not valid JSON: {e}\nstdout:\n{stdout}"));

    let item = &value
        .as_array()
        .unwrap_or_else(|| panic!("expected top-level JSON array, got: {value}"))[0];

    // No `type_` on the metadata → CSL type falls back to "manuscript".
    assert_eq!(item["type"], Value::String("manuscript".into()));

    // Optional fields with `None` on the metadata side are omitted from
    // the JSON object — `Value::get` returns None for missing keys.
    assert!(
        item.get("container-title").is_none(),
        "container-title should be omitted when venue is None; got: {item}"
    );
    assert!(
        item.get("publisher").is_none(),
        "publisher should be omitted when publisher is None; got: {item}"
    );
    assert!(
        item.get("ISSN").is_none(),
        "ISSN should be omitted when issn is None; got: {item}"
    );

    // `Family, Given` form splits on the first comma.
    let authors = item["author"]
        .as_array()
        .unwrap_or_else(|| panic!("expected author array, got: {}", item["author"]));
    assert_eq!(authors.len(), 1);
    assert_eq!(
        authors[0],
        serde_json::json!({"family": "Smith", "given": "John"}),
        "author[0] mismatch for comma-form name"
    );
}

#[test]
fn csl_fails_for_missing_entry() {
    // Empty store: `csl` for a ref that was never written must fail
    // with a non-zero exit and a recognisable error on stderr, so shell
    // pipelines can distinguish "not in store" from "empty entry".
    let dir = TempDir::new().expect("tempdir");
    let root = utf8_path(&dir).join("papers");
    FsStore::new(root.clone()).expect("FsStore::new");

    doiget(&root)
        .args(["csl", "doi:10.9999/missing"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no entry for"));
}

#[test]
fn csl_fails_for_invalid_ref_string() {
    // Garbage input must short-circuit at `Ref::parse` and never touch
    // the filesystem. We assert non-zero exit + a hint mentioning the
    // bad input.
    let dir = TempDir::new().expect("tempdir");
    let root = utf8_path(&dir).join("papers");
    FsStore::new(root.clone()).expect("FsStore::new");

    doiget(&root)
        .args(["csl", "not-a-ref"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid ref"));
}

/// Seed a temp store with BOTH the journal-article and comma-form
/// fixtures (two distinct DOIs / safekeys).
fn seeded_store_both() -> (TempDir, Utf8PathBuf) {
    let dir = TempDir::new().expect("tempdir");
    let root = utf8_path(&dir).join("papers");
    let store = FsStore::new(root.clone()).expect("FsStore::new");
    let (k1, m1) = journal_article_fixture();
    let (k2, m2) = comma_form_fixture();
    store.write(&k1, &m1, None).expect("seed entry 1");
    store.write(&k2, &m2, None).expect("seed entry 2");
    (dir, root)
}

#[test]
fn csl_all_exports_every_store_entry_in_one_array() {
    // Issue #305 (CSL parity): `csl --all` emits one flat CSL JSON array
    // of every store entry — no ref argument, no network.
    let (_dir_guard, root) = seeded_store_both();

    let assert = doiget(&root).args(["csl", "--all"]).assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("stdout utf-8");
    let value: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("not valid JSON: {e}\nstdout:\n{stdout}"));
    let array = value
        .as_array()
        .unwrap_or_else(|| panic!("expected a JSON array, got: {value}"));
    assert_eq!(array.len(), 2, "one item per store entry: {value}");

    let ids: Vec<&str> = array.iter().filter_map(|i| i["id"].as_str()).collect();
    assert!(ids.contains(&"doi_10.1234_example"), "ids: {ids:?}");
    assert!(ids.contains(&"doi_10.5678_comma"), "ids: {ids:?}");
}

#[test]
fn csl_from_file_renders_present_and_exits_nonzero_on_missing() {
    // Issue #305: `csl --from-file` renders the present refs into one array
    // and exits non-zero when a requested ref is missing (batch convention).
    let (dir_guard, root) = seeded_store_journal();
    let refs = utf8_path(&dir_guard).join("refs.txt");
    std::fs::write(
        refs.as_std_path(),
        "doi:10.1234/example\ndoi:10.9999/missing\n",
    )
    .expect("write refs file");

    let assert = doiget(&root)
        .args(["csl", "--from-file", refs.as_str()])
        .assert()
        .failure(); // one ref missing → non-zero exit
    let out = assert.get_output();
    let stdout = String::from_utf8(out.stdout.clone()).expect("stdout utf-8");
    let value: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("not valid JSON: {e}\nstdout:\n{stdout}"));
    assert_eq!(
        value.as_array().map(|a| a.len()),
        Some(1),
        "only the present ref renders: {value}"
    );
    let stderr = String::from_utf8(out.stderr.clone()).expect("stderr utf-8");
    // Full summary line (not the loose "1 missing" substring): 1 rendered,
    // 1 skipped, so a wording or count regression is caught (review #318).
    assert!(
        stderr.contains("csl --from-file: exported 1 entries, 1 missing"),
        "stderr digest: {stderr}"
    );
}

#[test]
fn csl_no_selector_is_a_usage_error() {
    let (_dir_guard, root) = seeded_store_journal();
    doiget(&root)
        .args(["csl"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("specify a ref"));
}

#[test]
fn csl_all_on_empty_store_emits_empty_array_exit_zero() {
    // Review #318: parity with `bib --all` on an empty store — `csl --all`
    // emits a valid empty CSL array `[]` and exits 0 (nothing to export is
    // not a failure), with a count note on stderr.
    let dir = TempDir::new().expect("tempdir");
    let root = utf8_path(&dir).join("papers");
    FsStore::new(root.clone()).expect("FsStore::new");

    let assert = doiget(&root).args(["csl", "--all"]).assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("stdout utf-8");
    let value: Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("not JSON: {e}\n{stdout}"));
    assert_eq!(
        value.as_array().map(|a| a.len()),
        Some(0),
        "empty array: {value}"
    );
    assert!(
        String::from_utf8(assert.get_output().stderr.clone())
            .unwrap()
            .contains("exported 0 entries"),
        "stderr count note"
    );
}
