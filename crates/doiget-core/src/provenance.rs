//! JSON Lines + SHA-256 hash-chained provenance log.
//!
//! Binding spec: `docs/PROVENANCE_LOG.md` (NORMATIVE, §3 row schema, §4 hash
//! chain). Failure semantics: **fail-closed** — callers MUST abort the fetch
//! if a log write returns `Err`. See `docs/SECURITY.md` §1.8 and ADR-0006.
//!
//! # On-disk format
//!
//! - JSON Lines (`.jsonl`): one JSON object per line, terminated by `\n` (LF).
//! - UTF-8. Timestamps are RFC3339 in UTC.
//! - Each row is appended via a single `write_all` whose payload always ends
//!   in `\n`, so a partially-written row is detectable as a missing trailing
//!   newline rather than a torn JSON record.
//! - In audit-grade mode (the only mode shipped here), the writer flushes the
//!   `BufWriter` and `fsync`s the file after every row.
//!
//! # Hash chain (PROVENANCE_LOG.md §4)
//!
//! Each row carries a `prev_hash` and a `this_hash`. The first row's
//! `prev_hash` is the literal string `"GENESIS"`. Every subsequent row's
//! `prev_hash` MUST equal the previous row's `this_hash`.
//!
//! When a log file rotates (§6 — not yet implemented in this crate; see TODO
//! below), the first row of the NEW log file also uses `prev_hash =
//! "GENESIS"`, restarting the chain.
//!
//! `this_hash` is computed as:
//!
//! ```text
//! this_hash = lower_hex(SHA-256(canonical_json(row \ {this_hash})))
//! ```
//!
//! where `canonical_json` is **compact JSON (no whitespace) with object keys
//! sorted lexicographically** (PROVENANCE_LOG.md §4). For a row with fields
//! `{ts: "...", ts_seq: 1, event: "fetch", ...}`, the canonical bytes begin
//! with `{"capability":...` because `capability` is the lex-first top-level
//! key. Downstream `doiget audit-log --verify` (Phase 1+) relies on this
//! exact rule — do not change the canonicalization without bumping the spec.
//!
//! # In-process serialization
//!
//! `ProvenanceLog` holds a `Mutex<LogState>`. All `append` calls within the
//! same process serialize on this mutex, satisfying the "process-local mutex
//! on log appender" requirement of `docs/SECURITY.md` §1.8. Cross-process
//! coordination (multiple `doiget` invocations) is out of scope here and
//! handled by the higher-level `flock`-based store layer.
//!
//! # Session id
//!
//! `session_id` (PROVENANCE_LOG.md §3) is a 26-char ULID generated **once per
//! process invocation** by the caller and stamped into every row written
//! through the resulting [`ProvenanceLog`]. This crate does not generate the
//! ULID itself — see [`ProvenanceLog::open`] for the contract.
//!
//! # Log rotation and retention (§6)
//!
//! Implemented (PROVENANCE_LOG.md §6): when `access.log` exceeds
//! `ROTATE_BYTES` (100 MiB) a subsequent [`ProvenanceLog::append`]
//! gzip-compresses the full file to `access.log.<YYYY-MM-DD-HHMMSS>.gz`,
//! removes the old `access.log`, and writes the incoming row as the
//! first row of a fresh file with `prev_hash = "GENESIS"` (the hash
//! chain **restarts** per segment — segments are NOT linked). Rotation
//! is fail-closed: any gzip / rename / unlink failure aborts the
//! `append` (the caller's fetch aborts) so the chain never silently
//! skips. At [`ProvenanceLog::open`], rotated `.gz` segments older than
//! the retention window (`DOIGET_LOG_RETENTION_DAYS`, default 90; `0`
//! disables) are deleted **best-effort** (a prune failure is logged,
//! not fatal — pruning is housekeeping, not integrity).
//! [`verify_all`] verifies the current file plus every rotated `.gz`
//! segment (each its own GENESIS-rooted chain).

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::sync::Mutex;

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;

use camino::{Utf8Path, Utf8PathBuf};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// One row of the provenance log (PROVENANCE_LOG.md §3).
///
/// The on-disk wire field names match the spec table; struct-field order is
/// **not** load-bearing for the hash because canonicalization sorts keys
/// lexicographically (see PROVENANCE_LOG.md §4).
///
/// **Schema version**: this struct is the **v2** row shape (ADR-0024).
/// Every v2 row carries `schema_version = "v2"` literally; the
/// `canonical_digest` field carries the ADR-0021 §1 audit identity of
/// the fetch on rows where one applies (`Fetch` / `Resolve` /
/// `StoreWrite`) and is `None` on session bookend rows
/// (`SessionStart` / `SessionEnd` / `CapabilityResolved`) that have no
/// ref. v1 rows (pre-Slice-4) lack both fields and MUST be migrated via
/// [`migrate_v1_to_v2`] before the v2 binary can read them — the
/// `deny_unknown_fields` + non-defaulted `schema_version` shape ensures
/// v1 rows fail to parse loudly rather than producing silent hash-chain
/// mismatches.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogRow {
    /// RFC3339 UTC timestamp of the append (millisecond precision).
    pub ts: DateTime<Utc>,
    /// Per-session monotonic sequence number, starting at 1.
    pub ts_seq: u64,
    /// Event class (see [`LogEvent`]).
    pub event: LogEvent,
    /// Optional reference (DOI / arXiv id). Wire field name is `ref`.
    #[serde(rename = "ref")]
    pub ref_: Option<String>,
    /// Optional source name (e.g. `unpaywall`).
    pub source: Option<String>,
    /// Result (see [`LogResult`]).
    pub result: LogResult,
    /// OA license string (`event=fetch`, `result=ok`); `None` otherwise.
    pub license: Option<String>,
    /// Bytes written / fetched, on success rows.
    pub size_bytes: Option<u64>,
    /// Path to the stored payload, relative to the store root
    /// (`event=fetch`, `result=ok`); `None` otherwise.
    pub store_path: Option<String>,
    /// Capability under which the row was written (REQUIRED, every row).
    pub capability: Capability,
    /// 26-char ULID identifying the process invocation (REQUIRED).
    pub session_id: String,
    /// Stable error code on failure rows.
    pub error_code: Option<String>,
    /// Row schema version. Always [`LOG_SCHEMA_VERSION`] (`"v2"`) for
    /// new rows written by this build (ADR-0024). v1 rows lack this
    /// field; they MUST be migrated via [`migrate_v1_to_v2`] first.
    pub schema_version: String,
    /// Canonical-digest of the fetch's audit identity (ADR-0021 §1) as
    /// 64 lowercase hex chars. Present on rows with a `ref` (`Fetch`,
    /// `Resolve`, `StoreWrite`); `None` on session bookend rows. The
    /// digest is computed from a [`crate::CanonicalRef`] whose
    /// `resolver_profile` matches this row's `source` field for
    /// migrated v1 rows; new v2 rows MAY pass an explicit
    /// `resolver_profile` distinct from `source`.
    pub canonical_digest: Option<String>,
    /// 64 lowercase hex chars, OR the literal string `"GENESIS"` for the
    /// first row of a fresh log file.
    pub prev_hash: String,
    /// 64 lowercase hex chars. SHA-256 of canonical JSON of THIS row with
    /// the `this_hash` field removed. See module docs.
    pub this_hash: String,
}

/// Provenance-log row schema version this build writes
/// (`docs/PROVENANCE_LOG.md` §3, ADR-0024).
///
/// Bumped from `"v1"` (implicit; pre-Slice-4 rows had no
/// `schema_version` field) to `"v2"` when the `canonical_digest` column
/// landed. The v1→v2 migration is one-shot, idempotent, and dry-runnable
/// via [`migrate_v1_to_v2`].
pub const LOG_SCHEMA_VERSION: &str = "v2";

/// Event class for a log row (PROVENANCE_LOG.md §3).
///
/// Note: result-status (`ok`/`err`/`denied`) lives in [`LogResult`], NOT in
/// the event variant. So `Fetch` covers both successful and failed fetch
/// attempts; the row's `result` distinguishes them.
///
/// `non_exhaustive` so adding new variants is non-breaking.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LogEvent {
    /// Process started; first row of a new session.
    SessionStart,
    /// Capability resolution finished (allowed / denied / which env var).
    CapabilityResolved,
    /// Reference resolved to a fetch URL.
    Resolve,
    /// Fetch attempt (success or failure determined by `result`).
    Fetch,
    /// Store write attempt (success or failure determined by `result`).
    StoreWrite,
    /// Process ended cleanly.
    SessionEnd,
}

/// Per-row outcome (PROVENANCE_LOG.md §3). `non_exhaustive` for forward
/// compatibility.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LogResult {
    /// The operation succeeded.
    Ok,
    /// The operation failed with an error.
    Err,
    /// The operation was denied (e.g. capability gate).
    Denied,
}

/// Capability under which a row was written (PROVENANCE_LOG.md §3).
///
/// `kebab-case` serde rename emits `oa`, `metadata`, `tdm-elsevier`,
/// `tdm-aps`, `tdm-springer` exactly as the spec requires. `non_exhaustive`
/// for forward compatibility.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Capability {
    /// Open access tier.
    Oa,
    /// Metadata-only access.
    Metadata,
    /// Elsevier TDM (Tier 3, opt-in build).
    TdmElsevier,
    /// APS TDM (Tier 3, opt-in build).
    TdmAps,
    /// Springer TDM (Tier 3, opt-in build).
    TdmSpringer,
}

/// Errors emitted by the provenance log writer. Callers MUST treat any
/// variant as a fail-closed signal and abort the surrounding fetch.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LogError {
    /// I/O error opening, reading, writing, or syncing the log file. Includes
    /// recovery-time corruption detection where the synthetic message is
    /// `"corrupted log at line N: …"`.
    #[error("provenance log io error: {0}")]
    Io(#[from] std::io::Error),
    /// Serialization of a row to canonical JSON failed.
    #[error("provenance log serialization error: {0}")]
    Serialize(#[from] serde_json::Error),
    /// Path supplied to [`ProvenanceLog::open`] exists but is not a regular
    /// file (e.g. a directory or symlink).
    #[error("provenance log path is not a regular file: {0}")]
    NotARegularFile(Utf8PathBuf),
}

/// Append-only writer with in-process serialization.
#[derive(Debug)]
pub struct ProvenanceLog {
    path: Utf8PathBuf,
    state: Mutex<LogState>,
    session_id: String,
    /// §6 rotation threshold, resolved ONCE at [`ProvenanceLog::open`]
    /// (not per-`append`). Reading `DOIGET_LOG_ROTATE_BYTES` once at
    /// open — rather than on every append — means a log opened without
    /// the env set keeps the real 100 MiB threshold for its whole life
    /// even if another (test) thread later mutates that process-global
    /// env var; this removes a parallel-test race without serializing
    /// every multi-append test. `0` = rotation disabled.
    rotate_threshold: u64,
}

/// Mutable internal state, guarded by [`ProvenanceLog::state`].
#[derive(Debug)]
struct LogState {
    /// `ts_seq` of the **next** row to be appended.
    next_seq: u64,
    /// 64 lowercase hex chars; [`GENESIS_HASH`] if the log is empty.
    last_hash: String,
}

/// The genesis sentinel used as `prev_hash` for the first row of a log file
/// (PROVENANCE_LOG.md §3, §6). Also written verbatim as the prev-hash of the
/// first row after a log rotation (the chain restarts per segment).
const GENESIS_HASH: &str = "GENESIS";

/// Rotate `access.log` once it reaches this size (PROVENANCE_LOG.md §6:
/// "100 MB"). 100 MiB. Overridable via the `DOIGET_LOG_ROTATE_BYTES`
/// env var — an internal ops/testing knob (NOT a documented public
/// surface): tests set it tiny to exercise rotation without writing
/// 100 MiB; a value of `0` disables rotation.
const ROTATE_BYTES: u64 = 100 * 1024 * 1024;

/// Default rotated-segment retention (PROVENANCE_LOG.md §6: "90 days").
/// Overridable via `DOIGET_LOG_RETENTION_DAYS`; `0` disables pruning.
const DEFAULT_RETENTION_DAYS: i64 = 90;

/// Resolve the rotation threshold: `DOIGET_LOG_ROTATE_BYTES` if set and
/// parseable, else [`ROTATE_BYTES`]. `0` (or unparsable) → returns the
/// value as-is (`0` means "never rotate").
fn rotate_threshold_bytes() -> u64 {
    match std::env::var("DOIGET_LOG_ROTATE_BYTES") {
        Ok(s) => s.trim().parse::<u64>().unwrap_or(ROTATE_BYTES),
        Err(_) => ROTATE_BYTES,
    }
}

/// Resolve retention days from `DOIGET_LOG_RETENTION_DAYS`
/// (default [`DEFAULT_RETENTION_DAYS`]). `0` disables pruning. A
/// negative / unparsable value falls back to the default with a warn.
fn retention_days() -> i64 {
    match std::env::var("DOIGET_LOG_RETENTION_DAYS") {
        Ok(s) => match s.trim().parse::<i64>() {
            Ok(n) if n >= 0 => n,
            _ => {
                tracing::warn!(
                    value = %s,
                    "DOIGET_LOG_RETENTION_DAYS is not a non-negative integer; \
                     using the {DEFAULT_RETENTION_DAYS}-day default"
                );
                DEFAULT_RETENTION_DAYS
            }
        },
        Err(_) => DEFAULT_RETENTION_DAYS,
    }
}

/// gzip-compress `path` to `<file_name>.<YYYY-MM-DD-HHMMSS>.gz` (in the
/// same directory) and unlink `path` (PROVENANCE_LOG.md §6).
///
/// Atomic & fail-closed: the gzip is written to a `.tmp`, fsynced, then
/// `rename`d into place (so a partial `.gz` is never observable), and
/// only then is the original removed. Every step propagates its error
/// to the caller (`ProvenanceLog::append`), which is fail-closed — a
/// rotation failure aborts the surrounding fetch. Crash safety: a crash
/// after the rename but before the unlink leaves both the full `.gz`
/// and the (over-size) `access.log`; the next `append` simply rotates
/// again, producing a second independently-valid segment — wasteful but
/// never lossy or corrupt.
fn rotate_log(path: &Utf8Path) -> Result<(), LogError> {
    let file_name = path.file_name().ok_or_else(|| {
        LogError::Io(std::io::Error::other(
            "provenance log path has no file name; cannot rotate",
        ))
    })?;
    let ts = Utc::now().format("%Y-%m-%d-%H%M%S");
    let gz_name = format!("{file_name}.{ts}.gz");
    let dir = path.parent().unwrap_or_else(|| Utf8Path::new("."));
    let gz_path = dir.join(&gz_name);
    let tmp_path = dir.join(format!("{gz_name}.tmp"));

    {
        let mut src = File::open(path)?;
        let tmp = File::create(&tmp_path)?;
        let mut enc = GzEncoder::new(BufWriter::new(tmp), Compression::default());
        std::io::copy(&mut src, &mut enc)?;
        let bufw = enc.finish()?;
        let tmp = bufw.into_inner().map_err(|e| {
            LogError::Io(std::io::Error::other(format!(
                "gz tmp buf flush failed: {}",
                e.error()
            )))
        })?;
        tmp.sync_all()?;
    }
    std::fs::rename(&tmp_path, &gz_path)?;
    std::fs::remove_file(path)?;
    Ok(())
}

/// Rotated `.gz` segments siblings of `current`, sorted ascending. The
/// embedded `YYYY-MM-DD-HHMMSS` timestamp makes lexicographic order ==
/// chronological order.
fn rotated_segments(current: &Utf8Path) -> Vec<Utf8PathBuf> {
    let Some(file_name) = current.file_name() else {
        return Vec::new();
    };
    let dir = current.parent().unwrap_or_else(|| Utf8Path::new("."));
    let prefix = format!("{file_name}.");
    let mut segs: Vec<Utf8PathBuf> = match std::fs::read_dir(dir.as_std_path()) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter_map(|e| Utf8PathBuf::from_path_buf(e.path()).ok())
            .filter(|p| {
                p.file_name()
                    .map(|n| n.starts_with(&prefix) && n.ends_with(".gz"))
                    .unwrap_or(false)
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    segs.sort();
    segs
}

/// Delete rotated `.gz` segments older than `days` (PROVENANCE_LOG.md
/// §6 retention). `days <= 0` is a no-op (disabled). **Best-effort**:
/// pruning is housekeeping, not integrity, so any failure is logged and
/// skipped — `ProvenanceLog::open` still succeeds.
fn prune_rotated_segments(current: &Utf8Path, days: i64) {
    if days <= 0 {
        return;
    }
    let Some(cutoff) = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(days as u64 * 86_400))
    else {
        return;
    };
    for seg in rotated_segments(current) {
        let aged = std::fs::metadata(seg.as_std_path())
            .and_then(|m| m.modified())
            .map(|mt| mt < cutoff)
            .unwrap_or(false);
        if !aged {
            continue;
        }
        match std::fs::remove_file(seg.as_std_path()) {
            Ok(()) => tracing::info!(
                segment = %seg,
                "provenance: pruned rotated segment past retention"
            ),
            Err(e) => tracing::warn!(
                segment = %seg, error = %e,
                "provenance: failed to prune rotated segment (best-effort; continuing)"
            ),
        }
    }
}

/// Verify the full provenance history: every rotated `.gz` segment
/// (oldest→newest) followed by the current `access.log`. Each segment
/// is its own GENESIS-rooted hash chain (segments are deliberately NOT
/// linked across a rotation, PROVENANCE_LOG.md §6), so they are
/// verified independently and reported per-segment.
///
/// The audited [`verify`] function itself is unchanged; this only
/// orchestrates it over the segment set (gunzipping each `.gz` to a
/// tempfile first).
///
/// # Errors
///
/// [`LogError::Io`] on a gunzip / tempfile failure. A missing current
/// `access.log` is not an error ([`verify`] reports it empty).
pub fn verify_all(current: &Utf8Path) -> Result<Vec<(Utf8PathBuf, VerifyReport)>, LogError> {
    let mut out = Vec::new();
    for seg in rotated_segments(current) {
        let gz = File::open(seg.as_std_path())?;
        let mut dec = GzDecoder::new(gz);
        let tmp = tempfile::NamedTempFile::new().map_err(|e| {
            LogError::Io(std::io::Error::other(format!(
                "verify_all: tempfile for {seg}: {e}"
            )))
        })?;
        {
            let mut w = File::create(tmp.path())?;
            std::io::copy(&mut dec, &mut w)?;
            w.sync_all()?;
        }
        let tmp_utf8 = Utf8Path::from_path(tmp.path()).ok_or_else(|| {
            LogError::Io(std::io::Error::other("verify_all: non-utf8 tempfile path"))
        })?;
        let report = verify(tmp_utf8)?;
        out.push((seg, report));
        // `tmp` (and the gunzipped file) drop here, after verify.
    }
    let report = verify(current)?;
    out.push((current.to_path_buf(), report));
    Ok(out)
}

/// Caller-supplied fields for a row. The writer fills in `ts`, `ts_seq`,
/// `session_id`, `prev_hash`, `this_hash`, and the literal
/// `schema_version = "v2"` (`LOG_SCHEMA_VERSION`).
///
/// Callers SHOULD populate [`Self::canonical_digest`] on rows that have
/// a meaningful audit identity (`Fetch` / `Resolve` / `StoreWrite` rows
/// with a `ref`), leaving it `None` on session bookend rows. The digest
/// is produced by [`crate::CanonicalRef::digest_hex`] from a
/// `(source_type, source_id, resolver_profile, version)` tuple — see
/// ADR-0021 §1 for the algorithm and ADR-0024 for the implementation
/// surface.
#[derive(Debug, Clone)]
pub struct RowInput<'a> {
    /// Event class.
    pub event: LogEvent,
    /// Result.
    pub result: LogResult,
    /// Capability under which the row is written (REQUIRED for every row).
    pub capability: Capability,
    /// Optional DOI / arXiv id.
    pub ref_: Option<&'a str>,
    /// Optional source name.
    pub source: Option<&'a str>,
    /// Optional error code on failure rows.
    pub error_code: Option<&'a str>,
    /// Optional payload size in bytes.
    pub size_bytes: Option<u64>,
    /// Optional OA license string (set on `event=fetch`, `result=ok`).
    pub license: Option<&'a str>,
    /// Optional store path relative to the store root (set on `event=fetch`,
    /// `result=ok`).
    pub store_path: Option<&'a str>,
    /// Optional canonical-digest (ADR-0021 §1) as 64 lowercase hex
    /// chars. `None` for session bookend / capability-resolution rows;
    /// SHOULD be `Some` for `Fetch` / `Resolve` / `StoreWrite` rows
    /// whose `source` field names the resolver. Build via
    /// [`crate::Ref::promote`] + [`crate::CanonicalRef::digest_hex`].
    pub canonical_digest: Option<&'a str>,
}

// ---------------------------------------------------------------------------
// Canonical-JSON helper (PROVENANCE_LOG.md §4)
//
// Hashing rule (CRITICAL — this is the spec contract for `audit-log --verify`):
//
//   this_hash = lower_hex(SHA-256(canonical_json(row \ {this_hash})))
//
// Canonical JSON = **compact (no whitespace), keys sorted lexicographically,
// no trailing whitespace** (§4). Struct field order is deliberately NOT
// load-bearing here; the canonicalizer sorts the resulting object keys via
// `BTreeMap<String, Value>`, which serializes in lex-sorted key order.
//
// Worked example: for the row fragment `{ts_seq: 1, ts: "..."}` (input order),
// the canonical bytes after lex sort are `{"ts":"...","ts_seq":1}` because
// `"ts"` < `"ts_seq"` lexicographically. In v2 (ADR-0024) the lex-first
// top-level key is `"canonical_digest"` — `"canonical_digest"` < `"capability"`
// because 'n'(110) < 'p'(112) at byte index 2 (both share the `"ca"`
// prefix). The pre-v2 lex-first key was `"capability"`.
// ---------------------------------------------------------------------------

/// Serializable shadow of [`LogRow`] **without** `this_hash`. Used solely as
/// an intermediate to compute the canonical bytes that `this_hash` is the
/// SHA-256 of. The wire key names match [`LogRow`]'s `serde` attributes.
///
/// v2 shape (ADR-0024): includes `schema_version` and
/// `canonical_digest`. Both fields participate in the hash chain — a
/// tampered `canonical_digest` is detected by `audit-log --verify`
/// exactly like a tampered `ref` or `source` would be.
#[derive(Serialize)]
struct RowForHash<'a> {
    ts: DateTime<Utc>,
    ts_seq: u64,
    event: LogEvent,
    #[serde(rename = "ref")]
    ref_: Option<&'a str>,
    source: Option<&'a str>,
    result: LogResult,
    license: Option<&'a str>,
    size_bytes: Option<u64>,
    store_path: Option<&'a str>,
    capability: Capability,
    session_id: &'a str,
    error_code: Option<&'a str>,
    schema_version: &'a str,
    canonical_digest: Option<&'a str>,
    prev_hash: &'a str,
}

/// Produce canonical-JSON bytes for a row-without-hash, with object keys
/// sorted lexicographically per PROVENANCE_LOG.md §4.
///
/// Implementation: serialize via `serde_json::to_value` to get a `Value`,
/// require it be an object, then move its entries into a
/// `BTreeMap<String, Value>` (which serializes with lex-sorted keys) and
/// re-serialize compactly. No new dependency required.
fn canonical_json_for_hash(rfh: &RowForHash<'_>) -> Result<Vec<u8>, LogError> {
    let value = serde_json::to_value(rfh)?;
    let map = match value {
        serde_json::Value::Object(m) => m,
        // RowForHash is always a struct, so this branch is unreachable in
        // practice; surface as a serde error if it ever changes.
        _ => {
            return Err(LogError::Serialize(serde::de::Error::custom(
                "RowForHash did not serialize to a JSON object",
            )));
        }
    };
    let sorted: BTreeMap<String, serde_json::Value> = map.into_iter().collect();
    Ok(serde_json::to_vec(&sorted)?)
}

/// Compute `this_hash` for the given row-without-hash. Returns 64 lowercase
/// hex chars.
fn compute_this_hash(rfh: &RowForHash<'_>) -> Result<String, LogError> {
    let bytes = canonical_json_for_hash(rfh)?;
    let digest = Sha256::digest(&bytes);
    Ok(hex::encode(digest))
}

impl ProvenanceLog {
    /// Open or create the log at `path`, stamping every row with
    /// `session_id`.
    ///
    /// `session_id` MUST be a 26-char ULID generated **once per process**
    /// invocation by the caller. Re-opening the log within the same process
    /// reuses the same `session_id`; re-opening in a new process gets a new
    /// one. This crate intentionally does NOT generate the ULID itself —
    /// callers are responsible for creating one (e.g. via the `ulid` crate
    /// already present in the workspace) and threading it through.
    ///
    /// If the file exists, scan it once to recover the last `ts_seq` and
    /// `this_hash`. If the file is missing or empty, the first row will use
    /// `prev_hash = "GENESIS"` and `ts_seq = 1`.
    ///
    /// # Errors
    ///
    /// Returns [`LogError::Io`] for I/O failures or if any line fails to
    /// parse as a [`LogRow`] (synthetic message: `"corrupted log at line N: …"`).
    /// The writer never silently truncates a corrupt log.
    ///
    /// Returns [`LogError::NotARegularFile`] if `path` exists but is not a
    /// regular file (e.g. a directory).
    pub fn open(path: impl Into<Utf8PathBuf>, session_id: String) -> Result<Self, LogError> {
        let path: Utf8PathBuf = path.into();

        // Reject obvious non-files up front so later `OpenOptions::append`
        // doesn't produce a confusing platform-dependent error.
        if path.exists() {
            let md = std::fs::metadata(&path)?;
            if !md.is_file() {
                return Err(LogError::NotARegularFile(path));
            }
        }

        let (next_seq, last_hash) = recover_state(&path)?;

        // §6 retention: prune rotated `.gz` segments older than the
        // window. Best-effort — pruning is housekeeping, not integrity,
        // so a failure is logged and `open` still succeeds (unlike
        // rotation, which is fail-closed).
        prune_rotated_segments(&path, retention_days());

        Ok(Self {
            path,
            state: Mutex::new(LogState {
                next_seq,
                last_hash,
            }),
            session_id,
            rotate_threshold: rotate_threshold_bytes(),
        })
    }

    /// Append a row. Computes `prev_hash`, `ts_seq`, `ts`, `session_id`, and
    /// `this_hash`; the caller only supplies the semantic fields via
    /// [`RowInput`].
    ///
    /// Returns the assigned `ts_seq` on success.
    ///
    /// # Errors
    ///
    /// Returns [`LogError`] on serialization, I/O, or fsync failure. Callers
    /// MUST treat this as fail-closed and abort the surrounding fetch.
    pub fn append(&self, input: RowInput<'_>) -> Result<u64, LogError> {
        // Hold the mutex for the entire append: serialize + write + flush +
        // fsync + state update. This is the in-process serialization point
        // promised by `docs/SECURITY.md` §1.8.
        //
        // A poisoned mutex only happens if a previous `append` panicked
        // mid-write. Surface that as an I/O error rather than propagating
        // a panic.
        let mut state = self
            .state
            .lock()
            .map_err(|_| LogError::Io(std::io::Error::other("provenance log mutex poisoned")))?;

        // §6 rotation, BEFORE this row is written. If `access.log` has
        // reached the threshold, gzip+rename it and reset the in-memory
        // chain state so this row becomes the GENESIS-rooted first row of
        // a fresh file. Fail-closed: a rotation error aborts the append
        // (the `?`), so the caller's fetch aborts and the chain never
        // silently continues in an over-size or half-rotated file. The
        // `state` mutex is held, so rotation is serialized with appends.
        let threshold = self.rotate_threshold;
        if threshold > 0 {
            let size = match std::fs::metadata(&self.path) {
                Ok(m) => m.len(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
                Err(e) => return Err(LogError::Io(e)),
            };
            if size >= threshold {
                rotate_log(&self.path)?;
                state.next_seq = 1;
                state.last_hash = GENESIS_HASH.to_string();
            }
        }

        let ts_seq = state.next_seq;
        let prev_hash = state.last_hash.clone();
        let ts = Utc::now();

        let rfh = RowForHash {
            ts,
            ts_seq,
            event: input.event,
            ref_: input.ref_,
            source: input.source,
            result: input.result,
            license: input.license,
            size_bytes: input.size_bytes,
            store_path: input.store_path,
            capability: input.capability,
            session_id: &self.session_id,
            error_code: input.error_code,
            schema_version: LOG_SCHEMA_VERSION,
            canonical_digest: input.canonical_digest,
            prev_hash: &prev_hash,
        };

        let this_hash = compute_this_hash(&rfh)?;

        // Build the on-disk row. Owned strings here because `LogRow` does
        // not borrow.
        let row = LogRow {
            ts,
            ts_seq,
            event: input.event,
            ref_: input.ref_.map(str::to_string),
            source: input.source.map(str::to_string),
            result: input.result,
            license: input.license.map(str::to_string),
            size_bytes: input.size_bytes,
            store_path: input.store_path.map(str::to_string),
            capability: input.capability,
            session_id: self.session_id.clone(),
            error_code: input.error_code.map(str::to_string),
            schema_version: LOG_SCHEMA_VERSION.to_string(),
            canonical_digest: input.canonical_digest.map(str::to_string),
            prev_hash,
            this_hash: this_hash.clone(),
        };

        // Serialize, append `\n`, write_all in one syscall, flush BufWriter,
        // fsync the underlying file. `\n` is part of the same buffer, so a
        // crash mid-write leaves at most a partial line (no trailing `\n`),
        // which is detectable on recovery as a corrupted final line.
        let mut bytes = serde_json::to_vec(&row)?;
        bytes.push(b'\n');

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        let mut writer = BufWriter::new(file);
        writer.write_all(&bytes)?;
        writer.flush()?;
        // `into_inner` to recover the underlying File for `sync_all`.
        let file = writer.into_inner().map_err(|e| {
            LogError::Io(std::io::Error::other(format!(
                "buf writer flush failed: {}",
                e.error()
            )))
        })?;
        file.sync_all()?;

        // Only after a successful fsync do we advance the in-memory state.
        // If any of the above fails, the next `append` retries from the
        // same `(ts_seq, prev_hash)` — at most a torn last line on disk.
        state.next_seq = ts_seq + 1;
        state.last_hash = this_hash;

        Ok(ts_seq)
    }

    /// Returns the path the log was opened at. Useful for tests and audit tooling.
    pub fn path(&self) -> &Utf8Path {
        &self.path
    }

    /// Returns the session id stamped into every row written through this
    /// writer.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

/// Scan an existing log to recover `(next_seq, last_hash)`.
///
/// Walk every line, parse as [`LogRow`], track the last successfully parsed
/// row. If parsing fails, return [`LogError::Io`] with a synthetic
/// `"corrupted log at line N: …"` message — never silently truncate.
fn recover_state(path: &Utf8Path) -> Result<(u64, String), LogError> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok((1, GENESIS_HASH.to_string()));
        }
        Err(e) => return Err(LogError::Io(e)),
    };

    let reader = BufReader::new(file);
    let mut last_seq: u64 = 0;
    let mut last_hash: String = GENESIS_HASH.to_string();

    for (idx, line_res) in reader.lines().enumerate() {
        let line_no = idx + 1;
        let line = line_res?;
        if line.is_empty() {
            // Tolerate trailing/empty lines silently — they are not data.
            continue;
        }
        let row: LogRow = serde_json::from_str(&line).map_err(|e| {
            LogError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("corrupted log at line {}: {}", line_no, e),
            ))
        })?;
        last_seq = row.ts_seq;
        last_hash = row.this_hash;
    }

    if last_seq == 0 {
        Ok((1, GENESIS_HASH.to_string()))
    } else {
        Ok((last_seq + 1, last_hash))
    }
}

// ---------------------------------------------------------------------------
// Verification (`doiget audit-log --verify`)
//
// The provenance log is a JSON Lines file with a SHA-256 hash chain
// (PROVENANCE_LOG.md §4). Tampering is detected by recomputing every row's
// `this_hash` and validating the chain. This module provides the offline
// verifier; the CLI wrapper lives in `doiget-cli::commands::audit_log`.
//
// Failure model: returning `Err` is reserved for I/O failures opening / reading
// the file. Per-row issues (parse failures, hash/chain mismatches, sequence
// regressions) are accumulated into [`VerifyReport::errors`] so callers can
// report them all in one pass — this is the contract Phase 1 ships.
// ---------------------------------------------------------------------------

/// Outcome of [`verify`]: per-row chain status across the entire log.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct VerifyReport {
    /// Total non-empty lines processed (1-based count).
    pub total_rows: usize,
    /// Rows whose hash, chain link, and `ts_seq` all validated.
    pub ok_rows: usize,
    /// Issues encountered, in encounter order. Line numbers are 1-based.
    pub errors: Vec<VerifyIssue>,
}

impl VerifyReport {
    /// An empty, all-clear report — used when the log file is absent.
    fn empty() -> Self {
        Self {
            total_rows: 0,
            ok_rows: 0,
            errors: Vec::new(),
        }
    }
}

/// A single issue discovered by [`verify`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct VerifyIssue {
    /// 1-based line number where the issue was detected.
    pub line: usize,
    /// Classification of the issue (see [`VerifyIssueKind`]).
    pub kind: VerifyIssueKind,
    /// Human-readable description (caller may format for stderr/stdout).
    pub message: String,
}

/// Classification of a [`VerifyIssue`]. `non_exhaustive` for forward
/// compatibility — future kinds may include `SessionIdChange`, etc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum VerifyIssueKind {
    /// Row failed to parse as [`LogRow`] (corrupted JSON or unknown field).
    ParseError,
    /// `prev_hash` did not match the previous row's `this_hash` (or the
    /// genesis sentinel on row 1).
    PrevHashMismatch,
    /// Row's stored `this_hash` did not match the recomputed canonical-JSON
    /// SHA-256.
    ThisHashMismatch,
    /// `ts_seq` did not increase strictly monotonically (within a session;
    /// see PROVENANCE_LOG.md §3 + §6 — chain restarts after rotation are
    /// permitted to reset `ts_seq` and are detected via the genesis sentinel).
    SequenceJump,
}

/// Verify the entire log file at `path`.
///
/// Returns `Ok(VerifyReport)` regardless of whether the chain validates;
/// callers inspect `report.errors.is_empty()` to determine pass/fail.
/// Returns `Err` only when the file itself cannot be opened or read at the
/// I/O level.
///
/// Behavior:
///
/// - A missing file is treated as a clean, empty log (no tampering possible
///   on bytes that don't exist) and returns an empty report after a `warn!`.
/// - Empty / blank lines are skipped — they are not data per the writer's
///   on-disk format (PROVENANCE_LOG.md §2).
/// - On a row that fails to parse as [`LogRow`], a `ParseError` is recorded
///   and verification continues on the next line. The chain anchor does NOT
///   advance through an unparsable row, so the next valid row's `prev_hash`
///   is checked against the last successfully parsed row (or against
///   `"GENESIS"` if no valid row has been seen yet).
/// - A `prev_hash == "GENESIS"` sentinel marks a chain restart (first row of
///   a fresh / rotated log per §6) and resets the `ts_seq` monotonicity
///   anchor — `ts_seq` is NOT compared to the prior row across a restart.
pub fn verify(path: &Utf8Path) -> Result<VerifyReport, LogError> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::warn!(
                path = %path,
                "audit-log verify: log file does not exist; reporting empty"
            );
            return Ok(VerifyReport::empty());
        }
        Err(e) => return Err(LogError::Io(e)),
    };

    let reader = BufReader::new(file);
    let mut report = VerifyReport::empty();

    // Anchor for the chain check: the LAST SUCCESSFULLY PARSED row. The chain
    // is anchored to the bytes on disk, not to a hypothetical "should have
    // been". This matches the spec — tampering at row N must surface both as
    // a hash mismatch on N and as a chain break on N+1.
    let mut prev_row: Option<LogRow> = None;

    for (idx, line_res) in reader.lines().enumerate() {
        let line_no = idx + 1;
        let line = line_res?;
        if line.is_empty() {
            continue;
        }

        report.total_rows += 1;

        let row: LogRow = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                report.errors.push(VerifyIssue {
                    line: line_no,
                    kind: VerifyIssueKind::ParseError,
                    message: format!("failed to parse row as LogRow: {e}"),
                });
                // Chain anchor cannot advance through an unparsable row;
                // leave `prev_row` untouched so the next valid row's
                // `prev_hash` is checked against the last-known anchor (or
                // GENESIS if we never had one).
                continue;
            }
        };

        let mut row_ok = true;

        // 1. Recompute `this_hash` from canonical JSON (row \ {this_hash}).
        let rfh = RowForHash {
            ts: row.ts,
            ts_seq: row.ts_seq,
            event: row.event,
            ref_: row.ref_.as_deref(),
            source: row.source.as_deref(),
            result: row.result,
            license: row.license.as_deref(),
            size_bytes: row.size_bytes,
            store_path: row.store_path.as_deref(),
            capability: row.capability,
            session_id: &row.session_id,
            error_code: row.error_code.as_deref(),
            schema_version: &row.schema_version,
            canonical_digest: row.canonical_digest.as_deref(),
            prev_hash: &row.prev_hash,
        };
        match compute_this_hash(&rfh) {
            Ok(recomputed) => {
                if recomputed != row.this_hash {
                    report.errors.push(VerifyIssue {
                        line: line_no,
                        kind: VerifyIssueKind::ThisHashMismatch,
                        message: format!(
                            "this_hash mismatch: stored={}, recomputed={}",
                            row.this_hash, recomputed
                        ),
                    });
                    row_ok = false;
                }
            }
            Err(e) => {
                // Canonicalization itself failed — surface as a hash
                // mismatch with the underlying error in the message.
                report.errors.push(VerifyIssue {
                    line: line_no,
                    kind: VerifyIssueKind::ThisHashMismatch,
                    message: format!("failed to recompute this_hash: {e}"),
                });
                row_ok = false;
            }
        }

        // 2. Chain link: `prev_hash` matches anchor (GENESIS on row 1 / after
        //    a chain restart, prior row's `this_hash` otherwise).
        let is_genesis = row.prev_hash == GENESIS_HASH;
        match &prev_row {
            None => {
                // First non-empty row in the file: must declare GENESIS.
                if !is_genesis {
                    report.errors.push(VerifyIssue {
                        line: line_no,
                        kind: VerifyIssueKind::PrevHashMismatch,
                        message: format!(
                            "first row must have prev_hash=\"GENESIS\", got {:?}",
                            row.prev_hash
                        ),
                    });
                    row_ok = false;
                }
            }
            Some(prev) => {
                if is_genesis {
                    // Chain restart (rotation per §6) — accepted, no link
                    // check, and the `ts_seq` monotonicity anchor resets
                    // (handled below via `is_genesis`).
                } else if row.prev_hash != prev.this_hash {
                    report.errors.push(VerifyIssue {
                        line: line_no,
                        kind: VerifyIssueKind::PrevHashMismatch,
                        message: format!(
                            "prev_hash mismatch: row stores {}, previous row's this_hash is {}",
                            row.prev_hash, prev.this_hash
                        ),
                    });
                    row_ok = false;
                }
            }
        }

        // 3. ts_seq monotonicity — strictly greater than the previous row's
        //    `ts_seq`, EXCEPT across a chain restart (where `ts_seq` resets).
        if let Some(prev) = &prev_row {
            if !is_genesis && row.ts_seq <= prev.ts_seq {
                report.errors.push(VerifyIssue {
                    line: line_no,
                    kind: VerifyIssueKind::SequenceJump,
                    message: format!(
                        "ts_seq did not increase strictly: previous={}, current={}",
                        prev.ts_seq, row.ts_seq
                    ),
                });
                row_ok = false;
            }
        }

        if row_ok {
            report.ok_rows += 1;
        }

        // Advance the anchor to the just-parsed row (whether or not it had
        // issues — the on-disk bytes ARE the chain).
        prev_row = Some(row);
    }

    Ok(report)
}

// ---------------------------------------------------------------------------
// v1 → v2 migration (ADR-0024, `docs/PROVENANCE_LOG.md` §"Schema migration").
//
// v1 rows lack `schema_version` and `canonical_digest`; the v2 binary
// fails loudly when asked to read them (see `recover_state` /
// `verify`). The migration recovers a v2 log from a v1 file by:
//
//   1. Parsing every v1 row via the [`V1LogRow`] shadow struct.
//   2. Deriving a [`crate::CanonicalRef`] from the v1 `(ref, source)`
//      pair — `source` becomes `resolver_profile`, `version` is `None`
//      (ADR-0021 §1 → ADR-0024 migration recipe).
//   3. Re-computing the SHA-256 hash chain across the new row
//      payloads. The v1 chain is invalidated by the schema change; the
//      v2 chain restarts at the first row's stored `prev_hash` (which
//      is `"GENESIS"` on a fresh log).
//   4. Writing the new rows to `<log_path>.v2-migrated`, then
//      atomically renaming it onto `<log_path>` after backing up the
//      original to `<log_path>.v1-backup`.
//
// The migration is **idempotent**: running it on an already-v2 log
// re-parses every row as v2, recomputes the same hash chain, and
// produces a byte-equivalent output.
//
// The migration is **dry-runnable**: `dry_run = true` returns a
// [`MigrationReport`] summarizing what would change without touching
// disk.
// ---------------------------------------------------------------------------

/// Summary of a [`migrate_v1_to_v2`] run.
///
/// Marked `#[non_exhaustive]` so future fields (e.g. a per-row error
/// list, an aborted-row count) can be added without breaking callers
/// that pattern-match.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct MigrationReport {
    /// Number of rows rewritten (or that WOULD be rewritten under
    /// `dry_run`).
    pub rows_rewritten: u64,
    /// Whether this was a dry-run preview (`true`) or a live rewrite
    /// (`false`).
    pub dry_run: bool,
    /// Stored `this_hash` of the first input row (the v1 chain anchor).
    /// `"GENESIS"` is reported as the literal `"GENESIS"` when the log
    /// was empty.
    pub first_row_v1_chain_hash: String,
    /// Recomputed `this_hash` of the first migrated row under the v2
    /// canonicalization. Equal to [`Self::first_row_v1_chain_hash`]
    /// only if the input was already v2 (idempotent case).
    pub first_row_v2_chain_hash: String,
}

/// v1 row shadow struct used ONLY by [`migrate_v1_to_v2`]. The
/// non-defaulted v2 fields (`schema_version`, `canonical_digest`) are
/// absent here; `deny_unknown_fields` rejects unexpected v2 fields so a
/// v2 row on disk fails to parse as v1, letting the migrator detect
/// already-v2 input via fallback to the v2 parser.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct V1LogRow {
    ts: DateTime<Utc>,
    ts_seq: u64,
    event: LogEvent,
    #[serde(rename = "ref")]
    ref_: Option<String>,
    source: Option<String>,
    result: LogResult,
    license: Option<String>,
    size_bytes: Option<u64>,
    store_path: Option<String>,
    capability: Capability,
    session_id: String,
    error_code: Option<String>,
    prev_hash: String,
    this_hash: String,
}

/// Minimal in-memory representation a v1 OR v2 row can be promoted to
/// before re-hashing.
#[derive(Debug, Clone)]
struct MigrationRowSeed {
    ts: DateTime<Utc>,
    ts_seq: u64,
    event: LogEvent,
    ref_: Option<String>,
    source: Option<String>,
    result: LogResult,
    license: Option<String>,
    size_bytes: Option<u64>,
    store_path: Option<String>,
    capability: Capability,
    session_id: String,
    error_code: Option<String>,
    /// `None` for v1 inputs (the digest is computed during migration);
    /// `Some(...)` for already-v2 inputs (carried through verbatim for
    /// idempotency).
    canonical_digest_in: Option<String>,
    /// As stored on disk in the input. Used only for the
    /// `first_row_v1_chain_hash` field of [`MigrationReport`].
    stored_this_hash: String,
}

/// Migrate a v1 provenance log to v2 (ADR-0024).
///
/// Returns a [`MigrationReport`] describing how many rows were (or
/// would be) rewritten and the first-row chain-anchor delta. The
/// migration is idempotent: running it twice produces byte-equivalent
/// output the second time.
///
/// On a missing log file, returns a no-op report (`rows_rewritten = 0`,
/// `first_row_v1_chain_hash = "GENESIS"`, `first_row_v2_chain_hash =
/// "GENESIS"`) — there is nothing to migrate.
///
/// # Errors
///
/// Returns [`LogError::Io`] on I/O failures and on rows that fail to
/// parse as either v1 or v2 (the synthetic message names the line
/// number). Returns [`LogError::Serialize`] on canonicalization
/// failures.
pub fn migrate_v1_to_v2(log_path: &Utf8Path, dry_run: bool) -> Result<MigrationReport, LogError> {
    use std::io::BufRead;

    // -- 1. Read the input log, parsing each line as v1 OR (idempotent
    //       fallback) v2. --------------------------------------------------
    let file = match File::open(log_path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(MigrationReport {
                rows_rewritten: 0,
                dry_run,
                first_row_v1_chain_hash: GENESIS_HASH.to_string(),
                first_row_v2_chain_hash: GENESIS_HASH.to_string(),
            });
        }
        Err(e) => return Err(LogError::Io(e)),
    };
    let reader = BufReader::new(file);
    let mut seeds: Vec<MigrationRowSeed> = Vec::new();

    for (idx, line_res) in reader.lines().enumerate() {
        let line_no = idx + 1;
        let line = line_res?;
        if line.is_empty() {
            continue;
        }
        // Try v1 first. If it fails, try v2 (idempotency: re-migrating
        // a v2 log MUST succeed and produce equivalent output).
        let seed = if let Ok(v1) = serde_json::from_str::<V1LogRow>(&line) {
            MigrationRowSeed {
                ts: v1.ts,
                ts_seq: v1.ts_seq,
                event: v1.event,
                ref_: v1.ref_,
                source: v1.source,
                result: v1.result,
                license: v1.license,
                size_bytes: v1.size_bytes,
                store_path: v1.store_path,
                capability: v1.capability,
                session_id: v1.session_id,
                error_code: v1.error_code,
                canonical_digest_in: None,
                stored_this_hash: v1.this_hash,
            }
        } else {
            match serde_json::from_str::<LogRow>(&line) {
                Ok(v2) => MigrationRowSeed {
                    ts: v2.ts,
                    ts_seq: v2.ts_seq,
                    event: v2.event,
                    ref_: v2.ref_,
                    source: v2.source,
                    result: v2.result,
                    license: v2.license,
                    size_bytes: v2.size_bytes,
                    store_path: v2.store_path,
                    capability: v2.capability,
                    session_id: v2.session_id,
                    error_code: v2.error_code,
                    canonical_digest_in: v2.canonical_digest,
                    stored_this_hash: v2.this_hash,
                },
                Err(e) => {
                    return Err(LogError::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("migration: line {line_no} is neither v1 nor v2: {e}"),
                    )));
                }
            }
        };
        seeds.push(seed);
    }

    // -- 2. Derive `canonical_digest` for each seed that lacks one. ------
    //
    // For v1 rows: build a CanonicalRef from
    //   - source_type from `event`/`ref` shape (DOI prefix `10.` vs
    //     arXiv) — we use a heuristic that matches `Ref::parse`'s rule
    //     (`starts_with "10."` ⇒ DOI; else arXiv).
    //   - source_id = ref value (verbatim).
    //   - resolver_profile = source value (verbatim, ADR-0021 §3
    //     migration recipe).
    //   - version = None.
    //
    // Rows without a `ref` (session bookend) keep `canonical_digest =
    // None` per the v2 row contract.

    fn derive_digest(seed: &MigrationRowSeed) -> Option<String> {
        let ref_str = seed.ref_.as_deref()?;
        let source_key = seed.source.as_deref().unwrap_or("");
        // Heuristic: bare DOIs always start `10.`; everything else is
        // treated as an arXiv id. Mirrors `Ref::parse` rule 3/4.
        let source_type = if ref_str.starts_with("10.") {
            crate::SourceType::Doi
        } else {
            crate::SourceType::Arxiv
        };
        let c = crate::CanonicalRef::new(source_type, ref_str, source_key, None);
        Some(c.digest_hex())
    }

    let digests: Vec<Option<String>> = seeds
        .iter()
        .map(|s| s.canonical_digest_in.clone().or_else(|| derive_digest(s)))
        .collect();

    // -- 3. Rebuild the hash chain across the v2 payloads. ----------------
    let mut out_rows: Vec<LogRow> = Vec::with_capacity(seeds.len());
    let mut prev_hash: String = GENESIS_HASH.to_string();

    for (seed, digest) in seeds.iter().zip(digests.iter()) {
        let rfh = RowForHash {
            ts: seed.ts,
            ts_seq: seed.ts_seq,
            event: seed.event,
            ref_: seed.ref_.as_deref(),
            source: seed.source.as_deref(),
            result: seed.result,
            license: seed.license.as_deref(),
            size_bytes: seed.size_bytes,
            store_path: seed.store_path.as_deref(),
            capability: seed.capability,
            session_id: &seed.session_id,
            error_code: seed.error_code.as_deref(),
            schema_version: LOG_SCHEMA_VERSION,
            canonical_digest: digest.as_deref(),
            prev_hash: &prev_hash,
        };
        let this_hash = compute_this_hash(&rfh)?;
        let row = LogRow {
            ts: seed.ts,
            ts_seq: seed.ts_seq,
            event: seed.event,
            ref_: seed.ref_.clone(),
            source: seed.source.clone(),
            result: seed.result,
            license: seed.license.clone(),
            size_bytes: seed.size_bytes,
            store_path: seed.store_path.clone(),
            capability: seed.capability,
            session_id: seed.session_id.clone(),
            error_code: seed.error_code.clone(),
            schema_version: LOG_SCHEMA_VERSION.to_string(),
            canonical_digest: digest.clone(),
            prev_hash: prev_hash.clone(),
            this_hash: this_hash.clone(),
        };
        prev_hash = this_hash;
        out_rows.push(row);
    }

    // -- 4. Build the report. --------------------------------------------
    let first_v1_hash = seeds
        .first()
        .map(|s| s.stored_this_hash.clone())
        .unwrap_or_else(|| GENESIS_HASH.to_string());
    let first_v2_hash = out_rows
        .first()
        .map(|r| r.this_hash.clone())
        .unwrap_or_else(|| GENESIS_HASH.to_string());
    let report = MigrationReport {
        rows_rewritten: out_rows.len() as u64,
        dry_run,
        first_row_v1_chain_hash: first_v1_hash,
        first_row_v2_chain_hash: first_v2_hash,
    };

    if dry_run {
        return Ok(report);
    }

    // -- 5. Live write: stage to `<log_path>.v2-migrated`, back up the
    //       v1, then atomically rename. -----------------------------------
    let staged_path = with_suffix(log_path, ".v2-migrated");
    let backup_path = with_suffix(log_path, ".v1-backup");

    {
        let staged_file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&staged_path)?;
        let mut writer = BufWriter::new(staged_file);
        for row in &out_rows {
            let mut bytes = serde_json::to_vec(row)?;
            bytes.push(b'\n');
            writer.write_all(&bytes)?;
        }
        writer.flush()?;
        let file = writer.into_inner().map_err(|e| {
            LogError::Io(std::io::Error::other(format!(
                "migration buf writer flush failed: {}",
                e.error()
            )))
        })?;
        file.sync_all()?;
    }

    // Sanity-check: the staged file MUST verify clean before we
    // commit the swap. If it doesn't, the migration is buggy — abort
    // without touching the live log.
    let verify_report = verify(&staged_path)?;
    if !verify_report.errors.is_empty() {
        return Err(LogError::Io(std::io::Error::other(format!(
            "migration: staged v2 log failed verify; first issue: {:?}",
            verify_report.errors.first()
        ))));
    }

    // Move the original aside as `<log_path>.v1-backup`. Overwriting
    // any prior backup is intentional — the user re-running migrate
    // expects the most recent original preserved.
    if log_path.exists() {
        if backup_path.exists() {
            std::fs::remove_file(&backup_path)?;
        }
        std::fs::rename(log_path, &backup_path)?;
    }
    // Atomically promote the staged file to the live path.
    std::fs::rename(&staged_path, log_path)?;

    Ok(report)
}

/// Append a literal suffix to a [`Utf8Path`], producing a sibling path
/// in the same directory. Avoids `std::path::PathBuf` per the workspace
/// posture rule (`docs/SECURITY.md` §3 — camino-only file paths in
/// production code).
fn with_suffix(path: &Utf8Path, suffix: &str) -> Utf8PathBuf {
    let s = format!("{path}{suffix}");
    Utf8PathBuf::from(s)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Arc;
    use std::thread;

    use tempfile::TempDir;

    /// Convert a `TempDir`'s `&std::path::Path` to a `Utf8PathBuf`. Tests
    /// always run on UTF-8 temp paths in CI; if the OS returns a non-UTF-8
    /// path we panic, which is acceptable for a unit test.
    fn tmp_dir_utf8(dir: &TempDir) -> Utf8PathBuf {
        Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("temp dir path must be UTF-8")
    }

    /// A fixed 26-char ULID-shaped string used in tests. Real callers use
    /// the `ulid` crate; tests pin a constant so output is reproducible.
    const TEST_SESSION_ID: &str = "01JCKZ7Q0000000000000000AB";

    fn open_log(path: &Utf8Path) -> ProvenanceLog {
        ProvenanceLog::open(path, TEST_SESSION_ID.to_string()).expect("open")
    }

    fn empty_input() -> RowInput<'static> {
        RowInput {
            event: LogEvent::Fetch,
            result: LogResult::Ok,
            capability: Capability::Oa,
            ref_: None,
            source: None,
            error_code: None,
            size_bytes: None,
            license: None,
            store_path: None,
            canonical_digest: None,
        }
    }

    /// Read the on-disk log and parse every line into a `LogRow`.
    fn read_rows(path: &Utf8Path) -> Vec<LogRow> {
        let raw = fs::read_to_string(path).expect("read log");
        raw.lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str::<LogRow>(l).expect("valid LogRow"))
            .collect()
    }

    /// Recompute `this_hash` for a stored row and assert it matches the
    /// stored value. Walks the same canonicalization rule as
    /// [`compute_this_hash`].
    fn verify_this_hash(row: &LogRow) {
        let rfh = RowForHash {
            ts: row.ts,
            ts_seq: row.ts_seq,
            event: row.event,
            ref_: row.ref_.as_deref(),
            source: row.source.as_deref(),
            result: row.result,
            license: row.license.as_deref(),
            size_bytes: row.size_bytes,
            store_path: row.store_path.as_deref(),
            capability: row.capability,
            session_id: &row.session_id,
            error_code: row.error_code.as_deref(),
            schema_version: &row.schema_version,
            canonical_digest: row.canonical_digest.as_deref(),
            prev_hash: &row.prev_hash,
        };
        let recomputed = compute_this_hash(&rfh).expect("hash");
        assert_eq!(
            recomputed, row.this_hash,
            "this_hash mismatch on ts_seq {}",
            row.ts_seq
        );
    }

    #[test]
    fn first_row_uses_genesis_prev_hash() {
        let dir = TempDir::new().expect("tmp");
        let path = tmp_dir_utf8(&dir).join("log.jsonl");
        let log = open_log(&path);
        let seq = log.append(empty_input()).expect("append");
        assert_eq!(seq, 1);

        let rows = read_rows(&path);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].ts_seq, 1);
        assert_eq!(rows[0].prev_hash, GENESIS_HASH);
        assert_eq!(rows[0].this_hash.len(), 64);
        assert_eq!(rows[0].session_id, TEST_SESSION_ID);
        verify_this_hash(&rows[0]);
    }

    #[test]
    #[serial_test::serial] // global DOIGET_LOG_ROTATE_BYTES (#140 tests) must not interleave
    fn subsequent_rows_chain_correctly() {
        let dir = TempDir::new().expect("tmp");
        let path = tmp_dir_utf8(&dir).join("log.jsonl");
        let log = open_log(&path);

        for _ in 0..3 {
            log.append(empty_input()).expect("append");
        }

        let rows = read_rows(&path);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].prev_hash, GENESIS_HASH);
        assert_eq!(rows[1].prev_hash, rows[0].this_hash);
        assert_eq!(rows[2].prev_hash, rows[1].this_hash);
        for r in &rows {
            verify_this_hash(r);
        }
        assert_eq!(rows[0].ts_seq, 1);
        assert_eq!(rows[1].ts_seq, 2);
        assert_eq!(rows[2].ts_seq, 3);
    }

    #[test]
    #[serial_test::serial] // global DOIGET_LOG_ROTATE_BYTES (#140 tests) must not interleave
    fn recovery_after_reopen() {
        let dir = TempDir::new().expect("tmp");
        let path = tmp_dir_utf8(&dir).join("log.jsonl");

        {
            let log = open_log(&path);
            for _ in 0..3 {
                log.append(empty_input()).expect("append");
            }
        } // drop writer

        let log2 = open_log(&path);
        let seq = log2.append(empty_input()).expect("append after reopen");
        assert_eq!(seq, 4);

        let rows = read_rows(&path);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].prev_hash, GENESIS_HASH);
        for i in 1..rows.len() {
            assert_eq!(
                rows[i].prev_hash,
                rows[i - 1].this_hash,
                "chain break at row {}",
                i + 1
            );
        }
        for (i, r) in rows.iter().enumerate() {
            assert_eq!(r.ts_seq, (i + 1) as u64);
            verify_this_hash(r);
        }
    }

    #[test]
    #[serial_test::serial] // global DOIGET_LOG_ROTATE_BYTES (#140 tests) must not interleave
    fn concurrent_writers_in_same_process_serialize() {
        let dir = TempDir::new().expect("tmp");
        let path = tmp_dir_utf8(&dir).join("log.jsonl");
        let log = Arc::new(open_log(&path));

        let mut handles = Vec::with_capacity(8);
        for _ in 0..8 {
            let log = Arc::clone(&log);
            handles.push(thread::spawn(move || {
                log.append(empty_input()).expect("append")
            }));
        }
        let mut returned: Vec<u64> = handles
            .into_iter()
            .map(|h| h.join().expect("join"))
            .collect();
        returned.sort_unstable();
        assert_eq!(returned, vec![1, 2, 3, 4, 5, 6, 7, 8]);

        let rows = read_rows(&path);
        assert_eq!(rows.len(), 8);

        // The in-process mutex serializes appends, so file order MUST equal
        // ts_seq order: row N (0-indexed) on disk has ts_seq = N+1.
        for (i, r) in rows.iter().enumerate() {
            assert_eq!(r.ts_seq, (i + 1) as u64, "ts_seq gap at file row {}", i + 1);
        }
        // Hash chain follows file order.
        assert_eq!(rows[0].prev_hash, GENESIS_HASH);
        for i in 1..rows.len() {
            assert_eq!(
                rows[i].prev_hash,
                rows[i - 1].this_hash,
                "chain break at file row {}",
                i + 1
            );
        }
        for r in &rows {
            verify_this_hash(r);
        }
    }

    #[test]
    fn corrupted_existing_log_fails_open() {
        let dir = TempDir::new().expect("tmp");
        let path = tmp_dir_utf8(&dir).join("log.jsonl");

        // JSON but not a valid LogRow: missing required fields, has unknown
        // field. `deny_unknown_fields` ensures the parser refuses.
        fs::write(&path, "{\"ts_seq\": 1, \"garbage\": true}\n").expect("write");

        let err =
            ProvenanceLog::open(&path, TEST_SESSION_ID.to_string()).expect_err("must fail open");
        match err {
            LogError::Io(io) => {
                let msg = io.to_string();
                assert!(
                    msg.contains("corrupted log at line 1"),
                    "expected synthetic corruption message, got: {}",
                    msg
                );
            }
            other => panic!("expected LogError::Io, got {:?}", other),
        }
    }

    #[test]
    fn rejects_non_regular_file() {
        // Pointing the log at a directory must fail with NotARegularFile.
        let dir = TempDir::new().expect("tmp");
        let err = ProvenanceLog::open(tmp_dir_utf8(&dir), TEST_SESSION_ID.to_string())
            .expect_err("must fail");
        match err {
            LogError::NotARegularFile(_) => {}
            other => panic!("expected NotARegularFile, got {:?}", other),
        }
    }

    #[test]
    fn canonical_json_excludes_this_hash_field() {
        // Spec contract: the hashed bytes do not include `this_hash`. If
        // this ever regresses, every previously-written log becomes
        // unverifiable.
        let rfh = RowForHash {
            ts: Utc::now(),
            ts_seq: 1,
            event: LogEvent::Fetch,
            ref_: None,
            source: None,
            result: LogResult::Ok,
            license: None,
            size_bytes: None,
            store_path: None,
            capability: Capability::Oa,
            session_id: TEST_SESSION_ID,
            error_code: None,
            schema_version: LOG_SCHEMA_VERSION,
            canonical_digest: None,
            prev_hash: GENESIS_HASH,
        };
        let bytes = canonical_json_for_hash(&rfh).expect("canonicalize");
        let s = std::str::from_utf8(&bytes).expect("utf8");
        assert!(!s.contains("this_hash"), "this_hash leaked into hash input");
        assert!(s.contains("\"prev_hash\":"));
    }

    #[test]
    fn canonical_json_keys_are_lexicographically_sorted() {
        // PROVENANCE_LOG.md §4: canonical JSON uses keys sorted
        // lexicographically. The lex-first top-level key of a row is
        // `capability` ("c..." < "e..." < ...). Build a row and assert the
        // canonical bytes start with that key.
        let rfh = RowForHash {
            ts: Utc::now(),
            ts_seq: 1,
            event: LogEvent::Fetch,
            ref_: Some("10.1234/example"),
            source: Some("unpaywall"),
            result: LogResult::Ok,
            license: Some("CC-BY-4.0"),
            size_bytes: Some(1234),
            store_path: Some("papers/x.pdf"),
            capability: Capability::Oa,
            session_id: TEST_SESSION_ID,
            error_code: None,
            schema_version: LOG_SCHEMA_VERSION,
            canonical_digest: Some(
                "0000000000000000000000000000000000000000000000000000000000000000",
            ),
            prev_hash: GENESIS_HASH,
        };
        let bytes = canonical_json_for_hash(&rfh).expect("canonicalize");
        let s = std::str::from_utf8(&bytes).expect("utf8");
        // v2: lex-first key is `canonical_digest` (< `capability` because
        // 'n' < 'p' at byte index 2). Pre-v2 it was `capability`.
        assert!(
            s.starts_with("{\"canonical_digest\":"),
            "canonical bytes must start with lex-first v2 key, got: {}",
            s
        );
        // Spot-check ordering: `prev_hash` (p) must come before `ref` (r),
        // which must come before `result` (re...) — wait, "ref" < "result"
        // lexicographically because 'f' < 's' in ascii at index 2 vs 'e' at
        // index 2 of "result"... let me just check a couple of unambiguous
        // pairs: `event` < `prev_hash`, and `ts` < `ts_seq`.
        let event_idx = s.find("\"event\":").expect("event key present");
        let prev_idx = s.find("\"prev_hash\":").expect("prev_hash key present");
        assert!(event_idx < prev_idx, "event must precede prev_hash");
        let ts_idx = s.find("\"ts\":").expect("ts key present");
        let tsseq_idx = s.find("\"ts_seq\":").expect("ts_seq key present");
        assert!(ts_idx < tsseq_idx, "ts must precede ts_seq");
    }

    // -----------------------------------------------------------------
    // verify() tests — Phase 1 surface for `doiget audit-log --verify`.
    // -----------------------------------------------------------------

    /// Rewrite a single field's quoted-string value on a specific 1-based
    /// line of `path`. Used to simulate tampering. Panics on malformed input
    /// — only valid inputs are produced by the test harness.
    ///
    /// `field_key` is matched as `"field_key":"...old..."` (quoted string
    /// JSON value). The new value is the literal string `new_value` (no
    /// JSON escaping needed for the test fixtures we use).
    fn tamper_string_field(
        path: &Utf8Path,
        line_no_1based: usize,
        field_key: &str,
        new_value: &str,
    ) {
        let raw = fs::read_to_string(path).expect("read log");
        let mut lines: Vec<String> = raw.lines().map(str::to_string).collect();
        let target = &lines[line_no_1based - 1];
        let needle = format!("\"{field_key}\":\"");
        let start = target
            .find(&needle)
            .unwrap_or_else(|| panic!("field {field_key} not found on line {line_no_1based}"))
            + needle.len();
        let end_rel = target[start..]
            .find('"')
            .unwrap_or_else(|| panic!("unterminated string for field {field_key}"));
        let end = start + end_rel;
        let mut new_line = String::with_capacity(target.len());
        new_line.push_str(&target[..start]);
        new_line.push_str(new_value);
        new_line.push_str(&target[end..]);
        lines[line_no_1based - 1] = new_line;
        let mut out = lines.join("\n");
        out.push('\n');
        fs::write(path, out).expect("write tampered log");
    }

    #[test]
    fn verify_empty_log_is_ok() {
        // Missing file is a clean log — no tampering possible on bytes that
        // don't exist. `verify` returns an empty report, not an error.
        let dir = TempDir::new().expect("tmp");
        let path = tmp_dir_utf8(&dir).join("nonexistent.jsonl");
        assert!(!path.exists(), "precondition: file must not exist");

        let report = verify(&path).expect("verify must not error on missing file");
        assert_eq!(report.total_rows, 0);
        assert_eq!(report.ok_rows, 0);
        assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
    }

    #[test]
    #[serial_test::serial] // global DOIGET_LOG_ROTATE_BYTES (#140 tests) must not interleave
    fn verify_well_formed_chain_passes() {
        // Three rows written via the real writer must verify clean.
        let dir = TempDir::new().expect("tmp");
        let path = tmp_dir_utf8(&dir).join("log.jsonl");
        let log = open_log(&path);
        for _ in 0..3 {
            log.append(empty_input()).expect("append");
        }

        let report = verify(&path).expect("verify must succeed");
        assert_eq!(report.total_rows, 3);
        assert_eq!(report.ok_rows, 3);
        assert!(
            report.errors.is_empty(),
            "expected no issues on a well-formed log; got: {:?}",
            report.errors
        );
    }

    #[test]
    fn verify_detects_tampered_row_hash() {
        // Mutate the SECOND row's `this_hash` to a syntactically-valid but
        // wrong hash. The recomputed canonical-JSON SHA-256 will not match.
        let dir = TempDir::new().expect("tmp");
        let path = tmp_dir_utf8(&dir).join("log.jsonl");
        let log = open_log(&path);
        log.append(empty_input()).expect("append 1");
        log.append(empty_input()).expect("append 2");
        drop(log);

        // 64 lowercase hex chars, all zeros — passes `LogRow` parse, fails hash check.
        tamper_string_field(
            &path,
            2,
            "this_hash",
            "0000000000000000000000000000000000000000000000000000000000000000",
        );

        let report = verify(&path).expect("verify must succeed");
        assert_eq!(report.total_rows, 2);
        // Row 2's hash mismatch breaks both the hash check on row 2 AND the
        // chain link from row 2's stored `prev_hash` (still correct) into the
        // forward direction. There's no row 3 to fail forward, so we expect
        // exactly one issue: the this-hash mismatch on line 2.
        let hash_issues: Vec<_> = report
            .errors
            .iter()
            .filter(|e| e.kind == VerifyIssueKind::ThisHashMismatch)
            .collect();
        assert_eq!(
            hash_issues.len(),
            1,
            "expected exactly one ThisHashMismatch, got {:?}",
            report.errors
        );
        assert_eq!(hash_issues[0].line, 2);
    }

    #[test]
    fn verify_detects_tampered_prev_hash() {
        // Mutate the SECOND row's `prev_hash` to a wrong value. This
        // invalidates the chain link but the row's own `this_hash` was
        // computed with the original `prev_hash`, so the this-hash check
        // ALSO fails (hash input changed). We assert at least the prev-hash
        // issue is reported on line 2.
        let dir = TempDir::new().expect("tmp");
        let path = tmp_dir_utf8(&dir).join("log.jsonl");
        let log = open_log(&path);
        log.append(empty_input()).expect("append 1");
        log.append(empty_input()).expect("append 2");
        drop(log);

        tamper_string_field(
            &path,
            2,
            "prev_hash",
            "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        );

        let report = verify(&path).expect("verify must succeed");
        assert_eq!(report.total_rows, 2);
        let prev_issues: Vec<_> = report
            .errors
            .iter()
            .filter(|e| e.kind == VerifyIssueKind::PrevHashMismatch)
            .collect();
        assert_eq!(
            prev_issues.len(),
            1,
            "expected exactly one PrevHashMismatch, got {:?}",
            report.errors
        );
        assert_eq!(prev_issues[0].line, 2);
    }

    #[test]
    fn verify_detects_corrupted_json() {
        // One valid row plus a literal `{"garbage":true}` line. The garbage
        // line fails `serde_json::from_str::<LogRow>` (missing fields +
        // `deny_unknown_fields`) and surfaces as a `ParseError` on line 2.
        let dir = TempDir::new().expect("tmp");
        let path = tmp_dir_utf8(&dir).join("log.jsonl");
        let log = open_log(&path);
        log.append(empty_input()).expect("append 1");
        drop(log);

        // Append a garbage line directly.
        let mut existing = fs::read_to_string(&path).expect("read");
        if !existing.ends_with('\n') {
            existing.push('\n');
        }
        existing.push_str("{\"garbage\":true}\n");
        fs::write(&path, existing).expect("write");

        let report = verify(&path).expect("verify must succeed");
        // total_rows counts non-empty lines, so both lines are counted.
        assert_eq!(report.total_rows, 2);
        let parse_issues: Vec<_> = report
            .errors
            .iter()
            .filter(|e| e.kind == VerifyIssueKind::ParseError)
            .collect();
        assert_eq!(
            parse_issues.len(),
            1,
            "expected exactly one ParseError, got {:?}",
            report.errors
        );
        assert_eq!(parse_issues[0].line, 2);
    }

    #[test]
    fn capability_serializes_kebab_case() {
        // PROVENANCE_LOG.md §3 requires `oa`, `metadata`, `tdm-elsevier`,
        // `tdm-aps`, `tdm-springer` on the wire (kebab-case).
        let cases = [
            (Capability::Oa, "\"oa\""),
            (Capability::Metadata, "\"metadata\""),
            (Capability::TdmElsevier, "\"tdm-elsevier\""),
            (Capability::TdmAps, "\"tdm-aps\""),
            (Capability::TdmSpringer, "\"tdm-springer\""),
        ];
        for (cap, expected) in cases {
            let got = serde_json::to_string(&cap).expect("serialize");
            assert_eq!(
                got, expected,
                "capability wire format mismatch for {:?}",
                cap
            );
        }
    }

    // -----------------------------------------------------------------
    // #140 — §6 rotation, retention, multi-segment verify.
    // -----------------------------------------------------------------

    fn gunzip_to_string(gz: &Utf8Path) -> String {
        use std::io::Read;
        let f = std::fs::File::open(gz.as_std_path()).expect("open gz");
        let mut dec = GzDecoder::new(f);
        let mut s = String::new();
        dec.read_to_string(&mut s).expect("gunzip");
        s
    }

    #[test]
    #[serial_test::serial]
    fn rotation_archives_to_gz_and_restarts_genesis_chain() {
        let dir = TempDir::new().expect("tmp");
        let path = tmp_dir_utf8(&dir).join("access.log");
        // Tiny threshold: row 1 fits, so the SECOND append (size>=50)
        // triggers rotation before it writes.
        std::env::set_var("DOIGET_LOG_ROTATE_BYTES", "50");
        std::env::set_var("DOIGET_LOG_RETENTION_DAYS", "0"); // don't prune in-test

        let log = open_log(&path);
        log.append(empty_input()).expect("append 1");
        let row1 = read_rows(&path);
        assert_eq!(row1.len(), 1);
        assert_eq!(row1[0].prev_hash, GENESIS_HASH);

        log.append(empty_input()).expect("append 2 (rotates first)");

        // Exactly one rotated segment; it gunzips to the original row 1.
        let segs = rotated_segments(&path);
        assert_eq!(segs.len(), 1, "one .gz segment expected; got {segs:?}");
        let archived: Vec<LogRow> = gunzip_to_string(&segs[0])
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).expect("row"))
            .collect();
        assert_eq!(archived.len(), 1);
        assert_eq!(archived[0].this_hash, row1[0].this_hash);

        // The fresh access.log restarts the chain at GENESIS, ts_seq 1.
        let cur = read_rows(&path);
        assert_eq!(cur.len(), 1, "fresh segment holds only the post-rotate row");
        assert_eq!(cur[0].prev_hash, GENESIS_HASH);
        assert_eq!(cur[0].ts_seq, 1);

        // verify_all sees both segments, each its own clean chain.
        let reports = verify_all(&path).expect("verify_all");
        assert_eq!(reports.len(), 2, "rotated .gz + current");
        for (p, r) in &reports {
            assert!(r.errors.is_empty(), "segment {p} must verify clean: {r:?}");
        }

        std::env::remove_var("DOIGET_LOG_ROTATE_BYTES");
        std::env::remove_var("DOIGET_LOG_RETENTION_DAYS");
    }

    #[test]
    fn rotate_log_is_fail_closed_on_missing_source() {
        // The append path propagates this via `?`, so a rotation failure
        // aborts the fetch (fail-closed) rather than silently continuing.
        let dir = TempDir::new().expect("tmp");
        let missing = tmp_dir_utf8(&dir).join("nope.log");
        let err = rotate_log(&missing).expect_err("missing source must error");
        assert!(matches!(err, LogError::Io(_)), "got {err:?}");
    }

    #[test]
    #[serial_test::serial]
    fn prune_respects_retention_window_and_disable() {
        let dir = TempDir::new().expect("tmp");
        let base = tmp_dir_utf8(&dir);
        let path = base.join("access.log");
        let old_gz = base.join("access.log.2020-01-01-000000.gz");
        let new_gz = base.join("access.log.2999-01-01-000000.gz");

        let mk = |p: &Utf8Path, aged: bool| {
            let f = std::fs::File::create(p.as_std_path()).expect("create gz");
            if aged {
                // 100 days ago — older than the 90-day default & a 1-day window.
                let when =
                    std::time::SystemTime::now() - std::time::Duration::from_secs(100 * 86_400);
                f.set_modified(when).expect("set mtime");
            }
        };

        // (a) days=0 disables pruning entirely.
        mk(&old_gz, true);
        std::env::set_var("DOIGET_LOG_RETENTION_DAYS", "0");
        let _ = open_log(&path);
        assert!(old_gz.exists(), "days=0 must NOT prune");

        // (b) days=1 prunes the aged segment, keeps a fresh one.
        mk(&new_gz, false);
        std::env::set_var("DOIGET_LOG_RETENTION_DAYS", "1");
        let _ = open_log(&path);
        assert!(!old_gz.exists(), "aged segment must be pruned at days=1");
        assert!(new_gz.exists(), "fresh segment must survive");

        std::env::remove_var("DOIGET_LOG_RETENTION_DAYS");
    }

    #[test]
    #[serial_test::serial]
    fn retention_days_env_parsing() {
        std::env::set_var("DOIGET_LOG_RETENTION_DAYS", "0");
        assert_eq!(retention_days(), 0);
        std::env::set_var("DOIGET_LOG_RETENTION_DAYS", "30");
        assert_eq!(retention_days(), 30);
        std::env::set_var("DOIGET_LOG_RETENTION_DAYS", "garbage");
        assert_eq!(retention_days(), DEFAULT_RETENTION_DAYS);
        std::env::set_var("DOIGET_LOG_RETENTION_DAYS", "-5");
        assert_eq!(retention_days(), DEFAULT_RETENTION_DAYS);
        std::env::remove_var("DOIGET_LOG_RETENTION_DAYS");
        assert_eq!(retention_days(), DEFAULT_RETENTION_DAYS);
    }

    #[test]
    #[serial_test::serial]
    fn verify_all_flags_tampered_segment_independently() {
        let dir = TempDir::new().expect("tmp");
        let path = tmp_dir_utf8(&dir).join("access.log");
        std::env::set_var("DOIGET_LOG_ROTATE_BYTES", "50");
        std::env::set_var("DOIGET_LOG_RETENTION_DAYS", "0");

        let log = open_log(&path);
        log.append(empty_input()).expect("append 1");
        log.append(empty_input()).expect("append 2 (rotates)");
        drop(log);

        // Tamper the CURRENT segment's row (flip a hex char in this_hash).
        let mut cur = read_rows(&path);
        let mut bad = cur.remove(0);
        bad.this_hash = format!("{}0", &bad.this_hash[..bad.this_hash.len() - 1]);
        std::fs::write(
            path.as_std_path(),
            format!("{}\n", serde_json::to_string(&bad).expect("ser")),
        )
        .expect("rewrite tampered current");

        let reports = verify_all(&path).expect("verify_all");
        assert_eq!(reports.len(), 2);
        // Oldest first = the rotated .gz (clean); current last (tampered).
        let (gz_path, gz_rep) = &reports[0];
        let (cur_path, cur_rep) = &reports[1];
        assert!(
            gz_path.as_str().ends_with(".gz") && gz_rep.errors.is_empty(),
            "rotated segment must stay clean: {gz_path} {gz_rep:?}"
        );
        assert!(
            cur_path.file_name() == Some("access.log") && !cur_rep.errors.is_empty(),
            "tampered current segment must report issues: {cur_path} {cur_rep:?}"
        );

        std::env::remove_var("DOIGET_LOG_ROTATE_BYTES");
        std::env::remove_var("DOIGET_LOG_RETENTION_DAYS");
    }
}
