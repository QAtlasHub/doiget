//! JSON Lines + SHA-256 hash-chained provenance log.
//!
//! Binding spec: `docs/PROVENANCE_LOG.md`. Failure semantics: **fail-closed** —
//! callers MUST abort the fetch if a log write returns `Err`. See
//! `docs/SECURITY.md` §1.8 and ADR-0006.
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
//! # Hash chain
//!
//! Each row carries a `prev_hash` and a `row_hash`. The first row's
//! `prev_hash` is the literal 64-char string `"0".repeat(64)`. Every
//! subsequent row's `prev_hash` MUST equal the previous row's `row_hash`.
//!
//! `row_hash` is computed as:
//!
//! ```text
//! row_hash = lower_hex(SHA-256(canonical_json(row \ {row_hash})))
//! ```
//!
//! where `canonical_json` is the bytes produced by `serde_json::to_string`
//! over a struct that contains every row field **except** `row_hash`, in the
//! exact field order declared in `LogRow` / `RowForHash` below. There is no
//! whitespace and no key sorting beyond the deterministic field order serde
//! produces for struct serialization. Downstream `audit-log --verify` (Phase
//! 1+) relies on this exact rule — do not reorder, rename, or insert fields
//! without bumping the spec.
//!
//! # In-process serialization
//!
//! `ProvenanceLog` holds a `Mutex<LogState>`. All `append` calls within the
//! same process serialize on this mutex, satisfying the "process-local mutex
//! on log appender" requirement of `docs/SECURITY.md` §1.8. Cross-process
//! coordination (multiple `doiget` invocations) is out of scope here and
//! handled by the higher-level `flock`-based store layer.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::sync::Mutex;

use camino::{Utf8Path, Utf8PathBuf};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// One row of the provenance log.
///
/// Field order matches `docs/PROVENANCE_LOG.md` §3 and is the canonical
/// serialization order used for hashing. **Do not reorder fields** — doing
/// so silently invalidates every previously-written `row_hash`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogRow {
    /// Per-log monotonic sequence, starting at 1.
    pub seq: u64,
    /// RFC3339 UTC timestamp of the append.
    pub ts: DateTime<Utc>,
    /// Event class (see [`LogEvent`]).
    pub event: LogEvent,
    /// Optional reference (DOI / arXiv id). Wire field name is `ref`.
    #[serde(rename = "ref")]
    pub ref_: Option<String>,
    /// Optional source name (e.g. `unpaywall`).
    pub source: Option<String>,
    /// Status (see [`LogStatus`]).
    pub status: LogStatus,
    /// Stable error code on failure rows.
    pub error_code: Option<String>,
    /// Bytes written / fetched, on success rows.
    pub size_bytes: Option<u64>,
    /// Filesystem-safe key (see `docs/SAFEKEY.md`).
    pub safekey: Option<String>,
    /// MCP request id, when invoked via the MCP server.
    pub mcp_call_id: Option<String>,
    /// Hostname of the machine that wrote this row. **No PID** —
    /// PIDs are not stable identifiers and would leak process-restart cadence.
    pub host: String,
    /// 64 lowercase hex chars. `"0".repeat(64)` for the first row of a log.
    pub prev_hash: String,
    /// 64 lowercase hex chars. SHA-256 of canonical JSON of THIS row with
    /// the `row_hash` field removed. See module docs.
    pub row_hash: String,
}

/// Event class for a log row. `non_exhaustive` so adding new variants is
/// non-breaking.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LogEvent {
    /// A fetch attempt has begun.
    FetchStart,
    /// Fetch completed successfully.
    FetchOk,
    /// Fetch failed.
    FetchErr,
    /// Store write completed successfully.
    StoreWriteOk,
    /// Store write failed.
    StoreWriteErr,
    /// A previous log file was rotated (chain restart marker).
    LogRotated,
}

/// Status field. `non_exhaustive` for forward compatibility.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LogStatus {
    /// The operation succeeded.
    Ok,
    /// The operation failed.
    Err,
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
    host: String,
}

/// Mutable internal state, guarded by [`ProvenanceLog::state`].
#[derive(Debug)]
struct LogState {
    /// `seq` of the **next** row to be appended.
    next_seq: u64,
    /// 64 lowercase hex chars; `"0".repeat(64)` if the log is empty.
    last_hash: String,
}

/// The genesis hash used as `prev_hash` for the first row of a log.
const GENESIS_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// Caller-supplied fields for a row. The writer fills in `seq`, `ts`, `host`,
/// `prev_hash`, and `row_hash`.
#[derive(Debug, Clone)]
pub struct RowInput<'a> {
    /// Event class.
    pub event: LogEvent,
    /// Status.
    pub status: LogStatus,
    /// Optional DOI / arXiv id.
    pub ref_: Option<&'a str>,
    /// Optional source name.
    pub source: Option<&'a str>,
    /// Optional error code on failure rows.
    pub error_code: Option<&'a str>,
    /// Optional payload size in bytes.
    pub size_bytes: Option<u64>,
    /// Optional safekey (`docs/SAFEKEY.md`).
    pub safekey: Option<&'a str>,
    /// Optional MCP call id.
    pub mcp_call_id: Option<&'a str>,
}

// ---------------------------------------------------------------------------
// Canonical-JSON helper
//
// Hashing rule (CRITICAL — this is the spec contract for `audit-log --verify`):
//
//   row_hash = lower_hex(SHA-256(serde_json::to_string(RowForHash { ... })))
//
// where `RowForHash` is a struct with EVERY field of `LogRow` EXCEPT
// `row_hash`, in the same declaration order (seq, ts, event, ref_, source,
// status, error_code, size_bytes, safekey, mcp_call_id, host, prev_hash).
//
// `serde_json::to_string` emits no whitespace and walks struct fields in
// declaration order, so this is deterministic without sorting. UTF-8.
// Lowercase hex output. 64 chars.
// ---------------------------------------------------------------------------

/// Serializable shadow of [`LogRow`] **without** `row_hash`. Used solely to
/// compute the canonical bytes that `row_hash` is the SHA-256 of.
#[derive(Serialize)]
struct RowForHash<'a> {
    seq: u64,
    ts: DateTime<Utc>,
    event: LogEvent,
    #[serde(rename = "ref")]
    ref_: Option<&'a str>,
    source: Option<&'a str>,
    status: LogStatus,
    error_code: Option<&'a str>,
    size_bytes: Option<u64>,
    safekey: Option<&'a str>,
    mcp_call_id: Option<&'a str>,
    host: &'a str,
    prev_hash: &'a str,
}

/// Compute `row_hash` for the given row-without-hash. Returns 64 lowercase hex
/// chars.
fn compute_row_hash(rfh: &RowForHash<'_>) -> Result<String, LogError> {
    let bytes = serde_json::to_vec(rfh)?;
    let digest = Sha256::digest(&bytes);
    Ok(hex::encode(digest))
}

impl ProvenanceLog {
    /// Open or create the log at `path`.
    ///
    /// If the file exists, scan it once to recover the last `seq` and
    /// `row_hash`. If the file is missing or empty, the first row will use
    /// `prev_hash = "0".repeat(64)` and `seq = 1`.
    ///
    /// # Errors
    ///
    /// Returns [`LogError::Io`] for I/O failures or if any line fails to
    /// parse as a [`LogRow`] (synthetic message: `"corrupted log at line N: …"`).
    /// The writer never silently truncates a corrupt log.
    ///
    /// Returns [`LogError::NotARegularFile`] if `path` exists but is not a
    /// regular file (e.g. a directory).
    pub fn open(path: impl Into<Utf8PathBuf>) -> Result<Self, LogError> {
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
            host: detect_host(),
        })
    }

    /// Append a row. Computes `prev_hash`, `seq`, and `row_hash`; the caller
    /// only supplies the semantic fields via [`RowInput`].
    ///
    /// Returns the assigned `seq` on success.
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

        let seq = state.next_seq;
        let prev_hash = state.last_hash.clone();
        let ts = Utc::now();

        let rfh = RowForHash {
            seq,
            ts,
            event: input.event,
            ref_: input.ref_,
            source: input.source,
            status: input.status,
            error_code: input.error_code,
            size_bytes: input.size_bytes,
            safekey: input.safekey,
            mcp_call_id: input.mcp_call_id,
            host: &self.host,
            prev_hash: &prev_hash,
        };

        let row_hash = compute_row_hash(&rfh)?;

        // Build the on-disk row. Owned strings here because `LogRow` does
        // not borrow.
        let row = LogRow {
            seq,
            ts,
            event: input.event,
            ref_: input.ref_.map(str::to_string),
            source: input.source.map(str::to_string),
            status: input.status,
            error_code: input.error_code.map(str::to_string),
            size_bytes: input.size_bytes,
            safekey: input.safekey.map(str::to_string),
            mcp_call_id: input.mcp_call_id.map(str::to_string),
            host: self.host.clone(),
            prev_hash,
            row_hash: row_hash.clone(),
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
        // same `(seq, prev_hash)` — at most a torn last line on disk.
        state.next_seq = seq + 1;
        state.last_hash = row_hash;

        Ok(seq)
    }

    /// Returns the path the log was opened at. Useful for tests and audit tooling.
    pub fn path(&self) -> &Utf8Path {
        &self.path
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
        last_seq = row.seq;
        last_hash = row.row_hash;
    }

    if last_seq == 0 {
        Ok((1, GENESIS_HASH.to_string()))
    } else {
        Ok((last_seq + 1, last_hash))
    }
}

/// Best-effort hostname detection. Falls back to `"unknown"` if the OS does
/// not expose one. Matches the `host` field semantics in
/// `docs/PROVENANCE_LOG.md` (no PID, hostname only).
fn detect_host() -> String {
    // Deliberately avoid pulling in the `gethostname` crate to keep the
    // dependency surface minimal; this small env fallback is sufficient for
    // the spec ("hostname, no PID") and works on both POSIX and Windows.
    if let Ok(h) = std::env::var("HOSTNAME") {
        if !h.is_empty() {
            return h;
        }
    }
    if let Ok(h) = std::env::var("COMPUTERNAME") {
        if !h.is_empty() {
            return h;
        }
    }
    "unknown".to_string()
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

    fn empty_input() -> RowInput<'static> {
        RowInput {
            event: LogEvent::FetchStart,
            status: LogStatus::Ok,
            ref_: None,
            source: None,
            error_code: None,
            size_bytes: None,
            safekey: None,
            mcp_call_id: None,
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

    /// Recompute `row_hash` for a stored row and assert it matches the
    /// stored value. Walks the same canonicalization rule as `compute_row_hash`.
    fn verify_row_hash(row: &LogRow) {
        let rfh = RowForHash {
            seq: row.seq,
            ts: row.ts,
            event: row.event,
            ref_: row.ref_.as_deref(),
            source: row.source.as_deref(),
            status: row.status,
            error_code: row.error_code.as_deref(),
            size_bytes: row.size_bytes,
            safekey: row.safekey.as_deref(),
            mcp_call_id: row.mcp_call_id.as_deref(),
            host: &row.host,
            prev_hash: &row.prev_hash,
        };
        let recomputed = compute_row_hash(&rfh).expect("hash");
        assert_eq!(
            recomputed, row.row_hash,
            "row_hash mismatch on seq {}",
            row.seq
        );
    }

    #[test]
    fn first_row_uses_zero_prev_hash() {
        let dir = TempDir::new().expect("tmp");
        let path = tmp_dir_utf8(&dir).join("log.jsonl");
        let log = ProvenanceLog::open(&path).expect("open");
        let seq = log.append(empty_input()).expect("append");
        assert_eq!(seq, 1);

        let rows = read_rows(&path);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].seq, 1);
        assert_eq!(rows[0].prev_hash, GENESIS_HASH);
        assert_eq!(rows[0].prev_hash.len(), 64);
        assert_eq!(rows[0].row_hash.len(), 64);
        verify_row_hash(&rows[0]);
    }

    #[test]
    fn subsequent_rows_chain_correctly() {
        let dir = TempDir::new().expect("tmp");
        let path = tmp_dir_utf8(&dir).join("log.jsonl");
        let log = ProvenanceLog::open(&path).expect("open");

        for _ in 0..3 {
            log.append(empty_input()).expect("append");
        }

        let rows = read_rows(&path);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].prev_hash, GENESIS_HASH);
        assert_eq!(rows[1].prev_hash, rows[0].row_hash);
        assert_eq!(rows[2].prev_hash, rows[1].row_hash);
        for r in &rows {
            verify_row_hash(r);
        }
        assert_eq!(rows[0].seq, 1);
        assert_eq!(rows[1].seq, 2);
        assert_eq!(rows[2].seq, 3);
    }

    #[test]
    fn recovery_after_reopen() {
        let dir = TempDir::new().expect("tmp");
        let path = tmp_dir_utf8(&dir).join("log.jsonl");

        {
            let log = ProvenanceLog::open(&path).expect("open");
            for _ in 0..3 {
                log.append(empty_input()).expect("append");
            }
        } // drop writer

        let log2 = ProvenanceLog::open(&path).expect("reopen");
        let seq = log2.append(empty_input()).expect("append after reopen");
        assert_eq!(seq, 4);

        let rows = read_rows(&path);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].prev_hash, GENESIS_HASH);
        for i in 1..rows.len() {
            assert_eq!(
                rows[i].prev_hash,
                rows[i - 1].row_hash,
                "chain break at row {}",
                i + 1
            );
        }
        for (i, r) in rows.iter().enumerate() {
            assert_eq!(r.seq, (i + 1) as u64);
            verify_row_hash(r);
        }
    }

    #[test]
    fn concurrent_writers_in_same_process_serialize() {
        let dir = TempDir::new().expect("tmp");
        let path = tmp_dir_utf8(&dir).join("log.jsonl");
        let log = Arc::new(ProvenanceLog::open(&path).expect("open"));

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
        // seq order: row N (0-indexed) on disk has seq = N+1.
        for (i, r) in rows.iter().enumerate() {
            assert_eq!(r.seq, (i + 1) as u64, "seq gap at file row {}", i + 1);
        }
        // Hash chain follows file order.
        assert_eq!(rows[0].prev_hash, GENESIS_HASH);
        for i in 1..rows.len() {
            assert_eq!(
                rows[i].prev_hash,
                rows[i - 1].row_hash,
                "chain break at file row {}",
                i + 1
            );
        }
        for r in &rows {
            verify_row_hash(r);
        }
    }

    #[test]
    fn corrupted_existing_log_fails_open() {
        let dir = TempDir::new().expect("tmp");
        let path = tmp_dir_utf8(&dir).join("log.jsonl");

        // JSON but not a valid LogRow: missing required fields, has unknown
        // field. `deny_unknown_fields` ensures the parser refuses.
        fs::write(&path, "{\"seq\": 1, \"garbage\": true}\n").expect("write");

        let err = ProvenanceLog::open(&path).expect_err("must fail open");
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
        let err = ProvenanceLog::open(tmp_dir_utf8(&dir)).expect_err("must fail");
        match err {
            LogError::NotARegularFile(_) => {}
            other => panic!("expected NotARegularFile, got {:?}", other),
        }
    }

    #[test]
    fn canonical_json_excludes_row_hash_field() {
        // Spec contract: the hashed bytes do not include `row_hash`. If this
        // ever regresses, every previously-written log becomes unverifiable.
        let rfh = RowForHash {
            seq: 1,
            ts: Utc::now(),
            event: LogEvent::FetchStart,
            ref_: None,
            source: None,
            status: LogStatus::Ok,
            error_code: None,
            size_bytes: None,
            safekey: None,
            mcp_call_id: None,
            host: "h",
            prev_hash: GENESIS_HASH,
        };
        let bytes = serde_json::to_vec(&rfh).expect("serialize");
        let s = std::str::from_utf8(&bytes).expect("utf8");
        assert!(!s.contains("row_hash"), "row_hash leaked into hash input");
        assert!(s.contains("\"prev_hash\":"));
    }
}
