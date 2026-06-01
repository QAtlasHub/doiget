//! `doiget cite <ref>` subcommand — resolve a DOI / arXiv reference to a
//! clean BibTeX entry on stdout, a `doi2bib`-style citation helper.
//!
//! Unlike [`bib`](super::bib) (which renders an entry already in the local
//! store), `cite` resolves the reference **live** — cache-aware via the
//! resolver cache (`docs/CACHE.md`), so repeat citations of the same ref
//! avoid upstream rate limits — and never writes to the store. The DOI
//! path enriches the entry from the Crossref envelope
//! ([`doiget_core::orchestrator::cite_metadata`]) so the output carries
//! year / journal / publisher / ISSN, not just the bare id.
//!
//! Rendering (field mapping, brace-stripping, HTML/MathML tag scrubbing)
//! is shared with `doiget bib` via
//! [`doiget_core::store::render::to_bibtex`].

use std::io::Write;

use anyhow::{Context, Result};

use doiget_core::orchestrator::{cite_metadata, resolve_only};
use doiget_core::store::render;
use doiget_core::{CapabilityProfile, Ref};

/// Run the `cite` subcommand.
///
/// `input` is the user-supplied ref string (a DOI, `arxiv:<id>`, or any
/// scheme accepted by [`Ref::parse`]).
///
/// On success a BibTeX entry is written to stdout. Like `bib`, the BibTeX
/// is the requested artifact (product output, not a diagnostic), so
/// `--quiet` does NOT suppress it. A resolve failure returns an error so
/// the CLI exits non-zero.
pub async fn run(input: String, _mode: super::output::OutputMode) -> Result<()> {
    let ref_ = Ref::parse(&input).with_context(|| format!("invalid ref: {input}"))?;

    let ctx = crate::commands::fetch::build_resolve_context()?;
    let profile = CapabilityProfile::from_env().context("resolving capability profile")?;

    let outcome = resolve_only(&ref_, &profile, &ctx)
        .await
        .with_context(|| format!("failed to resolve {input}"))?;

    let metadata = cite_metadata(&ref_, &outcome);
    let bib = render::to_bibtex(ref_.safekey().as_str(), &metadata);

    // Workspace lints deny the `print!`/`println!` macros so JSON-RPC
    // frames never collide with diagnostics; `write!` against an explicit
    // `stdout().lock()` is the sanctioned escape hatch. `to_bibtex` already
    // terminates the entry with `}\n`, so no extra newline is added.
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    write!(out, "{bib}").context("failed to write BibTeX entry to stdout")?;
    Ok(())
}
