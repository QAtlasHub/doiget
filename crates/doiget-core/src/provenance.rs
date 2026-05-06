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
//! # TODO: log rotation (§6)
//!
//! Log rotation is not yet implemented. When it lands, the first row of the
//! NEW log file MUST use `prev_hash = "GENESIS"` (chain restart), matching
//! the `GENESIS_HASH` constant below.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::sync::Mutex;

use camino::{Utf8Path, Utf8PathBuf};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// One row of the provenance log (PROVENANCE_LOG.md §3).
///
/// The on-disk wire field names match the spec table; struct-field order is
/// **not** load-bearing for the hash because canonicalization sorts keys
/// lexicographically (see PROVENANCE_LOG.md §4).
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
    /// 64 lowercase hex chars, OR the literal string `"GENESIS"` for the
    /// first row of a fresh log file.
    pub prev_hash: String,
    /// 64 lowercase hex chars. SHA-256 of canonical JSON of THIS row with
    /// the `this_hash` field removed. See module docs.
    pub this_hash: String,
}

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
/// first row after a log rotation (TODO: rotation not yet implemented).
const GENESIS_HASH: &str = "GENESIS";

/// Caller-supplied fields for a row. The writer fills in `ts`, `ts_seq`,
/// `session_id`, `prev_hash`, and `this_hash`.
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
// `"ts"` < `"ts_seq"` lexicographically. Similarly, the lex-first top-level
// key of a full row is `"capability"`.
// ---------------------------------------------------------------------------

/// Serializable shadow of [`LogRow`] **without** `this_hash`. Used solely as
/// an intermediate to compute the canonical bytes that `this_hash` is the
/// SHA-256 of. The wire key names match [`LogRow`]'s `serde` attributes.
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

        Ok(Self {
            path,
            state: Mutex::new(LogState {
                next_seq,
                last_hash,
            }),
            session_id,
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
///   advance through an unparseable row, so the next valid row's `prev_hash`
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
                // Chain anchor cannot advance through an unparseable row;
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
            prev_hash: GENESIS_HASH,
        };
        let bytes = canonical_json_for_hash(&rfh).expect("canonicalize");
        let s = std::str::from_utf8(&bytes).expect("utf8");
        assert!(
            s.starts_with("{\"capability\":"),
            "canonical bytes must start with lex-first key, got: {}",
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
}
