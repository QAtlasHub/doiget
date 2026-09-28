//! `doiget missing` (#607), offline: the store and local-file halves need
//! no network, so they are pinned here; how a lookup's answer becomes a
//! status is `commands::missing::tests`, and the lookup itself the resolver's.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use assert_cmd::Command;
use camino::Utf8PathBuf;
use predicates::prelude::*;
use tempfile::TempDir;

use doiget_core::store::{FsStore, Metadata, Store};
use doiget_core::{Doi, Ref};

fn setup() -> (TempDir, Utf8PathBuf) {
    let dir = TempDir::new().expect("tempdir");
    let base = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("utf-8");
    let root = base.join("papers");
    let store = FsStore::new(root.clone()).expect("store");
    // One entry with its PDF in the store.
    let doi = Doi::parse("10.1103/PhysRevB.48.10345").expect("doi");
    let key = Ref::Doi(doi.clone()).safekey();
    let pdf = base.join("staged.pdf");
    std::fs::write(&pdf, b"%PDF-1.4\n").expect("pdf");
    let m = Metadata {
        schema_version: "1.0".into(),
        title: "Density-matrix algorithms".into(),
        doi: Some(doi),
        year: Some(1993),
        ..Metadata::default()
    };
    store.write(&key, &m, Some(&pdf)).expect("seed");
    std::fs::create_dir_all(base.join("refs")).expect("refs");
    std::fs::write(base.join("refs/hartree1928.pdf"), b"%PDF-1.4\n").expect("local");
    std::fs::write(
        base.join("refs.bib"),
        "@article{white1993, title={x}, doi={10.1103/PhysRevB.48.10345}, year={1993}}\n\
         @article{hartree1928, title={y}, doi={10.1017/S0305004100011919}, year={1928}}\n\
         @article{fock1930, title={z}, doi={10.1007/BF01340294}, year={1930}}\n\
         @article{nokey, title={no id}, year={1999}}\n",
    )
    .expect("bib");
    (dir, base)
}

fn doiget(base: &Utf8PathBuf) -> Command {
    let mut cmd = Command::cargo_bin("doiget").expect("binary");
    cmd.current_dir(base)
        .env("DOIGET_STORE_ROOT", base.join("papers").as_str())
        .env("HOME", base.as_str())
        .env("XDG_CONFIG_HOME", base.as_str());
    cmd
}

#[test]
fn missing_offline_reports_each_entry_and_exits_with_the_missing_count() {
    let (_dir, base) = setup();
    let out = doiget(&base)
        .args(["--mode", "json", "missing", "refs.bib", "--offline"])
        .args(["--path-pattern", "refs/{key}.pdf"])
        .assert()
        .code(2)
        .get_output()
        .stdout
        .clone();
    let rows: Vec<serde_json::Value> = String::from_utf8(out)
        .expect("utf-8")
        .lines()
        .map(|l| serde_json::from_str(l).expect("json line"))
        .collect();
    let status = |k: &str| {
        rows.iter()
            .find(|r| r["entry_key"] == k)
            .map(|r| r["status"].as_str().unwrap().to_string())
    };
    assert_eq!(status("white1993").as_deref(), Some("in_store"));
    assert_eq!(status("hartree1928").as_deref(), Some("local_file"));
    assert_eq!(status("fock1930").as_deref(), Some("not_in_store"));
    assert_eq!(status("nokey").as_deref(), Some("unsupported"));
    let fock = rows.iter().find(|r| r["entry_key"] == "fock1930").unwrap();
    assert_eq!(fock["expected_path"], "refs/fock1930.pdf");
}

#[test]
fn missing_human_output_is_printed_on_a_non_tty_like_the_other_artifacts() {
    let (_dir, base) = setup();
    doiget(&base)
        .args(["missing", "refs.bib", "--offline"])
        .assert()
        .code(3)
        .stdout(predicate::str::contains("not_in_store"))
        .stdout(predicate::str::contains("fock1930"))
        .stdout(predicate::str::contains("white1993").not())
        .stderr(predicate::str::contains("missing: 3 of 4 entries"));
}

#[test]
fn missing_with_nothing_missing_exits_zero() {
    let (_dir, base) = setup();
    std::fs::write(
        base.join("one.bib"),
        "@article{white1993, title={x}, doi={10.1103/PhysRevB.48.10345}, year={1993}}\n",
    )
    .expect("bib");
    doiget(&base)
        .args(["missing", "one.bib", "--offline"])
        .assert()
        .success();
}

fn json_rows(out: &[u8]) -> Vec<serde_json::Value> {
    String::from_utf8(out.to_vec())
        .expect("utf-8")
        .lines()
        .map(|l| serde_json::from_str(l).expect("json line"))
        .collect()
}

#[test]
fn an_entry_with_no_identifier_is_found_by_its_key_on_disk() {
    let (_dir, base) = setup();
    std::fs::write(base.join("refs/nokey.pdf"), b"%PDF-1.4\n").expect("local");
    let out = doiget(&base)
        .args(["--mode", "json", "missing", "refs.bib", "--offline"])
        .args(["--path-pattern", "refs/{key}.pdf"])
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();
    let rows = json_rows(&out);
    let nokey = rows.iter().find(|r| r["entry_key"] == "nokey").unwrap();
    assert_eq!(nokey["status"], "local_file");
    assert_eq!(nokey["expected_path"], "refs/nokey.pdf");
}

#[test]
fn the_safekey_placeholder_names_the_store_key() {
    let (_dir, base) = setup();
    let out = doiget(&base)
        .args(["--mode", "json", "missing", "refs.bib", "--offline"])
        .args(["--path-pattern", "dl/{safekey}.pdf"])
        .assert()
        .code(3)
        .get_output()
        .stdout
        .clone();
    let rows = json_rows(&out);
    let fock = rows.iter().find(|r| r["entry_key"] == "fock1930").unwrap();
    let safekey = Ref::parse("10.1007/BF01340294").unwrap().safekey();
    assert_eq!(
        fock["expected_path"],
        format!("dl/{}.pdf", safekey.as_str())
    );
    // No identifier, no safekey: the pattern cannot name that entry's file.
    let nokey = rows.iter().find(|r| r["entry_key"] == "nokey").unwrap();
    assert!(nokey.get("expected_path").is_none());
}

#[test]
fn an_unreadable_reference_file_exits_2() {
    let (_dir, base) = setup();
    doiget(&base)
        .args(["missing", "absent.bib", "--offline"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("failed to read reference file"));
}

#[test]
fn the_exit_code_is_capped_at_255() {
    let (_dir, base) = setup();
    let bib: String = (0..300)
        .map(|i| format!("@article{{e{i}, title={{t}}, year={{2000}}}}\n"))
        .collect();
    std::fs::write(base.join("many.bib"), bib).expect("bib");
    doiget(&base)
        .args(["missing", "many.bib", "--offline"])
        .assert()
        .code(255)
        .stderr(predicate::str::contains("missing: 300 of 300"));
}
