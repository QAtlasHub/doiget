//! `doiget search <query>` subcommand — case-insensitive substring search
//! over stored metadata.
//!
//! Phase 1 is a linear scan over `<store-root>/.metadata/*.toml` (per
//! [`FsStore::search`](doiget_core::store::FsStore) — see
//! `crates/doiget-core/src/store/fs_store.rs`). Phase 2+ swaps in a
//! tantivy / sqlite-fts index when the corpus grows past the point where
//! O(N) per query becomes noticeable in CLI latency.
//!
//! Output is a tab-separated table with a header line, mirroring
//! [`list_recent`](super::list_recent) so the two subcommands stay
//! `cut(1)`-compatible. Future fields will be APPENDED, not inserted.

use std::io::Write;

use anyhow::{Context, Result};

use doiget_core::store::{FsStore, Store};

use super::resolve_store_root;

/// Phase 1 default cap on the number of returned rows. Picked to match the
/// "small CLI table" feel — large enough to be useful for an ad-hoc
/// `doiget search foo`, small enough that an unbounded scan over a
/// pathological store still terminates promptly. A `--limit` flag lands
/// alongside the Phase 2 index work.
const DEFAULT_LIMIT: usize = 50;

/// Format string for [`chrono::DateTime`] columns. RFC3339-shaped, UTC, no
/// fractional seconds — identical to the [`list_recent`](super::list_recent)
/// table so downstream pipelines can treat both outputs uniformly.
const FETCHED_AT_FMT: &str = "%Y-%m-%dT%H:%M:%SZ";

/// Run the `search` subcommand against the configured store.
///
/// Empty / whitespace-only queries are rejected with a non-zero exit and an
/// error on stderr — the on-disk `Store::search` would otherwise return
/// every entry up to `DEFAULT_LIMIT`, which is almost never what the caller
/// intended.
pub fn run(query: String, mode: super::output::OutputMode) -> Result<()> {
    // `mode` honors ADR-0017: `Quiet` suppresses the TSV table; the
    // empty-query bail!() and store error paths still raise (#203). Json
    // body is tracked in #204.
    if query.trim().is_empty() {
        anyhow::bail!("search query is empty");
    }

    let store_root = resolve_store_root()?;
    let store = FsStore::new(store_root)?;
    let entries = store
        .search(&query, DEFAULT_LIMIT)
        .with_context(|| format!("search failed for query {query:?}"))?;

    if mode == super::output::OutputMode::Quiet {
        return Ok(());
    }

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if mode == super::output::OutputMode::Json {
        // #204: `EntryInfo` carries `Serialize`; emit a JSON array.
        let s = serde_json::to_string_pretty(&entries)
            .context("failed to serialize search entries to JSON")?;
        writeln!(out, "{s}").context("failed to write search JSON to stdout")?;
        return Ok(());
    }
    writeln!(out, "safekey\tyear\ttitle\tfetched_at")
        .context("failed to write search header to stdout")?;
    for e in entries {
        let year = e.year.map(|y| y.to_string()).unwrap_or_else(|| "-".into());
        let fetched = e
            .fetched_at
            .map(|t| t.format(FETCHED_AT_FMT).to_string())
            .unwrap_or_else(|| "-".into());
        writeln!(
            out,
            "{}\t{}\t{}\t{}",
            e.safekey.as_str(),
            year,
            e.title,
            fetched
        )
        .context("failed to write search row to stdout")?;
    }
    Ok(())
}
