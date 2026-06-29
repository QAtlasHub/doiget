//! Subcommand implementations for the `doiget` CLI.
//!
//! Each module corresponds to a single `clap` subcommand declared in
//! `main.rs`. The dispatch table in `main.rs` calls `run(...)` on the
//! matching module. Subcommands return `anyhow::Result<()>`; any error
//! surfaces via the CLI's top-level error reporter (stderr).
//!
//! ## Phase 1 surface (so far)
//!
//! - [`audit_log`] — `doiget audit-log --verify` recomputes the SHA-256 hash
//!   chain on the provenance log and reports any mismatches.
//! - [`batch`] — `doiget batch <path>` multi-ref orchestrator (rate-bounded).
//! - [`bib`] — `doiget bib <ref>` BibTeX exporter (Phase 2 starter).
//! - [`cite`] — `doiget cite <ref>` live-resolve BibTeX (doi2bib-style).
//! - [`config`] — `doiget config show/path/doctor`.
//! - [`csl`] — `doiget csl <ref>` exports a stored entry as CSL JSON 1.0.
//! - [`fetch`] — `doiget fetch <ref>` orchestrator (arXiv E2E + DOI metadata-only).
//! - [`info`] — prints a stored entry's `Metadata` as TOML on stdout.
//! - [`list_recent`] — prints up to N most-recently-fetched entries.
//! - [`search`] — case-insensitive substring search over stored metadata.
//!
//! Other subcommands (`serve`) land in separate PRs.

pub mod audit_log;
pub mod batch;
pub mod bib;
pub mod capabilities;
pub mod cite;
pub mod config;
pub mod csl;
pub mod fetch;
pub mod frontier;
pub mod info;
pub mod link;
pub mod lint;
pub mod list_recent;
pub mod output;
pub mod provenance;
pub mod resolve_citation;
pub mod search;
pub mod source;
pub mod tag;
pub mod tex_source;
pub mod text;
pub mod verify;
pub mod version;

// Phase 4 / Slice 16. Compile-gated by the `citation` Cargo feature
// (which itself enables `doiget-core/citation`).
#[cfg(feature = "citation")]
pub mod graph;

use anyhow::{Context, Result};
use camino::Utf8PathBuf;

/// Resolve the on-disk store root.
///
/// Resolution order (subset of `docs/CONFIG.md` §4 — full CLI-flag /
/// config-file resolution lands with the `config` subcommand):
///
/// 1. `DOIGET_STORE_ROOT` environment variable, if set and non-empty.
/// 2. Fallback to `./papers` — `papers/` directly under the current working
///    directory (#344 / ADR-0036), so fetched artifacts are visible where the
///    user (or an LLM agent) is working rather than hidden in a far-off home
///    directory. For a central, shared library set `DOIGET_STORE_ROOT`
///    (e.g. `~/papers`, which also restores BiblioFetch.jl co-location —
///    ADR-0004).
///
/// The env-var hook is sufficient for both real use and integration tests
/// — tests set `DOIGET_STORE_ROOT` to a `tempfile::TempDir` to keep the
/// real working directory untouched.
pub(crate) fn resolve_store_root() -> Result<Utf8PathBuf> {
    if let Ok(s) = std::env::var("DOIGET_STORE_ROOT") {
        // Ignore an empty value or an unexpanded "${...}" placeholder — a
        // Desktop-Extension config left blank can pass the literal
        // "${user_config.store_root}", which must not become a path (#369).
        let s = s.trim();
        if !s.is_empty() && !s.contains("${") {
            return Ok(Utf8PathBuf::from(s));
        }
    }
    let cwd = std::env::current_dir().context(
        "could not determine the current working directory for the default store root \
         (set DOIGET_STORE_ROOT to choose an explicit store location)",
    )?;
    Utf8PathBuf::from_path_buf(cwd)
        .map(|d| d.join("papers"))
        .map_err(|p| anyhow::anyhow!("current directory path is not UTF-8: {}", p.display()))
}
