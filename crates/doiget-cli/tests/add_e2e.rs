//! `doiget add` (#606) through the real binary, against a store that already
//! holds the entry's metadata, so no network is needed.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use assert_cmd::Command;
use camino::Utf8PathBuf;
use predicates::prelude::*;
use tempfile::TempDir;

use doiget_core::store::{FsStore, Metadata, Store};
use doiget_core::{Doi, Ref};

const DOI: &str = "10.1007/BF01340294";

fn setup() -> (TempDir, Utf8PathBuf) {
    let td = TempDir::new().expect("tempdir");
    let base = Utf8PathBuf::from_path_buf(td.path().to_path_buf()).expect("utf-8");
    let store = FsStore::new(base.join("papers")).expect("store");
    for doi in [DOI, "10.1103/PhysRev.34.1293"] {
        let d = Doi::parse(doi).expect("doi");
        let m = Metadata {
            schema_version: "1.0".into(),
            title: format!("Paper {doi}"),
            doi: Some(d.clone()),
            ..Metadata::default()
        };
        store.write(&Ref::Doi(d).safekey(), &m, None).expect("seed");
    }
    std::fs::create_dir_all(base.join("dl")).expect("dl");
    for name in [
        "BF01340294.pdf",
        "BF01397394.pdf",
        "PhysRev.34.1293.pdf",
        "notes.pdf",
    ] {
        std::fs::write(base.join("dl").join(name), b"%PDF-1.4\nbytes\n").expect("pdf");
    }
    std::fs::write(base.join("dl/fake.pdf"), b"<!doctype html>").expect("html");
    (td, base)
}

fn doiget(base: &Utf8PathBuf) -> Command {
    let mut cmd = Command::cargo_bin("doiget").expect("binary");
    cmd.current_dir(base)
        .env("HOME", base.as_str())
        .env("XDG_CONFIG_HOME", base.as_str())
        .env("DOIGET_STORE_ROOT", base.join("papers").as_str())
        .env("DOIGET_LOG_PATH", base.join("log.jsonl").as_str());
    cmd
}

#[test]
fn add_records_a_user_supplied_pdf_and_logs_no_source_path() {
    let (_td, base) = setup();
    doiget(&base)
        .args(["add", DOI, "dl/BF01340294.pdf"])
        .assert()
        .success()
        .stderr(predicate::str::contains("user-supplied"));
    assert!(base.join("papers/doi_10.1007_BF01340294.pdf").exists());
    assert!(
        base.join("dl/BF01340294.pdf").exists(),
        "the original is copied, not moved"
    );
    let toml =
        std::fs::read_to_string(base.join("papers/.metadata/doi_10.1007_BF01340294.toml")).unwrap();
    assert!(toml.contains("origin = \"user-supplied\""), "{toml}");
    assert!(toml.contains("license = \"unknown\""), "{toml}");
    assert!(toml.contains("source = \"user\""), "{toml}");
    let log = std::fs::read_to_string(base.join("log.jsonl")).unwrap();
    let row = log
        .lines()
        .find(|l| l.contains("store_write"))
        .expect("store_write row");
    assert!(row.contains("\"capability\":\"user-supplied\""), "{row}");
    assert!(
        !row.contains("/dl/"),
        "the original path must not be logged: {row}"
    );

    // A second add is refused unless --force.
    doiget(&base)
        .args(["add", DOI, "dl/BF01340294.pdf"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("--force"));
}

#[test]
fn add_refuses_a_file_named_for_another_work_or_not_a_pdf() {
    let (_td, base) = setup();
    doiget(&base)
        .args(["add", DOI, "dl/BF01397394.pdf"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("id of a different work"));
    doiget(&base)
        .args(["add", DOI, "dl/fake.pdf"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("not a PDF"));
    // A name that makes no claim is fine.
    doiget(&base)
        .args(["add", DOI, "dl/notes.pdf"])
        .assert()
        .success();
}

#[test]
fn add_from_dir_plans_then_applies_by_file_name() {
    let (_td, base) = setup();
    doiget(&base)
        .args(["add", "--from-dir", "dl"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "would add dl/BF01340294.pdf -> 10.1007/BF01340294",
        ))
        .stderr(predicate::str::contains(
            "would add dl/PhysRev.34.1293.pdf -> 10.1103/PhysRev.34.1293",
        ))
        .stderr(predicate::str::contains("unmatched: dl/notes.pdf"));
    assert!(
        !base.join("papers/doi_10.1007_BF01340294.pdf").exists(),
        "a dry run writes nothing"
    );
    doiget(&base)
        .args(["add", "--from-dir", "dl", "--apply"])
        .assert()
        .success()
        .stderr(predicate::str::contains("2 matched, "));
    assert!(base.join("papers/doi_10.1007_BF01340294.pdf").exists());
    assert!(base.join("papers/doi_10.1103_PhysRev.34.1293.pdf").exists());
}
