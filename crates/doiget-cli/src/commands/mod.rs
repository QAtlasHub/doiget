//! Subcommand implementations for the `doiget` CLI.
//!
//! Each module corresponds to a single `clap` subcommand declared in
//! `main.rs`. The dispatch table in `main.rs` calls `run(...)` on the
//! matching module. Subcommands return `anyhow::Result<()>`; any error
//! surfaces via the CLI's top-level error reporter (stderr).
//!
//! ## Phase 1 surface (so far)
//!
//! - [`config`] — `doiget config show/path/doctor`.
//! - [`fetch`] — `doiget fetch <ref>` orchestrator (arXiv E2E + DOI metadata-only).
//! - [`info`] — prints a stored entry's `Metadata` as TOML on stdout.
//! - [`list_recent`] — prints up to N most-recently-fetched entries.
//! - [`search`] — case-insensitive substring search over stored metadata.
//!
//! Other subcommands (`batch`, `bib`, `csl`, `audit-log`, `serve`) land in
//! separate PRs.

pub mod config;
pub mod fetch;
pub mod info;
pub mod list_recent;
pub mod search;

use anyhow::{Context, Result};
use camino::Utf8PathBuf;

/// Resolve the on-disk store root.
///
/// Resolution order (subset of `docs/CONFIG.md` §4 — full CLI-flag /
/// config-file resolution lands with the `config` subcommand):
///
/// 1. `DOIGET_STORE_ROOT` environment variable, if set and non-empty.
/// 2. Fallback to `$HOME/papers` (POSIX) or `%USERPROFILE%\papers` (Windows).
///
/// The env-var hook is sufficient for both real use and integration tests
/// — tests set `DOIGET_STORE_ROOT` to a `tempfile::TempDir` to keep the
/// real `~/papers/` untouched.
pub(crate) fn resolve_store_root() -> Result<Utf8PathBuf> {
    if let Ok(s) = std::env::var("DOIGET_STORE_ROOT") {
        if !s.is_empty() {
            return Ok(Utf8PathBuf::from(s));
        }
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .context(
            "could not determine home directory: \
             neither HOME nor USERPROFILE is set, and DOIGET_STORE_ROOT was not provided",
        )?;
    Ok(Utf8PathBuf::from(home).join("papers"))
}
