//! Filesystem-backed [`Store`] implementation.
//!
//! Binding spec: `docs/STORE.md` §§1-7. Re-stated as the implementation
//! contract:
//!
//! - **§1 Layout:** `<root>/<safekey>.pdf` and `<root>/.metadata/<safekey>.toml`,
//!   `.toml.lock` siblings for advisory locking.
//! - **§3 Schema version policy:** parse `<MAJOR>.<MINOR>`. Future `MAJOR`
//!   yields [`StoreError::SchemaTooNew`] on writes, warn-and-tolerate on
//!   reads. Future `MINOR` (same major) yields a `tracing::warn!`-then-OK on
//!   reads.
//! - **§4 Lock protocol:** `flock` (`fs2::FileExt`) on the SEPARATE
//!   `.toml.lock` file with a 5 s timeout polled via `try_lock_*`.
//! - **§5 Atomic write:** write `<safekey>.toml.tmp` → `sync_all` → `rename`
//!   → fsync parent dir (POSIX). On Windows `std::fs::rename` invokes
//!   `MoveFileEx` with `MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH`,
//!   so no extra parent-fsync syscall is required. Each file is atomic
//!   individually; there is no cross-file transaction. The PDF is
//!   therefore written BEFORE the metadata that references it (issue
//!   #122), so a crash between the two renames can only leave an orphan
//!   PDF or the prior consistent entry — never metadata pointing at a
//!   missing PDF.
//! - **§6 Coexistence with BiblioFetch.jl:** when re-writing an existing
//!   entry, reserved top-level fields previously present are NOT overwritten
//!   if the new value differs. Only the `[doiget]` table and `other` are
//!   updated freely.
//! - **§7 Normalization:** alphabetical key order, `\n` line endings,
//!   trailing newline. Implemented through `BTreeMap`-backed re-serialization
//!   of the on-wire `toml::Value`.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::time::{Duration, Instant};

use camino::{Utf8Path, Utf8PathBuf};
use fs2::FileExt;
use tracing::warn;

use super::metadata::{DoigetExtension, Metadata};
use super::{EntryInfo, Store, StoreError};
use crate::{Safekey, SCHEMA_VERSION};

/// Subdirectory under `<root>` that holds metadata TOML files and their
/// advisory lock siblings, per `docs/STORE.md` §1.
const METADATA_DIR: &str = ".metadata";

/// Lock-acquisition timeout per `docs/STORE.md` §4 (5 seconds).
const LOCK_TIMEOUT: Duration = Duration::from_secs(5);

/// How long to back off between `try_lock_*` polls. Small relative to
/// [`LOCK_TIMEOUT`] so a contended writer in the common case sees the lock
/// released within ~50 ms.
const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Filesystem-shaped [`Store`] implementation rooted at `<root>`.
#[derive(Debug, Clone)]
pub struct FsStore {
    root: Utf8PathBuf,
    metadata_dir: Utf8PathBuf,
}

impl FsStore {
    /// Open or create a store at `root`.
    ///
    /// Creates `<root>/` and `<root>/.metadata/` if missing. On POSIX, both
    /// directories are created with mode `0700` (owner-only). On Windows,
    /// directory ACLs are inherited (no-op).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Io`] if `root` exists but is not a directory,
    /// or if directory creation fails.
    pub fn new(root: Utf8PathBuf) -> Result<Self, StoreError> {
        // Reject non-directory existing paths up front; `create_dir_all` on
        // a regular-file path returns a confusing platform-dependent error.
        if root.exists() && !root.is_dir() {
            return Err(StoreError::Io(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("store root {} exists but is not a directory", root),
            )));
        }
        let metadata_dir = root.join(METADATA_DIR);

        create_dir_secure(root.as_std_path())?;
        create_dir_secure(metadata_dir.as_std_path())?;

        Ok(Self { root, metadata_dir })
    }

    /// Returns the store root.
    pub fn root(&self) -> &Utf8Path {
        &self.root
    }

    /// Resolve the metadata-TOML path for `key`, with a defense-in-depth
    /// path-traversal check.
    ///
    /// `Safekey` construction already restricts the inner string to
    /// `[A-Za-z0-9._-]` per `docs/SAFEKEY.md`. The check below catches
    /// hand-crafted `Safekey` values produced by in-crate `pub(crate)`
    /// shortcuts (e.g. tests) and any future regression in the safekey
    /// charset.
    fn metadata_path(&self, key: &Safekey) -> Result<Utf8PathBuf, StoreError> {
        guard_safekey(key.as_str())?;
        let p = self.metadata_dir.join(format!("{}.toml", key.as_str()));
        // Final paranoia: parent must equal `metadata_dir`. After the charset
        // check above this should always hold; if it ever does not, surface
        // it as `PathTraversal` rather than panicking.
        if p.parent() != Some(self.metadata_dir.as_path()) {
            return Err(StoreError::PathTraversal { path: p });
        }
        Ok(p)
    }

    fn lock_path(&self, key: &Safekey) -> Result<Utf8PathBuf, StoreError> {
        guard_safekey(key.as_str())?;
        Ok(self
            .metadata_dir
            .join(format!("{}.toml.lock", key.as_str())))
    }

    fn pdf_path(&self, key: &Safekey) -> Result<Utf8PathBuf, StoreError> {
        guard_safekey(key.as_str())?;
        Ok(self.root.join(format!("{}.pdf", key.as_str())))
    }

    /// Return up to `limit` entries whose `[doiget].tags` list contains `tag`
    /// (case-sensitive exact match). If `query` is non-empty, also apply a
    /// case-insensitive substring filter over title / authors / venue /
    /// publisher. An empty `query` matches all tagged entries.
    pub fn search_by_tag(
        &self,
        tag: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<EntryInfo>, StoreError> {
        let q = query.to_lowercase();
        let mut hits = Vec::new();
        for path in metadata_files(&self.metadata_dir)? {
            let raw = std::fs::read_to_string(path.as_std_path())?;
            let Ok(md) = toml::from_str::<Metadata>(&raw) else {
                continue;
            };
            let has_tag = md
                .doiget
                .as_ref()
                .is_some_and(|d| d.tags.iter().any(|t| t == tag));
            if !has_tag {
                continue;
            }
            if !q.is_empty() {
                let haystacks = [
                    md.title.to_lowercase(),
                    md.authors.join(" ").to_lowercase(),
                    md.venue.clone().unwrap_or_default().to_lowercase(),
                    md.publisher.clone().unwrap_or_default().to_lowercase(),
                ];
                if !haystacks.iter().any(|h| h.contains(&q)) {
                    continue;
                }
            }
            let safekey = safekey_from_metadata_filename(&path);
            hits.push(EntryInfo {
                safekey,
                title: md.title,
                year: md.year,
                fetched_at: md.doiget.as_ref().map(|d| d.fetched_at),
            });
            if hits.len() >= limit {
                break;
            }
        }
        Ok(hits)
    }
}

impl Store for FsStore {
    fn read(&self, key: &Safekey) -> Result<Option<Metadata>, StoreError> {
        let meta_path = self.metadata_path(key)?;
        if !meta_path.exists() {
            return Ok(None);
        }

        // Per `docs/STORE.md` §4 we MAY take a shared lock for reads. Use the
        // sibling `.lock` file. Lock acquisition errors are surfaced as
        // LockTimeout (5 s budget); locking is best-effort on platforms that
        // implement it as a no-op.
        let lock_path = self.lock_path(key)?;
        let lock_file = open_or_create_lock_file(&lock_path)?;
        acquire_lock(&lock_file, &lock_path, LockMode::Shared)?;

        let raw = std::fs::read_to_string(meta_path.as_std_path())?;
        // Drop the lock by closing the file handle; explicit unlock ensures
        // determinism on platforms where Drop semantics differ. Disambiguate
        // from `std::fs::File::unlock` (stabilized in 1.89) to keep MSRV
        // at 1.86 — the `<File as FileExt>::…` form forces the `fs2` impl.
        let _ = <File as FileExt>::unlock(&lock_file);

        let metadata: Metadata = toml::from_str(&raw)?;
        check_schema_version(&metadata.schema_version)?;
        Ok(Some(metadata))
    }

    fn write(&self, key: &Safekey, m: &Metadata, pdf: Option<&Utf8Path>) -> Result<(), StoreError> {
        let meta_path = self.metadata_path(key)?;
        let lock_path = self.lock_path(key)?;
        let lock_file = open_or_create_lock_file(&lock_path)?;
        acquire_lock(&lock_file, &lock_path, LockMode::Exclusive)?;

        // Re-read existing TOML (if any) so we can apply the §6 merge rule:
        // never overwrite a reserved top-level field previously written by
        // another tool. We DO let the new value win for the [doiget] table
        // (doiget owns it per §6) and for `other` (preserve unknown tables
        // on update; new contents replace prior contents).
        let merged = if meta_path.exists() {
            let raw = std::fs::read_to_string(meta_path.as_std_path())?;
            let existing: Metadata = toml::from_str(&raw)?;
            check_schema_version_for_write(&existing.schema_version)?;
            merge_metadata(existing, m.clone())
        } else {
            m.clone()
        };

        // Serialize → normalize per §7. The normalizer enforces alphabetical
        // key order within tables and a trailing `\n`.
        let normalized = normalize_toml(&merged)?;

        // Issue #122 — crash-consistent ordering: the PDF is written
        // BEFORE the metadata that references it. A crash between the
        // two atomic renames then leaves either the previous
        // consistent entry or no metadata at all — NEVER metadata
        // whose `pdf_path` points at a `.pdf` that does not exist
        // yet. (The reverse order could publish a dangling pointer.)
        // Worst case under the new order is an orphan `<safekey>.pdf`
        // with stale/absent metadata, which list/search ignore (they
        // key off metadata) and a re-fetch overwrites — strictly
        // safer than a torn pointer. There is still no cross-file
        // transaction; this ordering is the bounded MVP guarantee
        // (documented in STORE.md §5).
        if let Some(pdf_src) = pdf {
            let pdf_dst = self.pdf_path(key)?;
            let mut bytes = Vec::new();
            File::open(pdf_src.as_std_path())?.read_to_end(&mut bytes)?;
            // Same atomic dance as the metadata, byte-by-byte.
            atomic_write(&pdf_dst, &bytes)?;
        }

        // Atomic write per §5: tmp → fsync → rename → fsync parent.
        // Done LAST so the metadata only becomes visible once its PDF
        // (if any) is already durably on disk.
        atomic_write(&meta_path, normalized.as_bytes())?;

        let _ = <File as FileExt>::unlock(&lock_file);
        Ok(())
    }

    fn list_recent(&self, limit: usize) -> Result<Vec<EntryInfo>, StoreError> {
        let mut entries = read_all_entries(&self.metadata_dir)?;
        // Most-recent first by [doiget].fetched_at; entries with no
        // `[doiget]` table sort last (None < Some via Reverse).
        entries.sort_by_key(|e| std::cmp::Reverse(e.fetched_at));
        entries.truncate(limit);
        Ok(entries)
    }

    /// Phase 1 search is a linear scan over all metadata files. Phase 2 will
    /// add a tantivy / sqlite-fts index when the corpus grows past the point
    /// where O(N) per query becomes noticeable in CLI latency.
    fn search(&self, query: &str, limit: usize) -> Result<Vec<EntryInfo>, StoreError> {
        let q = query.to_lowercase();
        let mut hits = Vec::new();
        for path in metadata_files(&self.metadata_dir)? {
            let raw = std::fs::read_to_string(path.as_std_path())?;
            let Ok(md) = toml::from_str::<Metadata>(&raw) else {
                // Malformed entries are skipped rather than failing the
                // whole query. A future audit task will surface them.
                continue;
            };
            let haystacks = [
                md.title.to_lowercase(),
                md.authors.join(" ").to_lowercase(),
                md.venue.clone().unwrap_or_default().to_lowercase(),
                md.publisher.clone().unwrap_or_default().to_lowercase(),
            ];
            if haystacks.iter().any(|h| h.contains(&q)) {
                let safekey = safekey_from_metadata_filename(&path);
                hits.push(EntryInfo {
                    safekey,
                    title: md.title,
                    year: md.year,
                    fetched_at: md.doiget.as_ref().map(|d| d.fetched_at),
                });
                if hits.len() >= limit {
                    break;
                }
            }
        }
        Ok(hits)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Reject any safekey containing path-traversal indicators. `Safekey`
/// construction already enforces `[A-Za-z0-9._-]`-only chars per
/// `docs/SAFEKEY.md`; this is defense-in-depth in case a hand-crafted
/// `Safekey` (e.g. an in-crate `Safekey("...".into())` shortcut) is passed
/// in.
fn guard_safekey(s: &str) -> Result<(), StoreError> {
    let bad = s.is_empty()
        || s.contains('/')
        || s.contains('\\')
        || s.contains("..")
        || s.contains('\0')
        || s.starts_with('.')
        || !s
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_');
    if bad {
        Err(StoreError::PathTraversal {
            path: Utf8PathBuf::from(s),
        })
    } else {
        Ok(())
    }
}

/// Recover the safekey from a `<key>.toml` filename. Used only for surfacing
/// list/search results; the safekey we emit here originated as a stored
/// safekey, so it has already passed `guard_safekey` at write time.
fn safekey_from_metadata_filename(p: &Utf8Path) -> Safekey {
    Safekey(p.file_stem().unwrap_or("").to_string())
}

/// Lock mode for [`acquire_lock`].
#[derive(Debug, Clone, Copy)]
enum LockMode {
    /// `flock(LOCK_SH)` — multiple readers OK.
    Shared,
    /// `flock(LOCK_EX)` — exclusive writer.
    Exclusive,
}

/// Open (or create) the advisory lock file. Lock files are never deleted
/// during normal operation per `docs/STORE.md` §4.
fn open_or_create_lock_file(path: &Utf8Path) -> Result<File, StoreError> {
    let f = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path.as_std_path())?;
    Ok(f)
}

/// Acquire `mode` on `lock_file`, polling `try_lock_*` until success or the
/// 5 s budget expires per `docs/STORE.md` §4.
fn acquire_lock(lock_file: &File, lock_path: &Utf8Path, mode: LockMode) -> Result<(), StoreError> {
    let deadline = Instant::now() + LOCK_TIMEOUT;
    loop {
        // Disambiguate from `std::fs::File::try_lock_shared` (stabilized in
        // 1.89), which is an inherent method on `File` and would otherwise
        // shadow the trait method. The `<File as FileExt>::…` form forces
        // the `fs2` impl; we want the cross-platform behavior that returns
        // `std::io::Error` rather than the std `TryLockError` newtype.
        let attempt = match mode {
            LockMode::Shared => <File as FileExt>::try_lock_shared(lock_file),
            LockMode::Exclusive => <File as FileExt>::try_lock_exclusive(lock_file),
        };
        match attempt {
            Ok(()) => return Ok(()),
            Err(e) => {
                let contended = e.raw_os_error() == fs2::lock_contended_error().raw_os_error();
                if !contended {
                    // Not a "would-block" error — surface it directly.
                    return Err(StoreError::Io(e));
                }
                if Instant::now() >= deadline {
                    return Err(StoreError::LockTimeout {
                        path: lock_path.to_owned(),
                    });
                }
                std::thread::sleep(LOCK_POLL_INTERVAL);
            }
        }
    }
}

/// Verify `schema_version` is acceptable for a read. Per `docs/STORE.md` §3,
/// reads succeed with a `tracing::warn!` for ANY future schema_version
/// (minor or major); the read-only mode is enforced at write time
/// (see [`check_schema_version_for_write`]).
fn check_schema_version(theirs: &str) -> Result<(), StoreError> {
    let (their_major, their_minor) = parse_schema_version(theirs)?;
    let (our_major, our_minor) = parse_schema_version(SCHEMA_VERSION)?;
    if their_major > our_major {
        warn!(
            theirs = theirs,
            ours = SCHEMA_VERSION,
            "store entry uses a future-major schema_version; entering read-only mode \
             for this entry (docs/STORE.md §3)"
        );
    } else if their_major == our_major && their_minor > our_minor {
        warn!(
            theirs = theirs,
            ours = SCHEMA_VERSION,
            "store entry uses a newer minor schema_version; reading in compatibility mode \
             (docs/STORE.md §3 future-minor tolerance)"
        );
    }
    Ok(())
}

/// Same as [`check_schema_version`] but used on the EXISTING file before a
/// write merge: any `schema_version` strictly greater than ours (major or
/// minor) refuses the write per `docs/STORE.md` §3 read-only-mode rule.
fn check_schema_version_for_write(theirs: &str) -> Result<(), StoreError> {
    let (their_major, their_minor) = parse_schema_version(theirs)?;
    let (our_major, our_minor) = parse_schema_version(SCHEMA_VERSION)?;
    if their_major > our_major || (their_major == our_major && their_minor > our_minor) {
        return Err(StoreError::SchemaTooNew {
            theirs: theirs.to_string(),
            ours: SCHEMA_VERSION.to_string(),
        });
    }
    Ok(())
}

fn parse_schema_version(s: &str) -> Result<(u32, u32), StoreError> {
    let (maj, min) = s.split_once('.').ok_or(StoreError::MissingField {
        field: "schema_version",
    })?;
    let maj: u32 = maj.parse().map_err(|_| StoreError::MissingField {
        field: "schema_version",
    })?;
    let min: u32 = min.parse().map_err(|_| StoreError::MissingField {
        field: "schema_version",
    })?;
    Ok((maj, min))
}

/// Apply the `docs/STORE.md` §6 merge rule: doiget MUST NOT modify reserved
/// top-level fields written by another tool. Concretely: if `existing` has a
/// reserved field set to a value different from `incoming`, KEEP existing.
/// `[doiget]` is owned by doiget and is overwritten freely. `other` (unknown
/// tables / fields like `[bibliofetch]`) is preserved through union: fields
/// in `existing` not present in `incoming` are kept; otherwise `incoming`
/// wins (callers usually leave `other` empty on a re-fetch, so existing
/// fields survive intact).
fn merge_metadata(existing: Metadata, incoming: Metadata) -> Metadata {
    let mut out = incoming.clone();

    // schema_version: never downgrade. The §6 exception explicitly allows a
    // coordinated minor revision bump, so we take the max of the two.
    if let (Ok((em, en)), Ok((im, in_))) = (
        parse_schema_version(&existing.schema_version),
        parse_schema_version(&incoming.schema_version),
    ) {
        if (em, en) > (im, in_) {
            out.schema_version = existing.schema_version.clone();
        }
    }

    // Reserved fields with non-Option String types: prefer existing if it
    // differs from incoming (and is non-empty).
    if !existing.title.is_empty() && existing.title != incoming.title {
        warn!(
            field = "title",
            existing = existing.title.as_str(),
            "preserving reserved field set by another tool (docs/STORE.md §6)"
        );
        out.title = existing.title;
    }
    if !existing.authors.is_empty() && existing.authors != incoming.authors {
        warn!(
            field = "authors",
            "preserving reserved field set by another tool (docs/STORE.md §6)"
        );
        out.authors = existing.authors;
    }

    // Optional reserved fields: prefer existing Some over incoming Some-different.
    macro_rules! merge_opt {
        ($field:ident) => {
            if existing.$field.is_some() && existing.$field != incoming.$field {
                warn!(
                    field = stringify!($field),
                    "preserving reserved field set by another tool (docs/STORE.md §6)"
                );
                out.$field = existing.$field;
            }
        };
    }
    merge_opt!(year);
    merge_opt!(doi);
    merge_opt!(arxiv_id);
    merge_opt!(abstract_);
    merge_opt!(venue);
    merge_opt!(volume);
    merge_opt!(issue);
    merge_opt!(pages);
    merge_opt!(publisher);
    merge_opt!(issn);
    merge_opt!(isbn);
    merge_opt!(type_);
    merge_opt!(url);
    merge_opt!(pdf_path);

    // keywords (Vec<String>): prefer existing if non-empty and different.
    if !existing.keywords.is_empty() && existing.keywords != incoming.keywords {
        warn!(
            field = "keywords",
            "preserving reserved field set by another tool (docs/STORE.md §6)"
        );
        out.keywords = existing.keywords;
    }

    // arxiv_categories (Vec<String>): same preserve-existing rule as
    // keywords, so a later metadata-only re-write that didn't carry the
    // Atom categories does not drop them (issue #303).
    if !existing.arxiv_categories.is_empty()
        && existing.arxiv_categories != incoming.arxiv_categories
    {
        warn!(
            field = "arxiv_categories",
            "preserving reserved field set by another tool (docs/STORE.md §6)"
        );
        out.arxiv_categories = existing.arxiv_categories;
    }

    // [doiget]: doiget owns this table; incoming wins (already in `out`).
    // If incoming has no [doiget] but existing did, keep the existing one
    // so a metadata-only re-write doesn't silently drop a fetch record.
    if out.doiget.is_none() && existing.doiget.is_some() {
        out.doiget = existing.doiget;
    }

    // `other` (unknown tables / fields): union, prefer EXISTING on key
    // collision (issue #123). STORE.md §6 forbids doiget overwriting a
    // field/table another tool authored; an unknown key already on disk
    // (e.g. a `[bibliofetch]` sub-key) must win over whatever doiget
    // happens to carry in `other`. Doiget normally leaves `other`
    // empty on a re-fetch, so this only changes behaviour in the
    // (latent) case where both sides populate the same unknown key —
    // there, "never overwrite" is the correct §6 resolution.
    let mut merged_other = existing.other;
    for (k, v) in out.other.iter() {
        merged_other.entry(k.clone()).or_insert_with(|| v.clone());
    }
    out.other = merged_other;

    out
}

/// Serialize `m` to TOML and apply `docs/STORE.md` §7 normalization:
/// alphabetical key order within tables, `\n` line endings, trailing
/// newline.
///
/// Implementation: serialize the [`Metadata`] to `toml::Value`, then walk
/// the value tree and re-emit through `BTreeMap` for stable key order.
fn normalize_toml(m: &Metadata) -> Result<String, StoreError> {
    // Serialize to a Value to escape Rust-struct field order; tables are
    // re-keyed alphabetically below.
    let value = toml::Value::try_from(m)?;
    let mut out = String::new();
    write_normalized_toml(&value, &mut out)?;
    if !out.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

/// Walk the top-level table, emit reserved-vs-table keys in normalized
/// order: `schema_version` first, then remaining scalar/array keys
/// alphabetically, then sub-tables alphabetically. Within sub-tables, keys
/// are alphabetical via `BTreeMap`.
fn write_normalized_toml(value: &toml::Value, out: &mut String) -> Result<(), StoreError> {
    let table = match value {
        toml::Value::Table(t) => t,
        _ => {
            return Err(StoreError::Serialize(
                <toml::ser::Error as serde::ser::Error>::custom(
                    "Metadata did not serialize to a TOML table",
                ),
            ));
        }
    };

    // Partition into scalar/array keys (top-level) vs sub-tables (rendered
    // as `[name]` blocks). `schema_version` is forced first per §7.
    let mut top_keys: Vec<&String> = Vec::new();
    let mut sub_table_keys: Vec<&String> = Vec::new();
    for (k, v) in table.iter() {
        if matches!(v, toml::Value::Table(_)) {
            sub_table_keys.push(k);
        } else {
            top_keys.push(k);
        }
    }
    top_keys.sort();
    sub_table_keys.sort();

    // schema_version always first.
    if let Some(v) = table.get("schema_version") {
        write_kv("schema_version", v, out)?;
    }
    for k in top_keys {
        if k == "schema_version" {
            continue;
        }
        if let Some(v) = table.get(k) {
            write_kv(k, v, out)?;
        }
    }
    for k in sub_table_keys {
        if let Some(toml::Value::Table(sub)) = table.get(k) {
            out.push('\n');
            out.push('[');
            out.push_str(k);
            out.push_str("]\n");
            // Within a sub-table, alphabetical order via BTreeMap.
            let sorted: std::collections::BTreeMap<&String, &toml::Value> = sub.iter().collect();
            for (sk, sv) in sorted {
                write_kv(sk, sv, out)?;
            }
        }
    }
    Ok(())
}

/// Render a single `key = value` line. Uses `toml::to_string` on the value
/// half so quoting / escaping matches the spec ("ASCII-safe single-line
/// strings use `\"...\"`", §7).
fn write_kv(key: &str, value: &toml::Value, out: &mut String) -> Result<(), StoreError> {
    out.push_str(key);
    out.push_str(" = ");
    let rendered = toml_value_inline(value)?;
    out.push_str(&rendered);
    out.push('\n');
    Ok(())
}

/// Render a TOML value as a single-line inline expression. Tables are
/// rejected (the caller emits them as `[name]` blocks instead).
fn toml_value_inline(value: &toml::Value) -> Result<String, StoreError> {
    let s = match value {
        toml::Value::Table(_) => {
            return Err(StoreError::Serialize(
                <toml::ser::Error as serde::ser::Error>::custom(
                    "nested tables not supported by inline writer",
                ),
            ));
        }
        // Defer to toml's own serializer for a single value via a one-key
        // shim. `toml::to_string` on a value alone is not supported in
        // toml 1.x, but wrapping it in a singleton table and slicing off
        // the key is reliable.
        v => {
            let mut wrapper = toml::map::Map::new();
            wrapper.insert("__v".to_string(), v.clone());
            let rendered = toml::to_string(&toml::Value::Table(wrapper))?;
            // Output looks like `__v = <value>\n`. Strip the prefix and
            // trailing newline.
            let body = rendered
                .strip_prefix("__v = ")
                .ok_or_else(|| {
                    StoreError::Serialize(<toml::ser::Error as serde::ser::Error>::custom(
                        "unexpected toml singleton format",
                    ))
                })?
                .trim_end_matches('\n')
                .to_string();
            body
        }
    };
    Ok(s)
}

/// Atomic write per `docs/STORE.md` §5: write `tmp` → `sync_all` → `rename`
/// → fsync parent (POSIX). On Windows `std::fs::rename` already issues
/// `MoveFileEx(.., MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)`,
/// so the parent-fsync step is a no-op.
///
/// A crash mid-write leaves either the old file intact (if before the
/// rename) or the new file fully written (if after). It never leaves a
/// partially-visible new file.
fn atomic_write(dst: &Utf8Path, bytes: &[u8]) -> std::io::Result<()> {
    let file_name = dst.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "destination path has no file name",
        )
    })?;
    let mut tmp_path = dst.to_path_buf();
    tmp_path.set_file_name(format!("{}.tmp", file_name));

    {
        let mut f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(tmp_path.as_std_path())?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(tmp_path.as_std_path(), dst.as_std_path())?;

    // Best-effort parent-dir fsync on POSIX. On Windows opening a directory
    // for sync is not supported; the rename above already used
    // MOVEFILE_WRITE_THROUGH semantics.
    #[cfg(unix)]
    {
        if let Some(parent) = dst.parent() {
            if let Ok(dir) = File::open(parent.as_std_path()) {
                let _ = dir.sync_all();
            }
        }
    }

    Ok(())
}

/// Create `path` if missing. On POSIX, set mode `0700`.
fn create_dir_secure(path: &std::path::Path) -> std::io::Result<()> {
    if path.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(0o700);
        std::fs::set_permissions(path, perms)?;
    }
    Ok(())
}

/// List metadata-TOML files (skipping `.tmp` artifacts and `.lock` siblings).
///
/// Non-UTF-8 entry names are skipped silently. Safekey-derived filenames are
/// pure ASCII per `docs/SAFEKEY.md`, so this only filters out unrelated
/// non-UTF-8 garbage that may have been dropped into the store directory by
/// a third party.
fn metadata_files(metadata_dir: &Utf8Path) -> std::io::Result<Vec<Utf8PathBuf>> {
    let mut out = Vec::new();
    if !metadata_dir.exists() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(metadata_dir.as_std_path())? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        let utf8_path = match Utf8PathBuf::from_path_buf(path) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let name = match utf8_path.file_name() {
            Some(n) => n,
            None => continue,
        };
        if name.ends_with(".toml") && !name.ends_with(".tmp") {
            out.push(utf8_path);
        }
    }
    Ok(out)
}

fn read_all_entries(metadata_dir: &Utf8Path) -> Result<Vec<EntryInfo>, StoreError> {
    let mut out = Vec::new();
    for path in metadata_files(metadata_dir)? {
        let raw = std::fs::read_to_string(path.as_std_path())?;
        let Ok(md) = toml::from_str::<Metadata>(&raw) else {
            // Corrupt / future-major entries are skipped from list output.
            continue;
        };
        let safekey = safekey_from_metadata_filename(&path);
        out.push(EntryInfo {
            safekey,
            title: md.title,
            year: md.year,
            fetched_at: md.doiget.map(|d| d.fetched_at),
        });
    }
    Ok(out)
}

// `DoigetExtension` is referenced in the test module below; this tiny shim
// keeps the symbol live in non-test builds so rustdoc intra-doc linking
// stays stable.
#[allow(dead_code)]
fn _doiget_extension_is_visible(d: DoigetExtension) -> DoigetExtension {
    d
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

// `expect`/`unwrap` are idiomatic in tests where panics double as assertions.
// Workspace lints deny them in production code; relax for the test module.
#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::thread;

    use chrono::TimeZone;
    use tempfile::TempDir;

    use crate::{Doi, Safekey, SCHEMA_VERSION};

    fn tmp_dir_utf8(dir: &TempDir) -> Utf8PathBuf {
        Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("temp dir path must be UTF-8")
    }

    fn sample_safekey() -> Safekey {
        // In-crate `pub(crate)` shortcut; matches the pattern used in
        // safekey vector tests in lib.rs.
        Safekey("doi_10.1234_example".to_string())
    }

    fn sample_metadata() -> Metadata {
        Metadata {
            schema_version: SCHEMA_VERSION.to_string(),
            title: "Sample Paper Title".to_string(),
            authors: vec!["Alice Researcher".to_string(), "Bob Coauthor".to_string()],
            year: Some(2026),
            doi: Some(Doi("10.1234/example".to_string())),
            arxiv_id: None,
            arxiv_categories: vec![],
            abstract_: Some("A short abstract.".to_string()),
            venue: Some("Phys. Rev. X".to_string()),
            volume: Some("12".to_string()),
            issue: Some("3".to_string()),
            pages: Some("031001".to_string()),
            publisher: Some("American Physical Society".to_string()),
            issn: Some("2160-3308".to_string()),
            isbn: None,
            type_: Some("journal-article".to_string()),
            keywords: vec!["physics".to_string(), "tdd".to_string()],
            url: Some("https://example.test/paper".to_string()),
            pdf_path: Some("doi_10.1234_example.pdf".to_string()),
            doiget: Some(DoigetExtension {
                fetched_at: chrono::Utc.with_ymd_and_hms(2026, 5, 6, 12, 0, 0).unwrap(),
                source: "unpaywall".to_string(),
                license: "CC-BY-4.0".to_string(),
                oa_status: Some("gold".to_string()),
                size_bytes: 1234567,
                mcp_call_id: Some("01JCKZ7Q0000000000000000AB".to_string()),
                tags: Vec::new(),
                collections: Vec::new(),
                annotation: None,
            }),
            other: BTreeMap::new(),
        }
    }

    fn fresh_store(dir: &TempDir) -> FsStore {
        let root = tmp_dir_utf8(dir).join("papers");
        FsStore::new(root).expect("FsStore::new")
    }

    #[test]
    fn merge_metadata_preserves_existing_arxiv_categories() {
        // Issue #303 / review #318: a later metadata-only re-write that did
        // NOT carry the Atom categories must not drop the stored ones.
        let mut existing = sample_metadata();
        existing.arxiv_categories = vec!["cond-mat.str-el".to_string()];
        let mut incoming = sample_metadata();
        incoming.arxiv_categories = vec![]; // re-write without categories
        let merged = merge_metadata(existing, incoming);
        assert_eq!(merged.arxiv_categories, vec!["cond-mat.str-el".to_string()]);
    }

    #[test]
    fn roundtrip_reserved_fields() {
        let dir = TempDir::new().expect("tmp");
        let store = fresh_store(&dir);
        let key = sample_safekey();
        let m = sample_metadata();
        store.write(&key, &m, None).expect("write");

        let read = store.read(&key).expect("read").expect("Some");
        assert_eq!(read.schema_version, m.schema_version);
        assert_eq!(read.title, m.title);
        assert_eq!(read.authors, m.authors);
        assert_eq!(read.year, m.year);
        assert_eq!(
            read.doi.as_ref().map(|d| d.as_str()),
            Some("10.1234/example")
        );
        assert_eq!(read.abstract_, m.abstract_);
        assert_eq!(read.venue, m.venue);
        assert_eq!(read.publisher, m.publisher);
        assert_eq!(read.issn, m.issn);
        assert_eq!(read.type_, m.type_);
        assert_eq!(read.keywords, m.keywords);
        assert_eq!(read.url, m.url);
        assert_eq!(read.pdf_path, m.pdf_path);
    }

    #[test]
    fn roundtrip_doiget_extension() {
        let dir = TempDir::new().expect("tmp");
        let store = fresh_store(&dir);
        let key = sample_safekey();
        let m = sample_metadata();
        store.write(&key, &m, None).expect("write");

        let read = store.read(&key).expect("read").expect("Some");
        let d = read.doiget.expect("doiget table present");
        let want = m.doiget.expect("input doiget");
        assert_eq!(d.fetched_at, want.fetched_at);
        assert_eq!(d.source, want.source);
        assert_eq!(d.license, want.license);
        assert_eq!(d.size_bytes, want.size_bytes);
        assert_eq!(d.mcp_call_id, want.mcp_call_id);
        // OA transparency (#281 item 4): oa_status must survive the
        // write→read round-trip, not just sit in the fixture.
        assert_eq!(d.oa_status, want.oa_status);
    }

    #[test]
    fn read_returns_none_for_missing_safekey() {
        let dir = TempDir::new().expect("tmp");
        let store = fresh_store(&dir);
        let key = Safekey("nonexistent".to_string());
        let res = store.read(&key).expect("read ok");
        assert!(res.is_none(), "expected Ok(None), got {:?}", res);
    }

    #[test]
    fn schema_too_new_blocks_writes_but_allows_reads() {
        let dir = TempDir::new().expect("tmp");
        let store = fresh_store(&dir);
        let key = sample_safekey();

        // Hand-craft a TOML with a future-major schema_version.
        let meta_path = store.metadata_path(&key).expect("path");
        std::fs::create_dir_all(meta_path.parent().expect("parent").as_std_path()).expect("mkdir");
        let body = "schema_version = \"2.0\"\ntitle = \"Future\"\nauthors = []\n";
        std::fs::write(meta_path.as_std_path(), body).expect("write");

        // Read should succeed (read-only mode per §3, warn is best-effort).
        let read = store.read(&key).expect("read ok");
        assert!(read.is_some(), "future-major file must be readable");

        // Write should refuse with SchemaTooNew.
        let m = sample_metadata();
        let err = store.write(&key, &m, None).expect_err("write must fail");
        match err {
            StoreError::SchemaTooNew { theirs, ours } => {
                assert_eq!(theirs, "2.0");
                assert_eq!(ours, SCHEMA_VERSION);
            }
            other => panic!("expected SchemaTooNew, got {:?}", other),
        }
    }

    #[test]
    fn concurrent_writers_serialize_via_flock() {
        // Two threads writing to the same key with different [doiget].source
        // values. The flock SHOULD make every write atomic from the on-disk
        // perspective: at no point is the metadata file half-written, and
        // every parse succeeds. We do not assert WHICH writer wins — only
        // that the file remains valid TOML throughout.
        let dir = TempDir::new().expect("tmp");
        let store = Arc::new(fresh_store(&dir));
        let key = sample_safekey();

        // Pre-create so both threads exercise the merge path.
        store.write(&key, &sample_metadata(), None).expect("seed");

        let mut handles = Vec::new();
        for source in ["unpaywall", "europepmc"] {
            let store = Arc::clone(&store);
            let key = key.clone();
            handles.push(thread::spawn(move || {
                let mut m = sample_metadata();
                if let Some(d) = m.doiget.as_mut() {
                    d.source = source.to_string();
                }
                store.write(&key, &m, None).expect("write");
            }));
        }
        for h in handles {
            h.join().expect("join");
        }

        // Every read after the two writers complete must succeed and produce
        // a value whose `[doiget].source` is one of the two contenders.
        let read = store.read(&key).expect("read").expect("Some");
        let source = read.doiget.expect("doiget").source;
        assert!(
            source == "unpaywall" || source == "europepmc",
            "winning source must be one of the contenders, got {}",
            source
        );
    }

    #[test]
    fn list_recent_orders_by_fetched_at_desc() {
        let dir = TempDir::new().expect("tmp");
        let store = fresh_store(&dir);

        for (idx, year_seed) in [(1, 2024_u32), (2, 2025), (3, 2026)] {
            let key = Safekey(format!("doi_10.1234_entry{}", idx));
            let mut m = sample_metadata();
            m.title = format!("Entry {}", idx);
            if let Some(d) = m.doiget.as_mut() {
                d.fetched_at = chrono::Utc
                    .with_ymd_and_hms(year_seed as i32, 5, 6, 12, 0, 0)
                    .unwrap();
            }
            store.write(&key, &m, None).expect("write");
        }

        let recent = store.list_recent(10).expect("list");
        assert_eq!(recent.len(), 3, "expected 3 entries, got {}", recent.len());
        // Most-recent first: 2026, 2025, 2024.
        assert_eq!(recent[0].title, "Entry 3");
        assert_eq!(recent[1].title, "Entry 2");
        assert_eq!(recent[2].title, "Entry 1");
        for w in recent.windows(2) {
            assert!(
                w[0].fetched_at >= w[1].fetched_at,
                "recent[].fetched_at must be non-increasing"
            );
        }
    }

    #[test]
    fn search_finds_by_title_substring() {
        let dir = TempDir::new().expect("tmp");
        let store = fresh_store(&dir);

        let key = Safekey("doi_10.1234_quantum".to_string());
        let mut m = sample_metadata();
        m.title = "Quantum Stuff and Other Topics".to_string();
        store.write(&key, &m, None).expect("write");

        let hits = store.search("quantum", 10).expect("search");
        assert_eq!(hits.len(), 1, "expected 1 hit, got {}", hits.len());
        assert_eq!(hits[0].title, "Quantum Stuff and Other Topics");

        let empty = store.search("relativity", 10).expect("search");
        assert!(empty.is_empty(), "expected no hits, got {:?}", empty);
    }

    #[test]
    fn path_traversal_in_safekey_blocked() {
        let dir = TempDir::new().expect("tmp");
        let store = fresh_store(&dir);
        let bad = Safekey("../etc/passwd".to_string());

        match store.read(&bad) {
            Err(StoreError::PathTraversal { .. }) => {}
            other => panic!("expected PathTraversal, got {:?}", other),
        }
        let m = sample_metadata();
        match store.write(&bad, &m, None) {
            Err(StoreError::PathTraversal { .. }) => {}
            other => panic!("expected PathTraversal, got {:?}", other),
        }
    }

    #[test]
    fn write_then_read_normalized_toml_alphabetizes_keys() {
        // §7 normalization: schema_version first, then reserved fields
        // alphabetically, then sub-tables alphabetically with alphabetical
        // keys inside.
        let dir = TempDir::new().expect("tmp");
        let store = fresh_store(&dir);
        let key = sample_safekey();
        store.write(&key, &sample_metadata(), None).expect("write");

        let path = store.metadata_path(&key).expect("path");
        let raw = std::fs::read_to_string(path.as_std_path()).expect("read");
        // schema_version must be the first line.
        let first_line = raw.lines().next().expect("at least one line");
        assert!(
            first_line.starts_with("schema_version = "),
            "first line must be schema_version, got: {:?}",
            first_line
        );
        // EOF newline.
        assert!(raw.ends_with('\n'), "file must end with a newline");
        // No CR characters anywhere.
        assert!(!raw.contains('\r'), "no CR allowed; LF only");
        // Sub-table appears.
        assert!(raw.contains("\n[doiget]\n"), "doiget sub-table missing");
        // Within [doiget], `fetched_at` must precede `license` alphabetically.
        let doiget_idx = raw.find("[doiget]").expect("doiget block");
        let after = &raw[doiget_idx..];
        let fetched_at_idx = after
            .find("fetched_at = ")
            .expect("fetched_at key in doiget");
        let license_idx = after.find("license = ").expect("license key in doiget");
        assert!(
            fetched_at_idx < license_idx,
            "fetched_at must precede license within [doiget]"
        );
    }

    #[test]
    fn write_preserves_unknown_table_from_existing_file() {
        // §6 + §8: if the existing file has a `[bibliofetch]` table, a
        // doiget rewrite must not silently drop it.
        let dir = TempDir::new().expect("tmp");
        let store = fresh_store(&dir);
        let key = sample_safekey();
        let meta_path = store.metadata_path(&key).expect("path");

        let body = format!(
            "schema_version = \"{}\"\ntitle = \"Existing\"\nauthors = [\"Carol\"]\n\n\
             [bibliofetch]\nharvest = \"2026-01-01\"\n",
            SCHEMA_VERSION
        );
        std::fs::write(meta_path.as_std_path(), body).expect("write");

        let mut m = sample_metadata();
        m.title = "Doiget Wins?".to_string(); // would normally overwrite, but §6 keeps existing
        store.write(&key, &m, None).expect("write");

        let read_raw = std::fs::read_to_string(meta_path.as_std_path()).expect("re-read");
        assert!(
            read_raw.contains("bibliofetch"),
            "[bibliofetch] table was dropped: {}",
            read_raw
        );
        assert!(
            read_raw.contains("title = \"Existing\""),
            "doiget overwrote a reserved field set by another tool: {}",
            read_raw
        );
    }

    /// Issue #121: prove the BiblioFetch.jl coexistence contract
    /// end-to-end through the actual `read()` / `write()` API with
    /// TYPED values — not a raw-text substring check. Seeds a
    /// "BiblioFetch-authored" entry with reserved fields, a
    /// `[bibliofetch]` table carrying typed sub-keys (string / int /
    /// array) AND an unknown top-level scalar, then asserts a
    /// doiget read→mutate→write→read cycle preserves all of it and
    /// does not clobber the reserved field (STORE.md §6 + §8).
    #[test]
    fn bibliofetch_typed_table_and_unknown_scalar_survive_roundtrip() {
        let dir = TempDir::new().expect("tmp");
        let store = fresh_store(&dir);
        let key = sample_safekey();
        let meta_path = store.metadata_path(&key).expect("path");

        // Written "by BiblioFetch.jl": reserved fields + a typed
        // [bibliofetch] table + an unknown top-level scalar.
        let body = format!(
            "schema_version = \"{}\"\n\
             title = \"Existing\"\n\
             authors = [\"Carol\"]\n\
             zotero_key = \"ABC123\"\n\n\
             [bibliofetch]\n\
             harvest = \"2026-02-03\"\n\
             count = 42\n\
             tags = [\"x\", \"y\"]\n",
            SCHEMA_VERSION
        );
        std::fs::write(meta_path.as_std_path(), body).expect("seed write");

        // First read through the real API must surface the unknowns
        // in `other`.
        let m0 = store.read(&key).expect("read ok").expect("entry present");
        assert!(
            m0.other.contains_key("bibliofetch"),
            "[bibliofetch] not captured into `other` on read: {:?}",
            m0.other
        );
        assert_eq!(
            m0.other.get("zotero_key").and_then(|v| v.as_str()),
            Some("ABC123"),
            "unknown top-level scalar not captured: {:?}",
            m0.other
        );

        // doiget rewrites (e.g. a re-fetch) with its own metadata.
        let mut m_doiget = sample_metadata();
        m_doiget.title = "Doiget Would Overwrite".to_string();
        store.write(&key, &m_doiget, None).expect("doiget write");

        // Read again — everything BiblioFetch authored must still be
        // there, byte/value-identical, and the reserved field intact.
        let m1 = store
            .read(&key)
            .expect("re-read ok")
            .expect("entry present");
        assert_eq!(
            m1.title, "Existing",
            "STORE.md §6: doiget overwrote a reserved field"
        );
        let bf = m1
            .other
            .get("bibliofetch")
            .and_then(|v| v.as_table())
            .expect("[bibliofetch] table survived read->write->read");
        assert_eq!(
            bf.get("harvest").and_then(|v| v.as_str()),
            Some("2026-02-03")
        );
        assert_eq!(bf.get("count").and_then(|v| v.as_integer()), Some(42));
        let tags = bf
            .get("tags")
            .and_then(|v| v.as_array())
            .expect("tags array survived");
        let tags: Vec<&str> = tags.iter().filter_map(|v| v.as_str()).collect();
        assert_eq!(tags, vec!["x", "y"]);
        assert_eq!(
            m1.other.get("zotero_key").and_then(|v| v.as_str()),
            Some("ABC123"),
            "unknown top-level scalar lost across the cycle"
        );

        // STORE.md §7 normalization: trailing newline preserved.
        let raw = std::fs::read_to_string(meta_path.as_std_path()).expect("raw re-read");
        assert!(raw.ends_with('\n'), "missing trailing newline: {raw:?}");
    }

    /// Issue #123: on an `other`-key collision the EXISTING on-disk
    /// value must win (STORE.md §6 "never overwrite"). Seeds a
    /// `zotero_key` "by another tool", then has doiget write an entry
    /// whose own `other` carries a different `zotero_key`; the disk
    /// value must survive.
    #[test]
    fn other_key_collision_prefers_existing() {
        let dir = TempDir::new().expect("tmp");
        let store = fresh_store(&dir);
        let key = sample_safekey();
        let meta_path = store.metadata_path(&key).expect("path");

        let body = format!(
            "schema_version = \"{}\"\ntitle = \"Existing\"\nauthors = [\"Carol\"]\n\
             zotero_key = \"FROM_BIBLIOFETCH\"\n",
            SCHEMA_VERSION
        );
        std::fs::write(meta_path.as_std_path(), body).expect("seed");

        let mut m = sample_metadata();
        m.other.insert(
            "zotero_key".to_string(),
            toml::Value::String("FROM_DOIGET".to_string()),
        );
        store.write(&key, &m, None).expect("write");

        let got = store.read(&key).expect("read").expect("present");
        assert_eq!(
            got.other.get("zotero_key").and_then(|v| v.as_str()),
            Some("FROM_BIBLIOFETCH"),
            "STORE.md §6: existing `other` value must win on collision"
        );
    }

    #[test]
    fn pdf_is_copied_atomically_on_write() {
        let dir = TempDir::new().expect("tmp");
        let store = fresh_store(&dir);
        let key = sample_safekey();

        // Write a small synthetic "PDF" file.
        let src_dir = TempDir::new().expect("tmp src");
        let src_path = Utf8PathBuf::from_path_buf(src_dir.path().to_path_buf())
            .expect("utf8 src dir")
            .join("input.pdf");
        std::fs::write(src_path.as_std_path(), b"%PDF-1.7 synthetic").expect("write src");

        store
            .write(&key, &sample_metadata(), Some(&src_path))
            .expect("write");

        let dst = store.pdf_path(&key).expect("pdf path");
        let bytes = std::fs::read(dst.as_std_path()).expect("read dst");
        assert_eq!(bytes, b"%PDF-1.7 synthetic");
    }
}
