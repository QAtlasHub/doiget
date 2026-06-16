//! End-to-end tests for `batch --mode json` (#205 + #210, ERRORS.md §3
//! CI persona).
//!
//! Validates the per-ref JSON-Lines wire shape:
//!
//! - Success: `{"ok":true,"ref":"...","result":{"safekey":"...","store_path":"...","canonical_digest":"..."}}`
//!   (#210 structured outcome plumbing).
//! - Failure: `{"ok":false,"ref":"...","error":{"code":"...","message":"..."[,"denial_context":{...}]}}`
//!   with `denial_context` per ADR-0023 when the underlying
//!   [`doiget_core::source::FetchError`] carries one.
//!
//! The exit code is the failure count (capped at 255, ERRORS.md §4).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::io::Write;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

fn doiget(dir: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("doiget").expect("locate doiget binary");
    let p = dir.path().to_str().expect("tempdir path is UTF-8");
    cmd.env("HOME", p)
        .env("USERPROFILE", p)
        .env("APPDATA", p)
        .env("XDG_CONFIG_HOME", p)
        .env("DOIGET_LOG_PATH", dir.path().join("access.jsonl"))
        .env("DOIGET_STORE_ROOT", dir.path().join("store"))
        .env("DOIGET_CONTACT_EMAIL", "test@example.com");
    cmd
}

#[test]
fn batch_json_parse_failure_emits_invalid_ref_jsonl() {
    let dir = TempDir::new().expect("tempdir");
    let refs = dir.path().join("refs.txt");
    {
        let mut f = std::fs::File::create(&refs).expect("create refs file");
        // One malformed line — must NOT parse as a DOI / arXiv id. We
        // also include a comment + blank to confirm those are skipped
        // (they should not produce JSONL records).
        f.write_all(b"# comment\nnot-a-doi\n\n")
            .expect("write refs");
    }

    let output = doiget(&dir)
        .args(["--json", "batch", refs.to_str().unwrap()])
        .assert()
        .failure() // parse_errors > 0 → CliExit(1)
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).expect("stdout utf-8");

    // Filter to non-empty lines so a stray trailing newline doesn't
    // break the count assertion.
    let lines: Vec<&str> = stdout.lines().filter(|s| !s.trim().is_empty()).collect();
    assert_eq!(lines.len(), 1, "exactly one JSONL record, got: {stdout}");

    let v: Value = serde_json::from_str(lines[0]).expect("line parses as JSON");
    assert_eq!(v["ok"], Value::Bool(false));
    assert_eq!(v["ref"], "not-a-doi");
    assert_eq!(
        v["error"]["code"], "INVALID_REF",
        "ERRORS.md §3 INVALID_REF on parse failure"
    );
    assert!(
        v["error"]["message"].is_string() && !v["error"]["message"].as_str().unwrap().is_empty(),
        "error.message MUST be a non-empty string"
    );
}

#[test]
fn batch_json_fetch_failure_emits_fetch_error_jsonl() {
    // Point the arxiv resolver at a closed loopback port so a parseable
    // ref deterministically fails at the transport layer. This exercises
    // the JoinSet drain's `Err(e)` branch and `emit_jsonl_failure` with
    // FETCH_ERROR — the previously-uncovered emit path.
    let dir = TempDir::new().expect("tempdir");
    let refs = dir.path().join("refs.txt");
    std::fs::File::create(&refs)
        .expect("create refs file")
        .write_all(b"arxiv:2401.99999\n")
        .expect("write refs");

    let output = doiget(&dir)
        // Closed port → connect-refused → fetch_one returns Err →
        // FETCH_ERROR JSONL.
        .env("DOIGET_ARXIV_BASE", "http://127.0.0.1:1/")
        .args(["--json", "batch", refs.to_str().unwrap()])
        .assert()
        .failure()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).expect("stdout utf-8");
    let lines: Vec<&str> = stdout.lines().filter(|s| !s.trim().is_empty()).collect();
    assert_eq!(lines.len(), 1, "exactly one JSONL record, got: {stdout}");
    let v: Value = serde_json::from_str(lines[0]).expect("line parses as JSON");
    assert_eq!(v["ok"], Value::Bool(false));
    // ADR-0030 + #210: the batch input now goes through
    // `refs::parse_input` → `Ref::as_input_str()`, which returns the
    // bare identifier per docs/PROVENANCE_LOG.md §3 (no `arxiv:` URI
    // scheme — that prefix is stripped at parse time). The
    // pre-ADR-0030 pipeline echoed the raw file line verbatim.
    assert_eq!(v["ref"], "2401.99999");
    // #210: the typed `FetchError → ErrorCode` mapping now surfaces
    // the closed-set wire code (`NETWORK_ERROR` for a transport-
    // layer connect-refused) instead of the previous generic
    // `FETCH_ERROR`.
    assert_eq!(
        v["error"]["code"], "NETWORK_ERROR",
        "connect-refused at the transport layer MUST surface as NETWORK_ERROR"
    );
    assert!(
        v["error"]["message"].is_string() && !v["error"]["message"].as_str().unwrap().is_empty(),
        "error.message MUST be a non-empty string"
    );
}

#[test]
fn batch_failure_digest_names_ref_and_code_on_stderr() {
    // Issue #222: the end-of-batch stderr digest names WHICH refs failed
    // and their primary error code, so a human / agent need not grep the
    // JSONL provenance log. Human mode (no `--json`) so the digest is the
    // visible failure surface.
    let dir = TempDir::new().expect("tempdir");
    let refs = dir.path().join("refs.txt");
    std::fs::File::create(&refs)
        .expect("create refs file")
        .write_all(b"arxiv:2401.99999\n")
        .expect("write refs");

    let stderr = doiget(&dir)
        // Closed port → connect-refused → NETWORK_ERROR.
        .env("DOIGET_ARXIV_BASE", "http://127.0.0.1:1/")
        .args(["batch", refs.to_str().unwrap()])
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let stderr = String::from_utf8(stderr).expect("stderr utf-8");
    assert!(
        stderr.contains("batch failures"),
        "digest header missing: {stderr}"
    );
    assert!(
        stderr.contains("2401.99999 -> NETWORK_ERROR"),
        "digest must name the ref and its primary error code: {stderr}"
    );
}

/// #210: a successful single-arxiv batch produces a structured success
/// JSONL record carrying `result.{safekey, store_path, canonical_digest}`.
/// Wiremock-driven so no real network traffic; the subprocess inherits
/// `DOIGET_ARXIV_BASE` pointing at the in-process mock.
///
/// Acceptance for #210: a CI consumer pipelining `batch --json` can
/// pull `result.safekey` to construct a store-relative path and
/// `result.canonical_digest` to deduplicate against an audit DB, all
/// without a follow-up `info` round-trip per ref.
#[tokio::test]
async fn batch_json_success_emits_structured_result_record() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let body = b"%PDF-1.7\n%fixture-bytes\n".to_vec();
    Mock::given(method("GET"))
        .and(path("/pdf/2401.12345.pdf"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("tempdir");
    let refs = dir.path().join("refs.txt");
    std::fs::File::create(&refs)
        .expect("create refs file")
        .write_all(b"arxiv:2401.12345\n")
        .expect("write refs");

    let output = doiget(&dir)
        .env("DOIGET_ARXIV_BASE", server.uri())
        .args(["--json", "batch", refs.to_str().unwrap()])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).expect("stdout utf-8");
    let lines: Vec<&str> = stdout.lines().filter(|s| !s.trim().is_empty()).collect();
    assert_eq!(lines.len(), 1, "exactly one JSONL record, got: {stdout}");

    let v: Value = serde_json::from_str(lines[0]).expect("line parses as JSON");
    assert_eq!(v["ok"], Value::Bool(true));
    // ADR-0030: ref is the canonical bare identifier
    // (`Ref::as_input_str`), not the raw input line.
    assert_eq!(v["ref"], "2401.12345");
    let result = v.get("result").expect("success record carries `result`");
    assert!(
        result["safekey"]
            .as_str()
            .map(|s| s.contains("2401.12345"))
            .unwrap_or(false),
        "result.safekey must echo the input id: {result}"
    );
    assert!(
        result["store_path"]
            .as_str()
            .map(|s| s.ends_with(".pdf"))
            .unwrap_or(false),
        "result.store_path must be the on-disk PDF path: {result}"
    );
    let digest = result["canonical_digest"]
        .as_str()
        .expect("canonical_digest is a string");
    assert_eq!(
        digest.len(),
        64,
        "canonical_digest MUST be 64-char lowercase hex (ADR-0021 §1): got {digest:?}"
    );
    assert!(
        digest
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
        "canonical_digest MUST be lowercase hex only: got {digest:?}"
    );
}

/// ADR-0030 slice 1: `doiget batch library.json` reads a CSL-JSON
/// export from a reference manager (Zotero / Mendeley) and walks the
/// resulting Refs through the same per-entry pipeline as plain refs.
///
/// This test seeds a 2-entry CSL-JSON file with one valid DOI and one
/// `archivePrefix=arXiv` entry, points the arxiv resolver at a closed
/// loopback (no actual fetch — the goal here is purely to verify the
/// adapter integration produces two JSONL lines, not to exercise the
/// orchestrator twice). Both entries surface as JSONL records with
/// the correct `ref` field, confirming the CSL-JSON parser landed
/// upstream of the fetch pipeline.
#[test]
fn batch_json_csl_input_yields_one_record_per_entry() {
    let dir = TempDir::new().expect("tempdir");
    let lib = dir.path().join("library.json");
    let body = r#"[
        {"id":"FooDOI","DOI":"10.1234/foo"},
        {"id":"BarArxiv","archivePrefix":"arXiv","eprint":"2401.12345"}
    ]"#;
    std::fs::File::create(&lib)
        .expect("create library.json")
        .write_all(body.as_bytes())
        .expect("write library");

    let output = doiget(&dir)
        // Closed port → every fetch fails fast; the test cares about
        // record count + per-record `ref` strings, not about success.
        .env("DOIGET_ARXIV_BASE", "http://127.0.0.1:1/")
        .env("DOIGET_CROSSREF_BASE", "http://127.0.0.1:1/")
        .env("DOIGET_UNPAYWALL_BASE", "http://127.0.0.1:1/")
        .args(["--json", "batch", lib.to_str().unwrap()])
        .assert()
        .failure() // every fetch fails → exit > 0
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).expect("stdout utf-8");
    let lines: Vec<&str> = stdout.lines().filter(|s| !s.trim().is_empty()).collect();
    assert_eq!(
        lines.len(),
        2,
        "expected one JSONL record per CSL-JSON entry, got: {stdout}"
    );
    let refs: Vec<String> = lines
        .iter()
        .map(|l| {
            let v: Value = serde_json::from_str(l).expect("line parses as JSON");
            v["ref"].as_str().expect("ref is string").to_string()
        })
        .collect();
    assert!(
        refs.iter().any(|r| r.contains("10.1234/foo")),
        "DOI entry must appear: {refs:?}"
    );
    assert!(
        refs.iter().any(|r| r.contains("2401.12345")),
        "arXiv entry must appear: {refs:?}"
    );
}

/// ADR-0030 slice 1: a malformed CSL-JSON document is surfaced as a
/// loud whole-input parse error before any fetch runs, not silently
/// treated as an empty batch.
#[test]
fn batch_malformed_csl_json_aborts_with_decode_error() {
    let dir = TempDir::new().expect("tempdir");
    let lib = dir.path().join("library.json");
    std::fs::File::create(&lib)
        .expect("create library.json")
        .write_all(b"{this is not JSON}")
        .expect("write library");

    let assert_result = doiget(&dir)
        .args(["batch", lib.to_str().unwrap()])
        .assert()
        .failure();
    let stderr =
        String::from_utf8(assert_result.get_output().stderr.clone()).expect("stderr utf-8");
    assert!(
        stderr.contains("csl-json") && stderr.to_lowercase().contains("deserialise"),
        "stderr must name the failed format + 'deserialise' verb: {stderr:?}"
    );
}

#[test]
fn batch_human_mode_remains_silent_on_stdout() {
    // ADR-0001 / pre-existing: batch in human mode emits its summary on
    // STDERR, not stdout. Regression-test that this is true even after
    // #205 wires the json branch onto stdout.
    let dir = TempDir::new().expect("tempdir");
    let refs = dir.path().join("refs.txt");
    std::fs::File::create(&refs)
        .expect("create refs file")
        .write_all(b"not-a-doi\n")
        .expect("write refs");

    let output = doiget(&dir)
        .env("DOIGET_MODE", "human")
        .args(["batch", refs.to_str().unwrap()])
        .assert()
        .failure()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).expect("stdout utf-8");
    assert!(
        stdout.is_empty(),
        "human-mode batch stdout MUST be empty (summary is stderr): {stdout:?}"
    );
}
