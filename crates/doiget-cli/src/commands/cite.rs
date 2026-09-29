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

use doiget_core::metadata_quality::repair;
use doiget_core::orchestrator::{cite_metadata, resolve_only, MetadataOnlyOutcome};
use doiget_core::software::{resolve_github, zenodo_doi, GithubRef, ZenodoDoi};
use doiget_core::store::{FsStore, Metadata, Store};
use doiget_core::{CapabilityProfile, Doi, Ref};

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
pub async fn run(
    input: String,
    offline: bool,
    zenodo_version: bool,
    keys: super::KeyOptions,
    _mode: super::output::OutputMode,
) -> Result<()> {
    // #614: a GitHub repository or release is software, cited from GitHub
    // itself; it has no DOI to parse and no store entry to fall back to.
    if let Some(g) = GithubRef::parse(&input) {
        return cite_github(&g, offline, keys).await;
    }
    // #500: a PubMed id is cited under the DOI PubMed lists for it.
    let ref_ = super::parse_ref_or_pubmed(&input, !offline).await?;
    // Validated before any network work, so a template typo is not
    // reported after a resolve that took seconds (#610).
    let keys = keys.with_config_defaults()?;

    // `--offline`: render straight from the store, no network at all.
    if offline {
        let bib = bib_from_store(&ref_, &keys)?.ok_or_else(|| {
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
            // #608: a Crossref record that lost characters to U+FFFD is
            // repaired from an enabled source when one matches it, and the
            // rest is named on stderr -- the entry compiles either way, so
            // this is the only place the damage is visible before the
            // bibliography is rendered.
            if outcome.source == "datacite" {
                choose_zenodo_doi(&mut metadata, &outcome, zenodo_version);
            }
            let quality = repair(&mut metadata, &profile, &ctx).await;
            for line in super::metadata_quality_lines(
                &quality.repaired,
                &quality.flags(),
                profile.metadata.semantic_scholar || profile.metadata.openalex,
            ) {
                print_err(format_args!("{line}"));
            }
            let bib = keys.bibtex(
                &metadata,
                ref_.safekey().as_str(),
                &mut std::collections::HashSet::new(),
            )?;
            keys.report();
            write_bib(&bib)
        }
        Err(e) => {
            // Live resolve failed. Fall back to the store so an
            // already-fetched ref still cites (issue #305) — but never a
            // silent empty stdout: a ref that is in neither place is a
            // non-zero error carrying the original resolve failure.
            match bib_from_store(&ref_, &keys)? {
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

/// Cite a GitHub repository or release as `@software` (#614).
async fn cite_github(g: &GithubRef, offline: bool, keys: super::KeyOptions) -> Result<()> {
    if offline {
        return Err(anyhow!(
            "--offline: a GitHub URL is cited from GitHub itself; there is no store entry to render"
        ));
    }
    let keys = keys.with_config_defaults()?;
    let ctx = crate::commands::fetch::build_resolve_context()?;
    let cited = match resolve_github(g, &ctx).await {
        Ok(c) => c,
        Err(e) => {
            if let Some(why) = doiget_core::software::explain(&e) {
                print_err(format_args!("note: {why}"));
            }
            return Err(anyhow::Error::new(e).context(format!("failed to cite {}", g.html_url())));
        }
    };
    for note in &cited.notes {
        print_err(format_args!("note: {note}"));
    }
    let bib = keys.bibtex(
        &cited.metadata,
        &g.default_key(),
        &mut std::collections::HashSet::new(),
    )?;
    keys.report();
    write_bib(&bib)
}

/// Zenodo gives every release its own DOI and one concept DOI for all of
/// them (#614). A citation of the software means the concept, so a version
/// DOI is cited by its concept unless `--zenodo-version` asks otherwise --
/// and either way stderr says which DOI the entry carries.
fn choose_zenodo_doi(metadata: &mut Metadata, outcome: &MetadataOnlyOutcome, keep_version: bool) {
    let given = metadata
        .doi
        .as_ref()
        .map(|d| d.as_str().to_string())
        .unwrap_or_default();
    match zenodo_doi(&outcome.metadata) {
        Some(ZenodoDoi::Version { concept }) if !keep_version => match Doi::parse(&concept) {
            Ok(d) => {
                print_err(format_args!(
                    "note: cited the concept DOI {concept}, which names every version; {given} is one version (pass --zenodo-version to cite it)"
                ));
                metadata.doi = Some(d);
                // The version's number and landing page describe that
                // version, not the concept.
                metadata.other.remove("version");
                metadata.url = None;
            }
            Err(_) => print_err(format_args!(
                "note: {given} names {concept:?} as its concept DOI, which is not a DOI; citing {given}"
            )),
        },
        Some(ZenodoDoi::Version { concept }) => print_err(format_args!(
            "note: cited version DOI {given} as asked; its concept DOI, for every version, is {concept}"
        )),
        Some(ZenodoDoi::Concept) => print_err(format_args!(
            "note: {given} is a concept DOI: it names every version of this record"
        )),
        None => {}
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
fn bib_from_store(ref_: &Ref, keys: &super::KeyOptions) -> Result<Option<String>> {
    let store = FsStore::new(resolve_store_root()?)?;
    let safekey = ref_.safekey();
    doiget_core::store::blocking_section(|| store.read(&safekey))?
        .map(|m| keys.bibtex(&m, safekey.as_str(), &mut std::collections::HashSet::new()))
        .transpose()
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

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn datacite_outcome(related: serde_json::Value) -> MetadataOnlyOutcome {
        serde_json::from_value(serde_json::json!({
            "source": "datacite",
            "resolver_profile": "test",
            "license": null,
            "oa_url": null,
            "metadata": {"relatedIdentifiers": related},
        }))
        .expect("outcome")
    }

    fn zenodo_record() -> Metadata {
        let mut m = Metadata {
            doi: Some(Doi::parse("10.5281/zenodo.200").unwrap()),
            url: Some("https://zenodo.org/records/200".into()),
            ..Metadata::default()
        };
        m.other
            .insert("version".into(), toml::Value::String("v1.2".into()));
        m
    }

    /// #614: a version DOI is cited by its concept by default, and that
    /// version's number and page go with it; --zenodo-version keeps it.
    #[test]
    fn a_zenodo_version_doi_is_cited_by_its_concept_unless_asked_not_to() {
        let related = serde_json::json!([{
            "relationType": "IsVersionOf", "relatedIdentifierType": "DOI",
            "relatedIdentifier": "10.5281/zenodo.100"
        }]);
        let mut m = zenodo_record();
        choose_zenodo_doi(&mut m, &datacite_outcome(related.clone()), false);
        assert_eq!(
            m.doi.as_ref().map(|d| d.as_str()),
            Some("10.5281/zenodo.100")
        );
        assert!(!m.other.contains_key("version"));
        assert!(m.url.is_none());

        let mut kept = zenodo_record();
        choose_zenodo_doi(&mut kept, &datacite_outcome(related), true);
        assert_eq!(
            kept.doi.as_ref().map(|d| d.as_str()),
            Some("10.5281/zenodo.200")
        );
        assert!(kept.other.contains_key("version"));
    }

    #[test]
    fn a_concept_doi_or_an_unusable_concept_leaves_the_doi_as_given() {
        let concept = serde_json::json!([{
            "relationType": "HasVersion", "relatedIdentifierType": "DOI",
            "relatedIdentifier": "10.5281/zenodo.201"
        }]);
        let mut m = zenodo_record();
        choose_zenodo_doi(&mut m, &datacite_outcome(concept), false);
        assert_eq!(
            m.doi.as_ref().map(|d| d.as_str()),
            Some("10.5281/zenodo.200")
        );

        let broken = serde_json::json!([{
            "relationType": "IsVersionOf", "relatedIdentifierType": "DOI",
            "relatedIdentifier": "not a doi"
        }]);
        let mut m = zenodo_record();
        choose_zenodo_doi(&mut m, &datacite_outcome(broken), false);
        assert_eq!(
            m.doi.as_ref().map(|d| d.as_str()),
            Some("10.5281/zenodo.200")
        );
        assert!(
            m.other.contains_key("version"),
            "nothing is dropped without a concept"
        );
    }
}
