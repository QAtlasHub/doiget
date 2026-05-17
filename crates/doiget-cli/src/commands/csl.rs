//! `doiget csl <ref>` — emit a CSL JSON 1.0 array for a stored Metadata.
//!
//! Reads the stored [`Metadata`](doiget_core::store::Metadata) for a
//! [`Ref`] and writes a single-element CSL JSON 1.0 **array** (a drop-in
//! for citeproc-js / pandoc `--csl-json` consumers that expect a list)
//! on stdout. Rendering lives in
//! [`doiget_core::store::render::to_csl_array`] so the CLI and the
//! `doiget_csl_export` MCP tool share one implementation (Slice 15b).
//!
//! Reads go through [`Store::read`]; network access is never required.

use std::io::Write;

use anyhow::{bail, Context, Result};

use doiget_core::store::{render, FsStore, Store};
use doiget_core::Ref;

use super::resolve_store_root;

/// Run the `csl` subcommand against the configured store.
///
/// `input` is the user-supplied ref string (anything accepted by
/// [`Ref::parse`]). On success a single-element CSL JSON 1.0 array is
/// written to stdout; a missing entry returns an error so the CLI exits
/// non-zero (pipelines can distinguish "not in store" from "empty").
pub fn run(input: String) -> Result<()> {
    let ref_ = Ref::parse(&input).with_context(|| format!("invalid ref: {input}"))?;
    let safekey = ref_.safekey();

    let store_root = resolve_store_root()?;
    let store = FsStore::new(store_root)?;

    let metadata = store
        .read(&safekey)
        .with_context(|| format!("failed to read store entry for {input}"))?;

    let metadata = match metadata {
        Some(m) => m,
        None => bail!("no entry for {input}"),
    };

    let array = render::to_csl_array(safekey.as_str(), &metadata);
    let json =
        serde_json::to_string_pretty(&array).context("failed to serialize CSL JSON for stdout")?;

    // Workspace lints deny `print_stdout` (`println!`/`print!`); the
    // sanctioned escape hatch is `writeln!(stdout().lock(), ...)`.
    // See `docs/SECURITY.md` §3 / ADR-0001 — guarantees JSON-RPC frames
    // never collide with diagnostics.
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(out, "{json}").context("failed to write CSL JSON to stdout")?;
    Ok(())
}
