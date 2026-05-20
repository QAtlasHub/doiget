//! `doiget bib <ref>` subcommand — emit a BibTeX entry for a stored entry.
//!
//! Reads the stored [`Metadata`](doiget_core::store::Metadata) for a
//! [`Ref`] and writes a BibTeX entry on stdout. The actual rendering
//! lives in [`doiget_core::store::render::to_bibtex`] so the CLI and the
//! `doiget_bibtex_export` MCP tool share one implementation (Slice 15b).
//! See that function's docs for the field/entry-type mapping and the
//! brace-stripping policy.

use std::io::Write;

use anyhow::{bail, Context, Result};

use doiget_core::store::{render, FsStore, Store};
use doiget_core::Ref;

use super::resolve_store_root;

/// Run the `bib` subcommand against the configured store.
///
/// `input` is the user-supplied ref string (e.g. `"10.1234/example"`,
/// `"arxiv:2401.12345"`, or any of the schemes accepted by [`Ref::parse`]).
///
/// On success, a BibTeX entry derived from the entry's `Metadata` is
/// written to stdout. On a missing entry, the function returns an error
/// so the CLI exits non-zero.
pub fn run(input: String, _mode: super::output::OutputMode) -> Result<()> {
    // `_mode` is threaded per ADR-0017 / #144. Bib's BibTeX output is
    // product output (the requested artifact), not informational, so
    // Quiet does NOT suppress it. No follow-up issue is needed here.
    let ref_ = Ref::parse(&input).with_context(|| format!("invalid ref: {input}"))?;
    let safekey = ref_.safekey();

    let store_root = resolve_store_root()?;
    let store = FsStore::new(store_root)?;

    let metadata = store
        .read(&safekey)
        .with_context(|| format!("failed to read store entry for {input}"))?;

    match metadata {
        Some(m) => {
            let bib = render::to_bibtex(safekey.as_str(), &m);
            // Workspace lints deny `print_stdout` (the `print!`/`println!`
            // macros) so JSON-RPC frames never collide with diagnostics.
            // `write!` against an explicit `stdout().lock()` is the
            // sanctioned escape hatch — `to_bibtex` already terminates
            // the entry with `}\n`, so no extra newline is added.
            // See `docs/SECURITY.md` §3 / ADR-0001.
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            write!(out, "{bib}").context("failed to write BibTeX entry to stdout")?;
            Ok(())
        }
        None => bail!("no entry for {input}"),
    }
}
