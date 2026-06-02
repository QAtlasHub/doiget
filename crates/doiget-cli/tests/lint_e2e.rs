//! End-to-end tests for `doiget lint <path>`.
//!
//! `lint` touches no network and no store — it only reads the given file —
//! so these tests need no wiremock or temp HOME, just a fixture `.bib`.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use tempfile::TempDir;

/// Write `content` to `<dir>/refs.bib` and return the path string.
fn write_bib(dir: &TempDir, content: &str) -> String {
    let path = dir.path().join("refs.bib");
    std::fs::write(&path, content).expect("write bib fixture");
    path.to_str().expect("utf-8 path").to_string()
}

fn doiget() -> Command {
    Command::cargo_bin("doiget").expect("locate doiget binary")
}

#[test]
fn lint_clean_article_has_no_findings() {
    let dir = TempDir::new().expect("tempdir");
    let bib = write_bib(
        &dir,
        "@article{ok, author={A. Author}, title={A Clean Title}, journal={Phys. Rev.}, year={2020}}",
    );
    doiget().args(["lint", &bib]).assert().success();
}

#[test]
fn lint_missing_fields_warns_but_passes_by_default() {
    let dir = TempDir::new().expect("tempdir");
    // @article with no journal and no year → two missing-field warnings.
    let bib = write_bib(
        &dir,
        "@article{miss, author={A}, title={No Journal No Year}}",
    );
    doiget().args(["lint", &bib]).assert().success().stdout(
        contains("\"rule\":\"missing_required_field\"").and(contains("\"severity\":\"warning\"")),
    );
}

#[test]
fn lint_missing_fields_strict_fails() {
    let dir = TempDir::new().expect("tempdir");
    let bib = write_bib(
        &dir,
        "@article{miss, author={A}, title={No Journal No Year}}",
    );
    doiget()
        .args(["lint", &bib, "--strict"])
        .assert()
        .failure()
        .stdout(contains("\"rule\":\"missing_required_field\""));
}

#[test]
fn lint_empty_field_warns() {
    let dir = TempDir::new().expect("tempdir");
    let bib = write_bib(
        &dir,
        "@article{e, author={A}, title={T}, journal={ }, year={2020}}",
    );
    doiget()
        .args(["lint", &bib])
        .assert()
        .success()
        .stdout(contains("\"rule\":\"empty_field\""));
}

#[test]
fn lint_flags_display_math_title_but_not_inline_math() {
    // `$$ ... $$` display math is a hazard for downstream renderers; plain
    // inline `$...$` must NOT be flagged (the hand-edited-maths case).
    let dir = TempDir::new().expect("tempdir");
    let hazard = write_bib(
        &dir,
        "@article{haz, author={A}, title={Bad $$\\mathrm{T}$$ math}, journal={J}, year={2001}}",
    );
    doiget()
        .args(["lint", &hazard])
        .assert()
        .success()
        .stdout(contains("\"rule\":\"title_math_hazard\""));

    let dir2 = TempDir::new().expect("tempdir");
    let inline = write_bib(
        &dir2,
        "@article{inl, author={A}, title={Good $T\\bar{T}$ inline}, journal={J}, year={2002}}",
    );
    doiget()
        .args(["lint", &inline])
        .assert()
        .success()
        .stdout(contains("title_math_hazard").not());
}

#[test]
fn lint_unparseable_file_is_a_parse_error() {
    // The biblatex parser rejects duplicate keys, which surfaces as a
    // `parse_error` (error severity) and fails the run.
    let dir = TempDir::new().expect("tempdir");
    let bib = write_bib(
        &dir,
        "@article{dup, title={One}}\n@book{dup, title={Two}, author={B}, publisher={P}, year={1999}}",
    );
    doiget()
        .args(["lint", &bib])
        .assert()
        .failure()
        .stdout(contains("\"rule\":\"parse_error\"").and(contains("\"severity\":\"error\"")));
}
