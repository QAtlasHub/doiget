//! Dry-run integration test for `doiget fetch <ref> --dry-run`
//! (ADR-0022).
//!
//! This is the binding success criterion for the dry-run posture of
//! ADR-0022 §1 / §3 / §5: a dry-run fetch
//!
//! - returns `Ok(())` immediately,
//! - never writes a PDF,
//! - never writes a metadata TOML,
//! - never appends (and never even creates) the provenance log file,
//! - never starts a wiremock server (i.e. no `DOIGET_*_BASE` overrides
//!   are needed because no HTTP traffic is attempted).
//!
//! ## Network purity
//!
//! Per the network-purity guard, this test makes NO outbound calls. It
//! also makes NO calls to wiremock — that is the whole point of the
//! dry-run mode.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use camino::{Utf8Path, Utf8PathBuf};
use doiget_cli::commands::fetch::{build_dry_run_envelope, build_fetch_plan, run_with_options};
use doiget_cli::commands::output::OutputMode;
use doiget_core::Ref;
use serial_test::serial;
use tempfile::TempDir;

mod common;
use common::env_guard::EnvGuard;

#[tokio::test]
#[serial]
async fn dry_run_fetch_doi_returns_ok_without_side_effects() {
    // Step 1: stage a clean tempdir for store + log paths, and clear
    // every base-URL override so a regression that accidentally falls
    // through to the live network path would surface as a DNS failure
    // (not a wiremock hit).
    let td = TempDir::new().expect("tempdir");
    let temp_root: Utf8PathBuf = Utf8Path::from_path(td.path())
        .expect("temp dir is utf-8")
        .to_path_buf();
    let store_root = temp_root.join("papers");
    let log_path = temp_root.join("log.jsonl");

    let env = EnvGuard::new(&[
        "DOIGET_STORE_ROOT",
        "DOIGET_LOG_PATH",
        "DOIGET_ARXIV_BASE",
        "DOIGET_CROSSREF_BASE",
        "DOIGET_UNPAYWALL_BASE",
        "DOIGET_OA_PUBLISHER_BASE",
        "DOIGET_CONTACT_EMAIL",
        "DOIGET_UNPAYWALL_EMAIL",
    ]);
    env.set("DOIGET_STORE_ROOT", store_root.as_str());
    env.set("DOIGET_LOG_PATH", log_path.as_str());

    // Step 2: invoke the orchestrator with dry_run=true. NO wiremock has
    // been started, so any accidental HTTP attempt would fail (DNS for
    // crossref/unpaywall/oa-publisher hosts is not stubbed). The fact
    // that this call returns Ok proves the dry-run path short-circuits
    // before any network call.
    let result = run_with_options("10.1234/foo".to_string(), true, None, OutputMode::Human).await;
    result.expect("dry-run fetch::run_with_options succeeds");

    // Step 3: assert the on-disk side-effects are empty. The store
    // directory and log file MUST NOT have been created by the
    // dry-run path (ADR-0022 §1 / §3 / §5 — "no network call, no file
    // write, and no provenance row appended").
    assert!(
        !log_path.exists(),
        "dry-run path must not create the provenance log file: {log_path}",
    );
    // The store root either does not exist OR exists but contains no
    // PDFs / no .metadata dir. We assert the safekey-derived PDF /
    // metadata paths do not exist.
    let pdf_path = store_root.join("doi_10.1234_foo.pdf");
    let toml_path = store_root.join(".metadata").join("doi_10.1234_foo.toml");
    assert!(
        !pdf_path.exists(),
        "dry-run path must not write the PDF: {pdf_path}",
    );
    assert!(
        !toml_path.exists(),
        "dry-run path must not write the metadata TOML: {toml_path}",
    );

    drop(env);
    drop(td);
}

#[tokio::test]
#[serial]
async fn dry_run_fetch_arxiv_returns_ok_without_side_effects() {
    // Mirror of the DOI test for the arXiv ref kind. Same posture: no
    // wiremock, no network, no writes, no log file.
    let td = TempDir::new().expect("tempdir");
    let temp_root: Utf8PathBuf = Utf8Path::from_path(td.path())
        .expect("temp dir is utf-8")
        .to_path_buf();
    let store_root = temp_root.join("papers");
    let log_path = temp_root.join("log.jsonl");

    let env = EnvGuard::new(&[
        "DOIGET_STORE_ROOT",
        "DOIGET_LOG_PATH",
        "DOIGET_ARXIV_BASE",
        "DOIGET_CROSSREF_BASE",
        "DOIGET_UNPAYWALL_BASE",
        "DOIGET_OA_PUBLISHER_BASE",
    ]);
    env.set("DOIGET_STORE_ROOT", store_root.as_str());
    env.set("DOIGET_LOG_PATH", log_path.as_str());

    let result = run_with_options(
        "arxiv:2401.12345".to_string(),
        true,
        None,
        OutputMode::Human,
    )
    .await;
    result.expect("dry-run arxiv fetch::run_with_options succeeds");

    assert!(
        !log_path.exists(),
        "dry-run arxiv path must not create the provenance log file: {log_path}",
    );
    let pdf_path = store_root.join("arxiv_2401.12345.pdf");
    let toml_path = store_root.join(".metadata").join("arxiv_2401.12345.toml");
    assert!(
        !pdf_path.exists(),
        "dry-run arxiv path must not write the PDF: {pdf_path}",
    );
    assert!(
        !toml_path.exists(),
        "dry-run arxiv path must not write the metadata TOML: {toml_path}",
    );

    drop(env);
    drop(td);
}

#[test]
fn build_fetch_plan_doi_envelope_matches_adr_0022_shape() {
    // Pure-function shape pin (no env state): the JSON envelope an
    // agent would observe for a DOI dry-run matches ADR-0022 §1 /
    // docs/MCP_TOOLS.md §10 to byte-level shape — `ok=true`,
    // `dry_run=true`, `ref={doi:...}`, `plan` with the four keys
    // documented in the ADR, and `rate_limit_budget` carrying the
    // hard-coded 5/sec + 200ms numbers.
    let r = Ref::parse("10.1234/foo").expect("DOI parses");
    let plan = build_fetch_plan(&r, &Utf8PathBuf::from("/tmp/store"));
    let env = build_dry_run_envelope(&r, &plan);

    assert_eq!(env["ok"], serde_json::json!(true));
    assert_eq!(env["dry_run"], serde_json::json!(true));
    assert_eq!(env["ref"], serde_json::json!({ "doi": "10.1234/foo" }));

    // Plan shape: per ADR-0022 §1.
    let plan_json = &env["plan"];
    assert_eq!(
        plan_json["metadata_sources"],
        serde_json::json!(["crossref", "unpaywall"])
    );
    assert!(plan_json["pdf_sources"].is_array());
    assert_eq!(plan_json["pdf_sources"][0]["key"], "oa-publisher");
    assert!(plan_json["pdf_sources"][0]["candidate_hosts"].is_array());
    assert_eq!(
        plan_json["redirect_allowlists_loaded"],
        serde_json::json!(["crossref", "unpaywall", "arxiv", "oa-publisher"])
    );
    // Path string form is platform-native (`/` on POSIX, `\` on Windows)
    // — compare against an `Utf8PathBuf::join` result rather than a raw
    // forward-slash literal so the assertion holds on both platforms.
    let expect_pdf = Utf8PathBuf::from("/tmp/store")
        .join("doi_10.1234_foo.pdf")
        .to_string();
    let expect_toml = Utf8PathBuf::from("/tmp/store")
        .join(".metadata")
        .join("doi_10.1234_foo.toml")
        .to_string();
    assert_eq!(plan_json["target_pdf_path"], serde_json::json!(expect_pdf));
    assert_eq!(
        plan_json["target_metadata_path"],
        serde_json::json!(expect_toml)
    );
    assert_eq!(
        plan_json["would_append_provenance"],
        serde_json::json!(true)
    );

    assert_eq!(
        env["rate_limit_budget"]["global_per_sec"],
        serde_json::json!(5.0)
    );
    assert_eq!(
        env["rate_limit_budget"]["per_source_min_gap_ms"],
        serde_json::json!(200)
    );
}

#[test]
fn build_fetch_plan_arxiv_envelope_uses_arxiv_pdf_source() {
    // Mirror for the arXiv branch: empty metadata_sources, single
    // arxiv pdf_source.
    let r = Ref::parse("arxiv:2401.12345").expect("arxiv parses");
    let plan = build_fetch_plan(&r, &Utf8PathBuf::from("/tmp/store"));
    let env = build_dry_run_envelope(&r, &plan);

    assert_eq!(env["ref"], serde_json::json!({ "arxiv": "2401.12345" }));
    assert_eq!(env["plan"]["metadata_sources"], serde_json::json!([]));
    assert_eq!(env["plan"]["pdf_sources"][0]["key"], "arxiv");
    let expect_pdf = Utf8PathBuf::from("/tmp/store")
        .join("arxiv_2401.12345.pdf")
        .to_string();
    assert_eq!(
        env["plan"]["target_pdf_path"],
        serde_json::json!(expect_pdf)
    );
}
