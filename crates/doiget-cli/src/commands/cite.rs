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
//! ## Offline resilience (issue #305)
//!
//! A live resolve that fails (network hiccup, OpenAlex flake) does NOT
//! discard an already-fetched reference: `cite` falls back to the local
//! store and renders the stored metadata, with a `note:` on stderr so the
//! offline path is visible. `--offline` skips the live resolve entirely
//! (store-only). Either way a total miss is a non-zero error, never a
//! silent empty stdout (the #302 / #304 "never exit 0 with nothing"
//! contract).
//!
//! Rendering (field mapping, brace-stripping, HTML/MathML tag scrubbing)
//! is shared with `doiget bib` via
//! [`doiget_core::store::render::to_bibtex`].
//!
//! ## Relation to doi2bib
//!
//! This command is functionally comparable to the `doi2bib` tool, and the
//! "doi2bib-style" / "doi2bib-quality" phrasing throughout doiget is a
//! descriptive comparison only. `cite` is an **independent, clean-room
//! implementation** built on doiget's own Crossref/arXiv resolver and
//! `to_bibtex` renderer; it incorporates **no code** from any external
//! doi2bib project. In particular it does not derive from the AGPL-3.0
//! `doi2bib` at <https://github.com/vandroogenbroeckmarc/doi2bib> — none
//! of that project's source, field-correction heuristics, or
//! Unicode→ASCII tables are used here, so doiget remains MIT-licensed.

use std::io::Write;

use anyhow::{anyhow, Context, Result};

use doiget_core::orchestrator::{cite_metadata, resolve_only, MetadataOnlyOutcome};
use doiget_core::store::{render, FsStore, Metadata, Store};
use doiget_core::{CapabilityProfile, Ref};

use super::output::print_err;
use super::resolve_store_root;

/// Run the `cite` subcommand.
///
/// `input` is the user-supplied ref string (a DOI, `arxiv:<id>`, or any
/// scheme accepted by [`Ref::parse`]). When `offline` is set the live
/// resolve is skipped and the entry is rendered from the local store;
/// otherwise a live resolve is attempted first and the store is used as a
/// fallback when it fails.
///
/// On success a BibTeX entry is written to stdout. Like `bib`, the BibTeX
/// is the requested artifact (product output, not a diagnostic), so
/// `--quiet` does NOT suppress it. A total miss (no live resolve and no
/// store entry) returns an error so the CLI exits non-zero.
pub async fn run(input: String, offline: bool, _mode: super::output::OutputMode) -> Result<()> {
    let ref_ = super::parse_ref_or_exit(&input)?;

    // `--offline`: render straight from the store, no network at all.
    if offline {
        let bib = bib_from_store(&ref_)?.ok_or_else(|| {
            anyhow!(
                "--offline: no local store entry for {input} (fetch it first with `doiget fetch`)"
            )
        })?;
        return write_bib(&bib);
    }

    let ctx = crate::commands::fetch::build_resolve_context()?;
    let profile = CapabilityProfile::from_env().context("resolving capability profile")?;

    match resolve_only(&ref_, &profile, &ctx).await {
        Ok(outcome) => {
            let mut metadata = cite_metadata(&ref_, &outcome);
            // #303 published-version merge: when an arXiv preprint's Atom
            // feed cross-references a published journal DOI (`<arxiv:doi>`),
            // resolve that DOI (Crossref) and prefer its rich `@article`
            // fields (journal / volume / issue / pages / publisher / issn /
            // doi) while RETAINING the arXiv preprint identity
            // (eprint / archivePrefix / primaryClass). Best-effort: a
            // missing or unresolvable cross-ref keeps the `@misc` preprint
            // entry, never failing the cite. No extra OpenAlex call — the
            // DOI comes free from the Atom feed already fetched.
            if let Some(doi_ref) = published_doi_ref(&ref_, &outcome) {
                match resolve_only(&doi_ref, &profile, &ctx).await {
                    Ok(doi_outcome) => {
                        metadata = merge_published(cite_metadata(&doi_ref, &doi_outcome), metadata);
                    }
                    // Best-effort, but make the degradation VISIBLE (review
                    // #318): a published version exists yet could not be
                    // resolved, so we fall back to the @misc preprint — say
                    // so on stderr rather than silently.
                    Err(e) => print_err(format_args!(
                        "note: published-version DOI resolve failed ({e}); citing the arXiv preprint"
                    )),
                }
            }
            let bib = render::to_bibtex(ref_.safekey().as_str(), &metadata);
            write_bib(&bib)
        }
        Err(e) => {
            // Live resolve failed. Fall back to the store so an
            // already-fetched ref still cites (issue #305) — but never a
            // silent empty stdout: a ref that is in neither place is a
            // non-zero error carrying the original resolve failure.
            match bib_from_store(&ref_)? {
                Some(bib) => {
                    print_err(format_args!(
                        "note: live resolve failed ({e}); citing offline from the local store"
                    ));
                    write_bib(&bib)
                }
                None => Err(anyhow::Error::new(e).context(format!(
                    "failed to resolve {input}, and no local store entry to cite offline"
                ))),
            }
        }
    }
}

/// The published-journal DOI an arXiv Atom feed cross-references via
/// `<arxiv:doi>` (issue #303), as a `Ref::Doi`. `None` for a DOI input, an
/// absent cross-ref, or a malformed DOI (a bad cross-ref is simply ignored
/// rather than failing the cite).
fn published_doi_ref(ref_: &Ref, outcome: &MetadataOnlyOutcome) -> Option<Ref> {
    if !matches!(ref_, Ref::Arxiv(_)) {
        return None;
    }
    let doi = outcome.metadata.get("doi").and_then(|v| v.as_str())?;
    // Narrow to a DOI: a URL-form or arXiv-shaped cross-ref must NOT trigger
    // a spurious second arXiv resolve against the wrong id (review #318). A
    // value that does not parse as a bare DOI is simply ignored.
    match Ref::parse(doi).ok()? {
        r @ Ref::Doi(_) => Some(r),
        Ref::Arxiv(_) => None,
    }
}

/// Merge the arXiv preprint identity into the published DOI's `@article`
/// metadata: keep the rich Crossref entry and graft on the arXiv id +
/// categories so `to_bibtex` still emits `eprint` / `archivePrefix` /
/// `primaryClass`. The published record wins on every shared field — it is
/// the version a reader should cite — with the preprint retained for
/// discoverability.
fn merge_published(mut article: Metadata, arxiv: Metadata) -> Metadata {
    article.arxiv_id = arxiv.arxiv_id;
    article.arxiv_categories = arxiv.arxiv_categories;
    article
}

/// Render the stored BibTeX for `ref_`, or `None` when the store has no
/// entry. The citation key is the entry's safekey, matching `bib`.
fn bib_from_store(ref_: &Ref) -> Result<Option<String>> {
    let store = FsStore::new(resolve_store_root()?)?;
    let safekey = ref_.safekey();
    Ok(store
        .read(&safekey)?
        .map(|m| render::to_bibtex(safekey.as_str(), &m)))
}

/// Write a rendered BibTeX entry to stdout. `to_bibtex` already terminates
/// the entry with `}\n`, so no extra newline is added. Workspace lints deny
/// `print!`/`println!`; `write!` against an explicit `stdout().lock()` is
/// the sanctioned escape hatch (ADR-0001).
fn write_bib(bib: &str) -> Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    write!(out, "{bib}").context("failed to write BibTeX entry to stdout")
}
