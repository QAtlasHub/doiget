//! Filesystem-backed metadata store.
//!
//! Binding spec: [`docs/STORE.md`](../../../../docs/STORE.md) (NORMATIVE shared
//! spec for layout, schema, lock protocol, atomic write, normalization).
//! Public API surface: `docs/PUBLIC_API.md` §2 (Store trait), §3 (Metadata).
//!
//! ## Entry points
//!
//! - [`Store`] — the trait surface implementations expose.
//! - [`FsStore`] — filesystem-backed implementation rooted at a configurable
//!   directory (default `./papers`, under the cwd; ADR-0036).
//! - [`Metadata`] / [`DoigetExtension`] — the on-disk schema, mirrored from
//!   `docs/STORE.md` §2.
//!
//! ## Other writers
//!
//! A store may also be written by another tool -- BiblioFetch.jl stores stay
//! readable, though the shared contract is retired (ADR-0060). Writers follow
//! the lock protocol in `docs/STORE.md` §4 and the atomic-write sequence in §5. Per §6, doiget MUST NOT overwrite reserved
//! top-level fields previously written by another tool — see [`FsStore::write`].

pub mod citekey;
mod fs_store;
pub mod metadata;
pub mod render;

pub use fs_store::FsStore;

/// Crash-consistent write (tmp + fsync + rename), shared with the resolver
/// cache so both write the same way. See `docs/STORE.md` §5.
pub(crate) use fs_store::atomic_write;
pub use metadata::{DoigetExtension, Metadata, ORIGIN_USER_SUPPLIED};
pub use render::{to_bibtex, to_csl_array};

/// Run a synchronous [`Store`] call from async code without stalling the
/// runtime (#590).
///
/// Every `Store` method does blocking filesystem I/O: `write` can poll the
/// advisory lock for up to 5 s (`LOCK_TIMEOUT`, `std::thread::sleep` in
/// 50 ms steps) and then `fsync`s, which on a Dropbox / OneDrive / SMB store
/// root costs hundreds of milliseconds uncontended. Called directly from an
/// `async fn`, that holds a tokio worker for the whole duration, delaying
/// every other task on it -- including the rate limiter's timers and other
/// in-flight MCP tool calls.
///
/// On a multi-thread runtime (`#[tokio::main]`, `doiget serve`) this runs
/// `f` under [`tokio::task::block_in_place`], which hands the worker's other
/// tasks to another thread first. Elsewhere -- no runtime, or a
/// current-thread runtime, where `block_in_place` would panic -- `f` runs
/// inline, as before.
///
/// `block_in_place` rather than `spawn_blocking` because the orchestrator
/// holds the store as `&dyn Store` (`docs/PUBLIC_API.md` §2): moving it into
/// a `'static` closure would change the public signature of `fetch_paper`,
/// which is the cost the issue ruled out for making the trait async.
pub fn blocking_section<T>(f: impl FnOnce() -> T) -> T {
    match tokio::runtime::Handle::try_current().map(|h| h.runtime_flavor()) {
        Ok(tokio::runtime::RuntimeFlavor::MultiThread) => tokio::task::block_in_place(f),
        _ => f(),
    }
}

use camino::Utf8Path;
use serde::Serialize;
use thiserror::Error;

use crate::Safekey;

/// Brief summary of a stored entry; returned by
/// [`Store::list_recent`] / [`Store::search`].
///
/// `non_exhaustive` so adding new summary fields (e.g. `doi`, `authors`) in a
/// later revision is non-breaking. Pattern-match with a wildcard arm.
///
/// `Serialize` enables `list-recent --mode json` / `search --mode json`
/// (#204) — the wire form is the obvious field-name JSON: `{"safekey":
/// "...", "title": "...", "year": 2024, "fetched_at": "2026-05-20T…Z"}`,
/// with `null` for absent optionals.
///
/// # Wire-format stability (post-#208 self-review §1)
///
/// Once a release ships with the \[`Serialize`\] derive, the field
/// **names** below become part of the public API: a downstream consumer
/// (CLI agent, MCP tool, BiblioFetch.jl, third-party script) MAY bind
/// to them. Renaming a field is then a semver minor bump and warrants
/// a CHANGELOG \[BREAKING\] note. Adding new fields is still safe
/// (per `#[non_exhaustive]`).
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct EntryInfo {
    /// The safekey of the entry. See `docs/SAFEKEY.md`.
    pub safekey: Safekey,
    /// Title from the entry's reserved `title` field.
    pub title: String,
    /// Year, if any, from the entry's reserved `year` field.
    pub year: Option<i32>,
    /// `fetched_at` from the `[doiget]` table, if any.
    pub fetched_at: Option<chrono::DateTime<chrono::Utc>>,
    /// `size_bytes` from the `[doiget]` table: the size of the stored PDF,
    /// `0` for a metadata-only entry, `None` when the entry has no
    /// `[doiget]` table at all.
    ///
    /// #481: without it the inventory commands could not tell a fetched
    /// paper from a metadata-only stub. Every other surface could -- the
    /// fetch itself failed loudly, the TOML omits `pdf_path`, the
    /// provenance log carries an `err` row, `doiget info` shows
    /// `size_bytes = 0` -- and the one command that answers "what do I
    /// have?" without knowing the ref in advance was the one that dropped
    /// it. Fifty refs with ten blocked listed as fifty identical rows.
    pub size_bytes: Option<u64>,
}

impl EntryInfo {
    /// Whether a PDF was actually stored for this entry.
    ///
    /// `false` for a metadata-only entry (`size_bytes == 0`) and for one
    /// with no `[doiget]` table. Deliberately not "is this entry useful" --
    /// a metadata-only entry is a legitimate result, it is just a different
    /// one, and #118 is the standing rule that the two must not be
    /// presented alike.
    #[must_use]
    pub fn has_pdf(&self) -> bool {
        self.size_bytes.is_some_and(|n| n > 0)
    }
}

/// Errors emitted by [`Store`] implementations.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StoreError {
    /// Underlying I/O failure.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// Malformed TOML or schema mismatch on read.
    #[error("toml deserialize error: {0}")]
    Deserialize(#[from] toml::de::Error),
    /// Failed to serialize a [`Metadata`] to TOML.
    #[error("toml serialize error: {0}")]
    Serialize(#[from] toml::ser::Error),
    /// Could not acquire the advisory `flock` within the 5 s budget named in
    /// `docs/STORE.md` §4.
    #[error("flock timeout (5s) on {path}")]
    LockTimeout {
        /// The lock-file path that was contended.
        path: camino::Utf8PathBuf,
    },
    /// The on-disk `schema_version` is a future major; per `docs/STORE.md` §3
    /// the entry is read-only for this build.
    #[error("schema_version too new: {theirs} > {ours}; entry is read-only")]
    SchemaTooNew {
        /// Schema version observed on disk.
        theirs: String,
        /// Schema version this build supports.
        ours: String,
    },
    /// A reserved field that the spec marks as required is missing.
    #[error("required field missing: {field}")]
    MissingField {
        /// The name of the missing reserved field.
        field: &'static str,
    },
    /// The supplied [`Safekey`] resolves to a path outside the store root.
    /// Defense-in-depth check; `Safekey` construction already enforces the
    /// `[A-Za-z0-9._-]`-only charset per `docs/SAFEKEY.md`.
    #[error("path is outside the store root: {path}")]
    PathTraversal {
        /// The offending resolved path.
        path: camino::Utf8PathBuf,
    },
}

/// Who is authoritative for the user-authored `[doiget]` fields
/// (`tags`, `collections`, `annotation`) on a write.
///
/// A fetch never authors them: every `DoigetExtension` the orchestrator
/// builds hard-codes `Vec::new()` / `None`. Letting that win silently
/// discarded a user's tags on any re-fetch, which is the loss ADR-0056
/// closed for `oa_status` / `license` and left open here.
///
/// The distinction has to be explicit rather than "is the incoming value
/// empty", because `doiget tag --remove` and `doiget annotate --clear`
/// legitimately mean the empty value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum UserFields {
    /// The caller did not author them; keep whatever is on disk. Fetches.
    Preserve,
    /// The caller means exactly what it wrote, empty included. `doiget tag`,
    /// `doiget annotate`, and their MCP equivalents.
    Authored,
}

/// Filesystem-shaped metadata store, semver-locked per `docs/PUBLIC_API.md`
/// §2.
///
/// Implementations are responsible for honoring:
///
/// - `docs/STORE.md` §4 lock protocol (advisory `flock` on
///   `<safekey>.toml.lock` with a 5 s timeout).
/// - `docs/STORE.md` §5 atomic-write sequence (`tmp` → fsync → rename →
///   fsync parent).
/// - `docs/STORE.md` §6 doiget write discipline: never overwrite reserved
///   top-level fields previously written by another tool.
/// - `docs/STORE.md` §7 TOML normalization (alphabetical key order, `\n`
///   line endings, trailing newline).
pub trait Store: Send + Sync {
    /// Read the entry keyed by `key`.
    ///
    /// Returns `Ok(None)` if no entry exists. Returns `Err` on I/O failure,
    /// malformed TOML, or unrecoverable schema mismatch (e.g. future major).
    fn read(&self, key: &Safekey) -> Result<Option<Metadata>, StoreError>;

    /// Write or update the entry keyed by `key`.
    ///
    /// If `pdf` is `Some`, the file at that path is copied to
    /// `<root>/<safekey>.pdf` via the same atomic-rename dance as the
    /// metadata file. The caller is responsible for emitting the
    /// `event=store_write` provenance row (see `docs/PROVENANCE_LOG.md` §3).
    /// Write a fetch result. User-authored `[doiget]` fields already on disk
    /// are preserved ([`UserFields::Preserve`]) -- a fetch does not author
    /// them, and silently dropping them is data loss.
    fn write(&self, key: &Safekey, m: &Metadata, pdf: Option<&Utf8Path>) -> Result<(), StoreError>;

    /// Write on behalf of a caller that DID author the user fields, so an
    /// empty `tags` / `collections` or a `None` annotation means exactly that
    /// ([`UserFields::Authored`]). `doiget tag` / `doiget annotate` only.
    fn write_user_authored(
        &self,
        key: &Safekey,
        m: &Metadata,
        pdf: Option<&Utf8Path>,
    ) -> Result<(), StoreError>;

    /// Return up to `limit` entries, most-recent first by `[doiget].fetched_at`.
    fn list_recent(&self, limit: usize) -> Result<Vec<EntryInfo>, StoreError>;

    /// Return up to `limit` entries whose title / authors / venue / publisher
    /// case-insensitively contain `query`.
    fn search(&self, query: &str, limit: usize) -> Result<Vec<EntryInfo>, StoreError>;
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    /// Ticks a spawned task managed during `hold` of blocking work, run on
    /// the ONLY worker of a one-worker multi-thread runtime.
    async fn ticks_during(hold: Duration, through_helper: bool) -> usize {
        let ticks = Arc::new(AtomicUsize::new(0));
        let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let (t, r) = (Arc::clone(&ticks), Arc::clone(&running));
        let ticker = tokio::spawn(async move {
            while r.load(Ordering::SeqCst) {
                t.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        });
        // Let the ticker start, then measure only the blocked window.
        tokio::time::sleep(Duration::from_millis(20)).await;
        let before = ticks.load(Ordering::SeqCst);
        let blocker = tokio::spawn(async move {
            if through_helper {
                super::blocking_section(|| std::thread::sleep(hold));
            } else {
                std::thread::sleep(hold);
            }
        });
        blocker.await.expect("blocker");
        let during = ticks.load(Ordering::SeqCst) - before;
        running.store(false, Ordering::SeqCst);
        ticker.await.expect("ticker");
        during
    }

    /// #590: a store call that blocks (lock poll, fsync on a synced
    /// folder) must not stall the other tasks on its worker.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn a_blocking_store_call_leaves_the_runtime_responsive() {
        let hold = Duration::from_millis(500);
        // Control: the same wait called directly starves the ticker, so the
        // assertion below measures the helper and not the scheduler's luck.
        let direct = ticks_during(hold, false).await;
        let wrapped = ticks_during(hold, true).await;
        assert!(
            direct <= 3,
            "control: direct blocking let {direct} ticks through"
        );
        // Loose on purpose: Windows' default timer granularity (~15.6 ms)
        // caps a 5 ms ticker near 32 ticks in 500 ms, before CI load. The
        // control above is what makes the bound meaningful.
        assert!(
            wrapped >= 8,
            "blocking_section let only {wrapped} ticks through"
        );
    }

    #[test]
    fn blocking_section_runs_inline_without_a_multi_thread_runtime() {
        assert_eq!(super::blocking_section(|| 7), 7);
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        assert_eq!(rt.block_on(async { super::blocking_section(|| 8) }), 8);
    }

    /// The call sites cannot express the convention in the type system
    /// (the trait is sync on purpose), so this pins it in the source: no
    /// `Store` method is called from the orchestrator except through
    /// `blocking_section`.
    #[test]
    fn every_orchestrator_store_call_goes_through_blocking_section() {
        let src = include_str!("../orchestrator.rs");
        assert_eq!(unwrapped_store_calls(src), Vec::<String>::new());
        assert!(src.contains("blocking_section(|| store.write("));
    }

    /// Every `store.<method>(` in the non-test part of `src` that is not the
    /// body of a `blocking_section` closure. Whitespace is stripped first, so
    /// neither rustfmt re-wrapping a call nor a method chain split across
    /// lines (`store\n    .read(`) hides one. Calls on a receiver not named
    /// `store` are out of its reach; the call sites keep that name.
    pub(crate) fn unwrapped_store_calls(src: &str) -> Vec<String> {
        let body = src.split("\nmod tests {").next().unwrap_or(src);
        let flat: String = body.chars().filter(|c| !c.is_whitespace()).collect();
        let mut out = Vec::new();
        for m in STORE_METHODS {
            let calls = flat.match_indices(&*format!("store.{m}(")).count();
            let wrapped = flat
                .match_indices(&*format!("blocking_section(||store.{m}("))
                .count()
                + flat
                    .match_indices(&*format!("blocking_section(||{{store.{m}("))
                    .count();
            if calls != wrapped {
                out.push(format!("store.{m}: {calls} calls, {wrapped} wrapped"));
            }
        }
        out
    }

    /// The `Store` trait's methods and `FsStore`'s inherent search.
    pub(crate) const STORE_METHODS: &[&str] = &[
        "read",
        "write",
        "write_user_authored",
        "list_recent",
        "search",
        "search_by_tag",
    ];
}
