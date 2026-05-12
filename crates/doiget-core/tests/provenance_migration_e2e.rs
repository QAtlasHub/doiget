//! End-to-end coverage for the v1 → v2 provenance log migration
//! (ADR-0024, `docs/PROVENANCE_LOG.md` §"Schema migration").
//!
//! Drives [`doiget_core::provenance::migrate_v1_to_v2`] against the
//! synthetic v1 fixture at
//! `tests/fixtures/provenance/migration_v1_to_v2.json` (loaded via
//! `include_str!`) and asserts:
//!
//! 1. **Dry-run preview**: report counts match the fixture's
//!    `expected_rows_rewritten`, no disk writes happen.
//! 2. **Live rewrite**: produces a v2 log that `verify()` accepts as
//!    clean, preserves the original at `<log_path>.v1-backup`, and
//!    emits the expected `canonical_digest` per row computed via the
//!    independent [`CanonicalRef::new`] reference path (byte-equality
//!    contract from the task spec).
//! 3. **Idempotency**: re-running the migration on the now-v2 log
//!    succeeds (re-parses every row as v2) and produces byte-equivalent
//!    output.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::fs;
use std::io::Write;

use camino::Utf8PathBuf;
use serde_json::Value;
use tempfile::TempDir;

use doiget_core::provenance::{migrate_v1_to_v2, verify, LogRow};
use doiget_core::{CanonicalRef, SourceType};

const FIXTURE: &str = include_str!("../../../tests/fixtures/provenance/migration_v1_to_v2.json");

fn tmp_utf8(dir: &TempDir) -> Utf8PathBuf {
    Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("temp dir must be UTF-8")
}

/// Serialize the fixture's `v1_rows` array as JSON Lines into `path`.
fn write_v1_fixture(path: &Utf8PathBuf, fixture: &Value) {
    let rows = fixture["v1_rows"].as_array().expect("v1_rows array");
    let mut buf: Vec<u8> = Vec::new();
    for row in rows {
        let mut line = serde_json::to_vec(row).expect("serialize v1 row");
        line.push(b'\n');
        buf.extend_from_slice(&line);
    }
    let mut f = fs::File::create(path).expect("create v1 log file");
    f.write_all(&buf).expect("write v1 log file");
}

/// Independent reference: build a `CanonicalRef` from the fixture's
/// `expected_digest_seeds[i]` and return its `digest_hex()`. Returns
/// `None` for rows whose seed is the JSON null (session bookend etc.).
fn expected_digest_at(fixture: &Value, idx: usize) -> Option<String> {
    let seeds = fixture["expected_digest_seeds"]
        .as_array()
        .expect("expected_digest_seeds array");
    let seed = &seeds[idx];
    if seed.is_null() {
        return None;
    }
    let source_type_str = seed["source_type"].as_str().expect("source_type str");
    let source_id = seed["source_id"].as_str().expect("source_id str");
    let resolver_profile = seed["resolver_profile"]
        .as_str()
        .expect("resolver_profile str");
    let version = seed["version"].as_str().map(str::to_string);
    let st = match source_type_str {
        "doi" => SourceType::Doi,
        "arxiv" => SourceType::Arxiv,
        other => panic!("unknown source_type in fixture: {other}"),
    };
    Some(CanonicalRef::new(st, source_id, resolver_profile, version).digest_hex())
}

fn read_v2_rows(path: &Utf8PathBuf) -> Vec<LogRow> {
    let raw = fs::read_to_string(path).expect("read v2 log");
    raw.lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str::<LogRow>(l).expect("valid v2 LogRow"))
        .collect()
}

#[test]
fn migrate_dry_run_reports_expected_counts_without_touching_disk() {
    let fixture: Value = serde_json::from_str(FIXTURE).expect("fixture parses as JSON");
    let dir = TempDir::new().expect("tmp");
    let path = tmp_utf8(&dir).join("access.jsonl");
    write_v1_fixture(&path, &fixture);

    // Snapshot pre-migration file bytes — dry-run MUST NOT modify.
    let before = fs::read(&path).expect("read v1 bytes");

    let report = migrate_v1_to_v2(&path, true).expect("dry-run migrate must succeed");
    assert!(report.dry_run, "dry_run flag must round-trip in report");
    let expected_rows = fixture["expected_rows_rewritten"].as_u64().unwrap();
    assert_eq!(
        report.rows_rewritten, expected_rows,
        "dry-run row count must match fixture"
    );
    let expected_anchor = fixture["expected_v1_chain_anchor"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        report.first_row_v1_chain_hash, expected_anchor,
        "first-row v1 chain hash must match fixture anchor"
    );

    // Disk untouched.
    let after = fs::read(&path).expect("read v1 bytes after dry-run");
    assert_eq!(before, after, "dry-run must not touch disk");
    let staged = tmp_utf8(&dir).join("access.jsonl.v2-migrated");
    let backup = tmp_utf8(&dir).join("access.jsonl.v1-backup");
    assert!(!staged.exists(), "dry-run must not stage a v2 file");
    assert!(!backup.exists(), "dry-run must not create a backup");
}

#[test]
fn migrate_live_produces_v2_log_with_expected_canonical_digests() {
    let fixture: Value = serde_json::from_str(FIXTURE).expect("fixture parses as JSON");
    let dir = TempDir::new().expect("tmp");
    let path = tmp_utf8(&dir).join("access.jsonl");
    write_v1_fixture(&path, &fixture);

    let report = migrate_v1_to_v2(&path, false).expect("live migrate must succeed");
    assert!(!report.dry_run);
    assert_eq!(
        report.rows_rewritten,
        fixture["expected_rows_rewritten"].as_u64().unwrap()
    );
    assert_ne!(
        report.first_row_v1_chain_hash, report.first_row_v2_chain_hash,
        "the chain restart under the v2 canonicalization must shift the first-row hash"
    );

    // The migrated log MUST verify clean (recomputed chain + digests).
    let verify_report = verify(&path).expect("verify v2 log");
    assert!(
        verify_report.errors.is_empty(),
        "migrated v2 log must verify clean; issues: {:?}",
        verify_report.errors
    );

    // The backup is preserved at `<path>.v1-backup`.
    let backup = tmp_utf8(&dir).join("access.jsonl.v1-backup");
    assert!(backup.exists(), "v1 backup must be preserved at {backup}");
    let staged = tmp_utf8(&dir).join("access.jsonl.v2-migrated");
    assert!(
        !staged.exists(),
        "staged file must be renamed onto the live path"
    );

    // Byte-equality of canonical_digest against the independent
    // reference impl (`CanonicalRef::new(...).digest_hex()`).
    let rows = read_v2_rows(&path);
    assert_eq!(rows.len(), report.rows_rewritten as usize);
    for (idx, row) in rows.iter().enumerate() {
        let expected = expected_digest_at(&fixture, idx);
        assert_eq!(
            row.canonical_digest, expected,
            "row {idx} canonical_digest mismatch: stored={:?}, reference={:?}",
            row.canonical_digest, expected
        );
        assert_eq!(row.schema_version, "v2");
    }
}

#[test]
fn migrate_is_idempotent_on_v2_log() {
    // Migrate v1 -> v2, then re-run migrate on the now-v2 log. The
    // second run MUST succeed (re-parsing every row via the v2
    // fallback) and produce byte-equivalent output.
    let fixture: Value = serde_json::from_str(FIXTURE).expect("fixture parses as JSON");
    let dir = TempDir::new().expect("tmp");
    let path = tmp_utf8(&dir).join("access.jsonl");
    write_v1_fixture(&path, &fixture);

    let _first = migrate_v1_to_v2(&path, false).expect("first migrate must succeed");
    let first_bytes = fs::read(&path).expect("read v2 bytes");

    let second = migrate_v1_to_v2(&path, false).expect("second migrate must succeed (idempotent)");
    assert_eq!(
        second.rows_rewritten,
        fixture["expected_rows_rewritten"].as_u64().unwrap()
    );
    let second_bytes = fs::read(&path).expect("read v2 bytes after re-run");

    assert_eq!(
        first_bytes, second_bytes,
        "re-running migrate on a v2 log must produce byte-equivalent output"
    );
}

#[test]
fn migrate_dry_run_on_v2_log_is_byte_equal_preview() {
    let fixture: Value = serde_json::from_str(FIXTURE).expect("fixture parses as JSON");
    let dir = TempDir::new().expect("tmp");
    let path = tmp_utf8(&dir).join("access.jsonl");
    write_v1_fixture(&path, &fixture);
    let _ = migrate_v1_to_v2(&path, false).expect("first migrate must succeed");
    let bytes_before = fs::read(&path).expect("read v2 bytes");

    let preview = migrate_v1_to_v2(&path, true).expect("dry-run on v2 log must succeed");
    assert!(preview.dry_run);
    assert_eq!(
        preview.rows_rewritten,
        fixture["expected_rows_rewritten"].as_u64().unwrap()
    );

    let bytes_after = fs::read(&path).expect("read v2 bytes after dry-run");
    assert_eq!(
        bytes_before, bytes_after,
        "dry-run on v2 log must not touch disk"
    );
}
