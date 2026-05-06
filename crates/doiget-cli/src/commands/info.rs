//! `doiget info <ref>` subcommand — read-only metadata inspection.
//!
//! Reads the metadata TOML for a given [`Ref`] from the [`FsStore`] and
//! prints it on stdout. Network access is never required.
//!
//! Per [`docs/PUBLIC_API.md`](../../../../docs/PUBLIC_API.md) §2 the read goes
//! through the [`Store::read`] trait method, so any future store backend
//! (e.g. a SQLite index in Phase 2) is a drop-in replacement.

use std::io::Write;

use anyhow::{bail, Context, Result};

use doiget_core::store::{FsStore, Store};
use doiget_core::Ref;

use super::resolve_store_root;

/// Run the `info` subcommand against the configured store.
///
/// `input` is the user-supplied ref string (e.g. `"10.1234/example"`,
/// `"arxiv:2401.12345"`, or any of the schemes accepted by [`Ref::parse`]).
///
/// On success, the entry's [`Metadata`](doiget_core::store::Metadata) is
/// written to stdout as TOML. On a missing entry, the function returns an
/// error so the CLI exits non-zero — the caller (a shell pipeline) can
/// distinguish "entry not in store" from "entry was empty".
pub fn run(input: String) -> Result<()> {
    let ref_ = Ref::parse(&input).with_context(|| format!("invalid ref: {input}"))?;
    let safekey = ref_.safekey();

    let store_root = resolve_store_root()?;
    let store = FsStore::new(store_root)?;

    let metadata = store
        .read(&safekey)
        .with_context(|| format!("failed to read store entry for {input}"))?;

    match metadata {
        Some(m) => {
            // Re-serialize to TOML for stdout. We use `toml::to_string_pretty`
            // for human readability; this is NOT the §7-normalized form
            // written to disk, but `info` is a presentation surface, not a
            // round-tripper.
            let s = toml::to_string_pretty(&m)
                .context("failed to serialize metadata to TOML for stdout")?;
            // Workspace lints deny `print_stdout` (the `print!`/`println!`
            // macros) so JSON-RPC frames never collide with diagnostics.
            // `writeln!` / `write!` against an explicit `stdout().lock()`
            // is the sanctioned escape hatch — the caller chose stdout
            // explicitly. See `docs/SECURITY.md` §3 / ADR-0001.
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            write!(out, "{s}").context("failed to write metadata to stdout")?;
            // toml::to_string_pretty already emits a trailing newline, but
            // be defensive in case a future toml-rs revision changes that.
            if !s.ends_with('\n') {
                writeln!(out).context("failed to write trailing newline")?;
            }
            Ok(())
        }
        None => bail!("no entry for {input}"),
    }
}
