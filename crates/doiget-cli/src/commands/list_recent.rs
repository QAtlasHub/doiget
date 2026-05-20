//! `doiget list-recent [--limit=N]` subcommand — read-only most-recent
//! listing.
//!
//! Reads the configured store's `.metadata/` directory via
//! [`Store::list_recent`] and prints one row per entry on stdout, ordered
//! most-recent first by `[doiget].fetched_at`. Network access is never
//! required.
//!
//! Output is a tab-separated table with a header line. The four columns
//! match [`EntryInfo`](doiget_core::store::EntryInfo): `safekey`, `year`,
//! `title`, `fetched_at`. Missing `year` / `fetched_at` render as `-`.

use std::io::Write;

use anyhow::{Context, Result};

use doiget_core::store::{FsStore, Store};

use super::resolve_store_root;

/// Format string for [`chrono::DateTime`] columns. RFC3339-shaped, UTC, no
/// fractional seconds — matches the on-disk wire format from
/// [`docs/STORE.md`](../../../../docs/STORE.md) §2.
const FETCHED_AT_FMT: &str = "%Y-%m-%dT%H:%M:%SZ";

/// Run the `list-recent` subcommand against the configured store.
///
/// Emits a tab-separated table on stdout. The column order is intentionally
/// stable for `cut(1)` consumption; future fields will be APPENDED, not
/// inserted.
pub fn run(limit: usize, mode: super::output::OutputMode) -> Result<()> {
    // `mode` honors ADR-0017: `Quiet` suppresses the TSV table but the
    // store read still runs (so I/O failures surface as exit 1) (#203).
    // Json body is tracked in #204.
    let store_root = resolve_store_root()?;
    let store = FsStore::new(store_root)?;
    let entries = store
        .list_recent(limit)
        .context("failed to list recent store entries")?;

    if mode == super::output::OutputMode::Quiet {
        return Ok(());
    }

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(out, "safekey\tyear\ttitle\tfetched_at")
        .context("failed to write list-recent header to stdout")?;
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
        .context("failed to write list-recent row to stdout")?;
    }
    Ok(())
}
