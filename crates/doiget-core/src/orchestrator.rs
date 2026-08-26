//! Cross-source orchestrators that compose multiple [`Source`] impls into
//! a single user-facing operation.
//!
//! Slice 2 of the doiget roadmap promotes [`fetch_paper`] and
//! [`batch_fetch`] from `doiget-cli` into this module so the MCP server
//! (`doiget-mcp`) and the CLI share one source of truth for the per-ref
//! orchestration. The CLI's `commands::fetch::fetch_one` is now a thin
//! wrapper that delegates here and adds the human-facing stderr print
//! line. Dry-run preview helpers live as [`fetch_paper_plan`] and
//! [`batch_fetch_plans`].
//!
//! [`Source`]: crate::source::Source

use std::collections::BTreeMap;

use camino::{Utf8Path, Utf8PathBuf};
use chrono::Utc;
use serde_json::Value;

use crate::dry_run::{build_fetch_plan, try_build_fetch_plan, FetchPlan};
use crate::http::HttpError;
use crate::provenance::{Capability, LogEvent, LogResult, RowInput};
use crate::source::{FetchContext, FetchError, FetchResult, Source};
use crate::sources::arxiv::ArxivSource;
use crate::sources::crossref::CrossrefSource;
use crate::sources::unpaywall::UnpaywallSource;
use crate::store::{DoigetExtension, Metadata, Store};
use crate::DenialContext;
use crate::{ArxivId, CapabilityProfile, Doi, Ref, Safekey, MAX_BATCH_REFS, SCHEMA_VERSION};

/// Outcome of a successful [`metadata_only`] call.
///
/// Mirrors the wire shape documented in `docs/MCP_TOOLS.md` §11: the
/// `source` identifies which resolver produced the metadata, `license`
/// is the OA license string when known (Unpaywall channel), `oa_url` is
/// the discovered OA URL **(never followed by this orchestrator)**, and
/// `metadata` is the source's native JSON payload (Crossref `message`,
/// Unpaywall work record, or the parsed arXiv Atom-feed object).
///
/// `metadata` is serialized as-is by the MCP envelope builder
/// (`crates/doiget-mcp/src/lib.rs`); we deliberately do NOT normalize
/// here so the agent can see exactly what the source returned.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct MetadataOnlyOutcome {
    /// Resolver key that produced the metadata payload. One of
    /// `"crossref"`, `"unpaywall"`, `"arxiv"` (the closed set named in
    /// `docs/MCP_TOOLS.md` §11 type alias).
    pub source: String,
    /// Resolver profile under which the canonical-digest (ADR-0021 §1)
    /// was minted for this call. In Slice 4 this equals
    /// [`Self::source`] verbatim (the metadata-only path emits one row
    /// per consulted resolver); future slices that introduce overlapping
    /// resolvers MAY have `resolver_profile != source`. Surfaced through
    /// the `doiget_metadata_only` MCP envelope per ADR-0021 §4.
    pub resolver_profile: String,
    /// OA license string when the resolver could supply one (today only
    /// the Unpaywall fallback path populates this). `None` when the
    /// primary source did not surface a license.
    pub license: Option<String>,
    /// Discovered OA URL — surfaced to the caller for separate action,
    /// **never followed by this orchestrator**. The Crossref response's
    /// `message.link[]` array is mined first; the Unpaywall fallback
    /// path uses `best_oa_location.url_for_pdf` (or `url`).
    pub oa_url: Option<String>,
    /// Open-access status for the ref, when known (#281 item 4): the
    /// Unpaywall classification `gold` / `green` / `hybrid` / `bronze` /
    /// `closed`, or `"green"` for an arXiv ref. `None` when the resolver
    /// that answered did not surface it — notably the Crossref-first
    /// metadata path, which does NOT consult Unpaywall unless Crossref
    /// fails, so a `None` here means "not determined", not "no OA".
    /// `#[serde(default)]` keeps older `resolver_cache` entries (written
    /// before this field existed) readable.
    #[serde(default)]
    pub oa_status: Option<String>,
    /// Source's native metadata payload. For Crossref this is the
    /// `message` object; for Unpaywall the work record; for arXiv the
    /// parsed Atom-feed JSON (see
    /// `crate::sources::arxiv::parse_atom_feed`).
    pub metadata: Value,
}

/// Resolve a [`Ref`] to metadata WITHOUT triggering a publisher PDF
/// fetch.
///
/// Binding spec: `docs/MCP_TOOLS.md` §11 (NORMATIVE — this function
/// MUST NOT call [`crate::http::HttpClient::fetch_pdf`] under any code
/// path). The posture-lint workflow greps for that pattern; the test
/// suite additionally exercises the DOI and arXiv branches end-to-end
/// against wiremock to assert the OA URL is reported, not followed.
///
/// # Dispatch
///
/// - `Ref::Doi(_)` → Crossref first (bibliographic metadata + OA URL
///   via `message.link[]`). If Crossref returns a usable payload the
///   call returns immediately; Unpaywall is consulted only as a fallback
///   when Crossref fails. The Unpaywall fallback surfaces a license
///   string and may overwrite `oa_url` with the `best_oa_location`
///   channel.
/// - `Ref::Arxiv(_)` → [`ArxivSource::fetch_metadata_only`]: ONLY the
///   Atom feed (`https://export.arxiv.org/api/query?id_list=<id>`) is
///   consulted; the PDF endpoint is NOT touched. `license` is set to
///   the platform-wide `"arxiv-default"` token, `oa_url` is `None`
///   (the arXiv abstract page is not a PDF URL).
///
/// # Side effects
///
/// Each consulted source appends ONE `LogEvent::Fetch` row to
/// `ctx.log` (arXiv emits its row under `Capability::Metadata`; the
/// DOI sources emit under `Capability::Oa` — they pre-date this
/// distinction and a follow-up slice may unify them). The orchestrator
/// itself does NOT bracket the call with `SessionStart` / `SessionEnd`
/// rows — that is the MCP server's responsibility (it owns the
/// per-tool-call session boundary).
///
/// This function is the **pure resolver**: it consults the source(s)
/// and emits provenance rows, but it does NOT write to the store.
/// The `docs/MCP_TOOLS.md` §11 store-write SIDE EFFECT is provided by
/// [`metadata_only_to_store`], which wraps this and persists the
/// metadata TOML to `<root>/.metadata/<safekey>.toml`. Keeping the
/// store-write in a *separate* entry point is exactly what lets
/// [`resolve_only`] safely delegate here — its contract forbids any
/// store write, and a pure `metadata_only` can never regress that
/// invariant (#139).
///
/// # Errors
///
/// Returns [`FetchError`] from the underlying [`Source`] dispatch. The
/// MCP boundary converts these to the closed [`crate::ErrorCode`] set
/// via the existing `From<FetchError> for ErrorCode` impl.
// Stays `pub` (a `pub(crate)` compile-time guard was considered and
// rejected): `crates/doiget-core/tests/` integration tests
// (`real_world_fixtures_e2e`) legitimately drive the PURE resolver
// directly and assert its outcome, and `tests/` compiles as a separate
// crate. The #139 pre-fix bug (an MCP caller
// picking the pure variant when it needed persistence) is instead
// prevented *structurally*: the MCP layer imports only
// `metadata_only_to_store`, and `resolve_only` delegates to this pure
// fn — neither can acquire or skip the store-write by mistake.
pub async fn metadata_only(
    ref_: &Ref,
    profile: &CapabilityProfile,
    ctx: &FetchContext,
) -> Result<MetadataOnlyOutcome, FetchError> {
    // Resolver cache (docs/CACHE.md): on a hit within TTL, return the
    // cached outcome without touching the network — this is what lets
    // `doiget verify` avoid upstream rate limits across repeated runs.
    //
    // The cache key is the ref's safekey only, so it MUST NOT be shared
    // across different resolver endpoints. When any `DOIGET_*_BASE`
    // override is set (wiremock in tests, or a non-production endpoint),
    // the cache is bypassed entirely — otherwise a wiremock-fabricated
    // entry could be written to (or read from) the real production cache
    // for the same ref, silently serving fake metadata.
    let cache_root = if resolver_base_overridden() {
        None
    } else {
        ctx.cache_root.as_deref()
    };

    if let Some(root) = cache_root {
        if let Some(cached) = crate::resolver_cache::read(root, ref_) {
            return Ok(cached);
        }
    }

    let outcome = match ref_ {
        Ref::Doi(doi) => metadata_only_doi(doi, ref_, profile, ctx).await?,
        Ref::Arxiv(id) => {
            let arxiv = arxiv_source_from_env();
            let metadata = arxiv.fetch_metadata_only(id, ctx).await?;
            // Pure resolver — no store write here (see fn doc); the
            // store-write side effect lives in `metadata_only_to_store`.
            MetadataOnlyOutcome {
                source: arxiv.name().to_string(),
                resolver_profile: arxiv.name().to_string(),
                license: Some("arxiv-default".to_string()),
                oa_url: None,
                // arXiv preprints are green OA by definition.
                oa_status: Some("green".to_string()),
                metadata,
            }
        }
    };

    // Best-effort cache write (never fails the resolve).
    if let Some(root) = cache_root {
        crate::resolver_cache::write(root, ref_, &outcome);
    }
    Ok(outcome)
}

/// Whether any resolver base-URL override is set. When true the resolver
/// cache is bypassed, so a non-production endpoint (wiremock in tests) can
/// never share cache entries with the real Crossref / Unpaywall / arXiv
/// endpoints for the same ref.
fn resolver_base_overridden() -> bool {
    [
        "DOIGET_CROSSREF_BASE",
        "DOIGET_UNPAYWALL_BASE",
        "DOIGET_ARXIV_BASE",
    ]
    .iter()
    .any(|k| std::env::var_os(k).is_some())
}

/// Resolve a [`Ref`] to metadata with **no local persistence**.
///
/// This is the audit-trail-preserving sibling of [`metadata_only`]: each
/// consulted [`Source`] still emits its own `LogEvent::Fetch` row
/// through `ctx.log` (so the provenance hash chain remains continuous,
/// per `docs/PROVENANCE_LOG.md`), but the orchestrator MUST NOT write
/// the metadata TOML to the store under any code path — present or
/// future.
///
/// Binding spec: `docs/MCP_TOOLS.md` §1 (the `doiget_resolve_paper`
/// tool — Slice 7).
///
/// # Why this exists as a distinct orchestrator
///
/// [`metadata_only`] is the **pure resolver** and never writes to the
/// store; the store-write SIDE EFFECT lives only in the separate
/// [`metadata_only_to_store`] wrapper. Because the write is in a
/// *different* entry point that this function does not call,
/// delegating to [`metadata_only`] is permanently safe — there is no
/// code path by which `resolve_only` can acquire a store write, now or
/// in future (#139). This structural separation is the entire reason
/// `metadata_only` was split into a pure core + a persisting wrapper
/// rather than gaining a `write: bool` parameter.
///
/// # Dispatch
///
/// Identical to [`metadata_only`] (DOI → Crossref-first with Unpaywall
/// fallback; arXiv → Atom feed only). The `oa_url` and `license`
/// outputs follow the same rules.
///
/// # Side effects
///
/// One `LogEvent::Fetch` row per consulted resolver, written by the
/// underlying [`Source`] impls. No metadata TOML write. No PDF fetch.
/// No store mutation.
///
/// # Errors
///
/// Returns [`FetchError`] from the underlying [`Source`] dispatch,
/// identical to [`metadata_only`].
pub async fn resolve_only(
    ref_: &Ref,
    profile: &CapabilityProfile,
    ctx: &FetchContext,
) -> Result<MetadataOnlyOutcome, FetchError> {
    // Delegating to the PURE `metadata_only` is the contract-correct
    // implementation, not a placeholder: `metadata_only` never writes
    // to the store (the persisting path is the separate
    // `metadata_only_to_store`, which this function does not call), so
    // `resolve_only`'s "no store mutation" guarantee holds structurally
    // and cannot regress (#139).
    metadata_only(ref_, profile, ctx).await
}

/// Resolve a [`Ref`] to metadata **and persist the metadata TOML to the
/// store** — the `docs/MCP_TOOLS.md` §11 `doiget_metadata_only` SIDE
/// EFFECT (#139).
///
/// Wraps the pure [`metadata_only`]: it runs the same resolver dispatch
/// (so the provenance hash chain is identical), then writes
/// `<root>/.metadata/<safekey>.toml` via the same
/// `write_metadata_and_pdf` path `fetch_paper` uses for its
/// metadata-only fallback, emitting one `StoreWrite` provenance row.
///
/// [`resolve_only`] MUST NOT call this — its contract forbids any store
/// write. The split (pure core vs. persisting wrapper) makes that
/// invariant structural rather than a convention.
///
/// # Errors
///
/// [`FetchError`] from the underlying resolver dispatch, or — if the
/// store write fails — [`FetchError::SourceSchema`] (the closest
/// closed-set arm; there is no dedicated `FetchError::StoreError`, so
/// the MCP boundary maps it to `INTERNAL_ERROR` — see the inline note
/// in `write_metadata_and_pdf`). On store-write failure
/// `write_metadata_and_pdf` makes a **best-effort** attempt to
/// append a `StoreWrite`/`Err` provenance row before the error
/// propagates (that append's own failure is not separately surfaced —
/// this matches the pre-existing `fetch_paper` metadata-only fallback
/// path and is out of scope for #139).
pub async fn metadata_only_to_store(
    ref_: &Ref,
    profile: &CapabilityProfile,
    ctx: &FetchContext,
    store: &dyn Store,
) -> Result<MetadataOnlyOutcome, FetchError> {
    let outcome = metadata_only(ref_, profile, ctx).await?;
    let safekey = ref_.safekey();
    let metadata = build_metadata_only_metadata(ref_, &outcome);
    // `pdf_src = None` => writes `<root>/.metadata/<safekey>.toml` and
    // appends the `StoreWrite` row (the exact path `fetch_paper` uses
    // for its DOI metadata-only fallback).
    write_metadata_and_pdf(store, &safekey, &metadata, None, ctx)?;
    Ok(outcome)
}

/// Build the [`Metadata`] persisted by [`metadata_only_to_store`].
///
/// Minimal but valid: enough that a subsequent `doiget_info` returns a
/// non-null `metadata` object (the #139 acceptance criterion). Title is
/// best-effort from the resolver payload (`title` as a string, or the
/// first element if it is an array — Crossref's `message.title` is
/// typically an array, arXiv/Unpaywall typically a string; the
/// extractor tolerates either regardless of source); it falls back to
/// the ref id so the required `title` field is never empty.
/// Bibliographic enrichment
/// (year, venue, …) is intentionally out of scope here — the
/// metadata-only contract is "persist what the resolver returned", and
/// the raw payload is preserved verbatim in `MetadataOnlyOutcome`.
fn build_metadata_only_metadata(ref_: &Ref, outcome: &MetadataOnlyOutcome) -> Metadata {
    let (doi, arxiv_id) = match ref_ {
        Ref::Doi(d) => (Some(d.clone()), None),
        Ref::Arxiv(a) => (None, Some(a.clone())),
    };
    let ref_id = ref_.as_input_str().to_string();
    let title = match extract_metadata_title(&outcome.metadata) {
        Some(t) => t,
        None => {
            // The resolver returned a payload with no usable title.
            // Persisting the ref id keeps the entry valid (#139), but
            // emit a diagnostic so a broken/partial resolver response is
            // not silently indistinguishable from a genuine title.
            tracing::warn!(
                ref_id = %ref_id,
                source = %outcome.source,
                "metadata-only: no usable title in resolver payload; \
                 persisting the ref id as the title placeholder"
            );
            ref_id
        }
    };
    Metadata {
        schema_version: SCHEMA_VERSION.to_string(),
        title,
        authors: extract_metadata_authors(&outcome.metadata),
        year: None,
        doi,
        arxiv_id,
        // Enriched by `cite_metadata`'s arXiv overlay (issue #303); the
        // metadata-only baseline leaves it empty like the other
        // bibliographic fields.
        arxiv_categories: Vec::new(),
        abstract_: None,
        venue: None,
        volume: None,
        issue: None,
        pages: None,
        publisher: None,
        issn: None,
        isbn: None,
        type_: None,
        keywords: Vec::new(),
        url: outcome.oa_url.clone(),
        pdf_path: None,
        doiget: Some(DoigetExtension {
            fetched_at: Utc::now(),
            source: outcome.source.clone(),
            license: outcome
                .license
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            oa_status: outcome.oa_status.clone(),
            size_bytes: 0,
            mcp_call_id: None,
            tags: Vec::new(),
            collections: Vec::new(),
            annotation: None,
        }),
        other: BTreeMap::new(),
    }
}

/// Build a doi2bib-quality [`Metadata`] for `doiget cite` from a
/// resolver outcome — WITHOUT any store write.
///
/// `build_metadata_only_metadata` deliberately persists only the
/// minimal "what the resolver returned" surface (title / authors /
/// id), leaving `year` / `venue` / `publisher` / `type_` as `None`.
/// `cite` needs a complete citation, so when the resolver hit Crossref
/// this overlays the bibliographic fields from the Crossref `message`
/// envelope (`extract_crossref_fields` plus `publisher` / `ISSN`).
///
/// Non-Crossref payloads (arXiv Atom, Unpaywall) keep the metadata-only
/// baseline: their envelopes don't carry these fields in the Crossref
/// shape, and fabricating them would be worse than omitting them — the
/// BibTeX entry is honest about what the source actually provided.
#[must_use]
pub fn cite_metadata(ref_: &Ref, outcome: &MetadataOnlyOutcome) -> Metadata {
    let mut m = build_metadata_only_metadata(ref_, outcome);
    if outcome.source == "crossref" {
        let f = extract_crossref_fields(&outcome.metadata);
        if let Some(title) = f.title {
            m.title = title;
        }
        if !f.authors.is_empty() {
            m.authors = f.authors;
        }
        m.year = f.year;
        m.venue = f.venue;
        m.volume = f.volume;
        m.issue = f.issue;
        m.pages = f.pages;
        m.type_ = f.type_;
        m.publisher = outcome
            .metadata
            .get("publisher")
            .and_then(Value::as_str)
            .map(str::to_string);
        // Crossref `ISSN` is an array; take the first entry.
        m.issn = outcome
            .metadata
            .get("ISSN")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(Value::as_str)
            .map(str::to_string);
    } else if outcome.source == "arxiv" {
        // arXiv Atom overlay (issue #303). The baseline already pulled
        // title/authors; add the publication year (from the Atom
        // `published` timestamp) and the subject categories (primary class
        // first) so `cite` renders a COMPLETE arXiv `@misc` —
        // eprint/archivePrefix/primaryClass/year — instead of the
        // title+author stub that read as an incomplete reference. Honest by
        // construction: every field comes straight from the Atom payload.
        m.year = outcome
            .metadata
            .get("published")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339_year);
        m.arxiv_categories = extract_arxiv_categories(&outcome.metadata);
    }
    m
}

/// Extract the four-digit year from an RFC3339 timestamp — the arXiv Atom
/// `published` field, e.g. `"2004-03-24T00:00:00Z"`. Returns `None` if the
/// value does not parse as RFC3339, so a malformed timestamp simply omits
/// the `year` rather than fabricating one.
fn parse_rfc3339_year(s: &str) -> Option<i32> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| chrono::Datelike::year(&dt))
}

/// The arXiv subject categories (primary first) from an Atom-feed JSON's
/// `categories` array, e.g. `["cond-mat.str-el", "cond-mat.dis-nn"]`.
/// Empty when absent. Shared by `cite_metadata` (live resolve) and
/// `fetch_paper_arxiv` (PDF-fetch path) so both populate
/// `Metadata.arxiv_categories` identically (issue #303).
fn extract_arxiv_categories(atom: &Value) -> Vec<String> {
    atom.get("categories")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Convert a Crossref `page` value to BibTeX page-range form: a single
/// inter-page hyphen (`477-528`) becomes an en-dash (`477--528`). A value
/// that already uses `--`, or a single page with no hyphen, is returned
/// unchanged. This is a generic BibTeX convention, not a port of any
/// external tool's logic.
fn normalize_page_range(page: &str) -> String {
    if page.contains("--") || !page.contains('-') {
        return page.to_string();
    }
    page.replace('-', "--")
}

/// `title` from a resolver payload: a bare string, or the first
/// **non-blank** element of an array (Crossref `message.title` is
/// `[String]`; a leading empty/whitespace element is skipped rather
/// than masking the real title). Trimmed. `None` if absent/blank.
fn extract_metadata_title(meta: &Value) -> Option<String> {
    let t = meta.get("title")?;
    let s = match t.as_str() {
        Some(s) => s.trim().to_string(),
        None => t
            .as_array()?
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .find(|s| !s.is_empty())?
            .to_string(),
    };
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Best-effort author list, tolerant of the resolver shapes we may see:
/// Crossref `author: [{given,family}]`, arXiv `authors: [String]`, and
/// a `z_authors: [{given,family}]` fallback. NOTE: doiget's Unpaywall
/// source deserializes a *partial* `UnpaywallWork` that does not capture
/// `z_authors`, so the `z_authors` branch is currently inert for the
/// Unpaywall path (kept as forward-compat for if/when that struct
/// captures it) — Unpaywall-sourced metadata-only entries get an empty
/// author list. Returns `Vec::new()` when nothing is parseable (a valid
/// metadata TOML — #139 only requires the entry to exist and be
/// readable).
fn extract_metadata_authors(meta: &Value) -> Vec<String> {
    if let Some(arr) = meta.get("authors").and_then(Value::as_array) {
        let v: Vec<String> = arr
            .iter()
            .filter_map(|a| a.as_str().map(str::to_string))
            .collect();
        if !v.is_empty() {
            return v;
        }
    }
    for key in ["author", "z_authors"] {
        if let Some(arr) = meta.get(key).and_then(Value::as_array) {
            let v: Vec<String> = arr
                .iter()
                .filter_map(|a| {
                    let given = a.get("given").and_then(Value::as_str).unwrap_or("");
                    let family = a.get("family").and_then(Value::as_str).unwrap_or("");
                    let name = format!("{given} {family}");
                    let name = name.trim();
                    if name.is_empty() {
                        a.get("name").and_then(Value::as_str).map(str::to_string)
                    } else {
                        Some(name.to_string())
                    }
                })
                .collect();
            if !v.is_empty() {
                return v;
            }
        }
    }
    Vec::new()
}

// ---------------------------------------------------------------------------
// Env-aware source constructors (mirrors doiget-cli::commands::fetch::build_*)
//
// These let MCP integration tests redirect the orchestrator at a
// wiremock origin via `DOIGET_*_BASE` env vars, without inverting the
// `doiget-mcp -> doiget-core` wiring by depending on `doiget-cli`. The
// override surface is identical to the CLI's `fetch.rs::build_*_source`
// helpers so a single test fixture can drive both crates.
// ---------------------------------------------------------------------------

/// `DOIGET_CONTACT_EMAIL`, defaulting to the same `doiget@localhost`
/// the CLI uses (`crates/doiget-cli/src/commands/fetch.rs::OrchestratorConfig`).
const FALLBACK_CONTACT_EMAIL: &str = "doiget@localhost";

fn contact_email_from_env() -> String {
    std::env::var("DOIGET_CONTACT_EMAIL").unwrap_or_else(|_| FALLBACK_CONTACT_EMAIL.to_string())
}

fn arxiv_source_from_env() -> ArxivSource {
    if let Ok(s) = std::env::var("DOIGET_ARXIV_BASE") {
        match url::Url::parse(&s) {
            Ok(url) => return ArxivSource::with_base(url),
            Err(e) => tracing::warn!(
                value = %s,
                error = %e,
                "DOIGET_ARXIV_BASE is not a valid URL; using the default arXiv base"
            ),
        }
    }
    ArxivSource::new()
}

fn crossref_source_from_env(contact: &str) -> CrossrefSource {
    if let Ok(s) = std::env::var("DOIGET_CROSSREF_BASE") {
        match url::Url::parse(&s) {
            Ok(url) => return CrossrefSource::with_base(url, contact.to_string()),
            Err(e) => tracing::warn!(
                value = %s,
                error = %e,
                "DOIGET_CROSSREF_BASE is not a valid URL; using the default Crossref base"
            ),
        }
    }
    CrossrefSource::new(contact.to_string())
}

fn unpaywall_source_from_env(contact: &str) -> UnpaywallSource {
    if let Ok(s) = std::env::var("DOIGET_UNPAYWALL_BASE") {
        match url::Url::parse(&s) {
            Ok(url) => return UnpaywallSource::with_base(url, contact.to_string()),
            Err(e) => tracing::warn!(
                value = %s,
                error = %e,
                "DOIGET_UNPAYWALL_BASE is not a valid URL; using the default Unpaywall base"
            ),
        }
    }
    UnpaywallSource::new(contact.to_string())
}

/// DOI branch — Crossref first, with Unpaywall as a fallback when
/// Crossref fails. Crossref's `message.link[]` array (when present)
/// supplies the OA URL hint without making a publisher request.
async fn metadata_only_doi(
    _doi: &Doi,
    ref_: &Ref,
    profile: &CapabilityProfile,
    ctx: &FetchContext,
) -> Result<MetadataOnlyOutcome, FetchError> {
    let contact = contact_email_from_env();
    let crossref = crossref_source_from_env(&contact);
    match crossref.fetch(ref_, profile, ctx).await {
        Ok(res) => {
            let metadata = res.metadata_json.unwrap_or(Value::Null);
            let oa_url = extract_crossref_oa_url(&metadata);
            // Pure resolver — no store write here (see `metadata_only`
            // doc); persistence is `metadata_only_to_store`'s job.
            Ok(MetadataOnlyOutcome {
                source: crossref.name().to_string(),
                resolver_profile: crossref.name().to_string(),
                // Crossref does not surface a license directly; the
                // license channel for DOI metadata is Unpaywall's
                // `best_oa_location.license`. Leave `None` here; the
                // agent can call `unpaywall` (or a follow-up slice's
                // chained orchestrator) if it needs a license string.
                license: None,
                oa_url,
                // Crossref does not report OA status; "not determined".
                // (The metadata path is Crossref-first and only consults
                // Unpaywall on a Crossref failure — see the fallback arm.)
                oa_status: None,
                metadata,
            })
        }
        Err(crossref_err) => {
            // Crossref failed. Try Unpaywall as a fallback before
            // surfacing the original error.
            let unpaywall = unpaywall_source_from_env(&contact);
            match unpaywall.fetch(ref_, profile, ctx).await {
                Ok(res) => {
                    let metadata = res.metadata_json.unwrap_or(Value::Null);
                    let oa_url = extract_unpaywall_oa_url(&metadata);
                    let oa_status = extract_unpaywall_oa_status(&metadata);
                    let license = if res.license == "unknown" {
                        None
                    } else {
                        Some(res.license)
                    };
                    Ok(MetadataOnlyOutcome {
                        source: unpaywall.name().to_string(),
                        resolver_profile: unpaywall.name().to_string(),
                        license,
                        oa_url,
                        oa_status,
                        metadata,
                    })
                }
                Err(_unpaywall_err) => {
                    // Both sources failed; surface the Crossref error
                    // (the primary path) for diagnosability.
                    Err(crossref_err)
                }
            }
        }
    }
}

/// Defensively pull a Crossref OA URL out of a `message.link[]` entry.
///
/// The Crossref `Link` model documents `link[].URL` as the OA URL string
/// when the work has one (see
/// `<https://api.crossref.org/swagger-ui/index.html>`). Multiple entries
/// may be present; we return the first non-empty `URL` field
/// encountered. Returns `None` if the array is missing, empty, or
/// contains no usable URL string.
fn extract_crossref_oa_url(msg: &Value) -> Option<String> {
    let arr = msg.get("link")?.as_array()?;
    arr.iter()
        .filter_map(|entry| entry.get("URL").and_then(Value::as_str))
        .find(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// Defensively pull Unpaywall's preferred OA URL
/// (`best_oa_location.url_for_pdf`, falling back to `.url`) out of a
/// metadata payload.
fn extract_unpaywall_oa_url(meta: &Value) -> Option<String> {
    let loc = meta.get("best_oa_location")?;
    loc.get("url_for_pdf")
        .and_then(Value::as_str)
        .or_else(|| loc.get("url").and_then(Value::as_str))
        .map(|s| s.to_string())
}

/// Pull Unpaywall's `oa_status` (`gold` / `green` / `hybrid` / `bronze` /
/// `closed`) out of a metadata payload, for OA transparency (#281 item 4).
/// Returns `None` when the field is absent — or an empty string, which is
/// "not determined", never a meaningful status (review #284 advisory).
fn extract_unpaywall_oa_status(meta: &Value) -> Option<String> {
    meta.get("oa_status")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

// ---------------------------------------------------------------------------
// fetch_paper — single-ref orchestrator (Slice 2)
// ---------------------------------------------------------------------------

/// Outcome of a successful [`fetch_paper`] call.
///
/// Wire shape mirrors `docs/MCP_TOOLS.md` §5 `FetchResult` minus the
/// envelope chrome the MCP server wraps it in (`ok: true`, `ref`,
/// optional `error`).
///
/// `path` is the absolute path of the resource the orchestrator wrote to
/// the store. For arXiv refs and successful DOI OA-PDF fetches this is
/// `<root>/<safekey>.pdf`; for the DOI metadata-only fallback (OA URL
/// host off the `oa-publisher` allowlist, or PDF leg failed for another
/// transport reason — `docs/REDIRECT_ALLOWLIST.md` §3 informed-best-
/// effort posture) this is `<root>/.metadata/<safekey>.toml`.
/// Outcome of the DOI OA-PDF leg, carried on [`FetchPaperOutcome`] so a
/// caller can NEVER silently report a blocked PDF as a plain
/// "metadata-only" success (issue #118). The product promise is
/// "immediately explain WHY a paper can't be fetched" — the distinction
/// between "there was no OA PDF to fetch" and "an OA PDF existed but we
/// were blocked, and here is the reason" is exactly that explanation.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum PdfLegStatus {
    /// A PDF was fetched and written to disk (arXiv always; DOI when
    /// the OA-publisher leg succeeded).
    Fetched,
    /// No OA URL was discovered (Unpaywall reported no
    /// `best_oa_location`). Metadata-only is the correct, expected
    /// result here — not a failure.
    NoOaUrl,
    /// An OA URL *was* discovered but the PDF could not be retrieved
    /// (host outside the oa-publisher allowlist, not-a-PDF body,
    /// transport failure, …). Metadata was still written, but the
    /// caller MUST surface this reason rather than pretending the
    /// fetch was a clean metadata-only success.
    Blocked {
        /// Closed-set code, mapped from the underlying transport error
        /// via the canonical `From<FetchError> for ErrorCode`.
        code: crate::ErrorCode,
        /// Human-readable one-line reason (the `FetchError` display).
        message: String,
        /// Structured denial side-channel (ADR-0023) when the failure
        /// was an allowlist / scheme denial; `None` otherwise.
        denial: Option<crate::DenialContext>,
        /// Actionable suggested arXiv ID for the same paper when Unpaywall
        /// metadata includes an arXiv alternative but the PDF leg was blocked.
        suggested_arxiv_id: Option<String>,
    },
    /// The OA publisher PDF was blocked (403, allowlist denial, etc.) but
    /// the `suggested_arxiv_id` pointed to an arXiv preprint that was
    /// successfully fetched and stored under the DOI entry (issue #325).
    /// The stored PDF came from arXiv, not the publisher; callers SHOULD
    /// surface a note so the user knows the file is a preprint.
    PreprintFallback {
        /// The arXiv ID that was successfully fetched as fallback.
        arxiv_id: String,
        /// The OA-publisher error that triggered the fallback (for logs
        /// and audit trail context).
        original_block: String,
    },
    /// The OA chain was blocked and a Tier-3 TDM source served the
    /// publisher's own copy under the user's TDM agreement (#458).
    ///
    /// Distinct from [`Self::Fetched`] on purpose. The bytes did not come
    /// from an OA host, they are not necessarily openly licensed, and the
    /// user obtained them under an agreement they signed — provenance
    /// that says `oa-publisher` here would be wrong in all three
    /// respects.
    TdmFetched {
        /// The Tier-3 source that served it (`"tdm-aps"`, ...). Matches
        /// [`crate::source::Source::name`].
        source: String,
        /// The OA-chain error that triggered the fallback, kept for the
        /// audit trail — the user still needs to know the open route
        /// failed even though the fetch succeeded.
        original_block: String,
    },
}

/// What `fetch_paper` wrote to disk and how.
///
/// `path` is the PDF (`<root>/<safekey>.pdf`) on a successful PDF
/// fetch, or the metadata TOML (`<root>/.metadata/<safekey>.toml`)
/// when the DOI path fell back to metadata-only. [`Self::pdf_leg`]
/// disambiguates *why* there is no PDF (genuinely none available vs.
/// available-but-blocked) so callers never report a blocked PDF as a
/// silent success (issue #118).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct FetchPaperOutcome {
    /// `Source::name()` of the resolver whose payload landed on disk:
    /// `"arxiv"` for an arXiv ref, `"oa-publisher"` when the DOI OA PDF
    /// leg succeeded, or `"crossref"` / `"unpaywall"` when the DOI path
    /// fell back to metadata-only. Mirrors the value written to
    /// `[doiget].source` in the metadata TOML.
    pub source: String,
    /// Resolver profile under which the canonical-digest (ADR-0021 §1)
    /// was minted for the final artifact. For an arXiv fetch this is
    /// `"arxiv"`; for a successful DOI OA PDF leg this is
    /// `"oa-publisher"`; for the DOI metadata-only fallback this is the
    /// metadata source key (`"crossref"` / `"unpaywall"`). Equal to
    /// [`Self::source`] verbatim in Slice 4 but kept distinct so future
    /// slices can decouple "which resolver wrote to disk" from "which
    /// resolver is the audit identity". Surfaced through the
    /// `doiget_fetch_paper` MCP envelope per ADR-0021 §4.
    pub resolver_profile: String,
    /// OA license string (`"CC-BY-4.0"`, `"cc-by"`, `"arxiv-default"`,
    /// `"unknown"`). Mirrors `[doiget].license`.
    pub license: String,
    /// Open-access status (#281 item 4): Unpaywall's `gold` / `green` /
    /// `hybrid` / `bronze` / `closed` for a DOI, or `"green"` for an arXiv
    /// ref. `None` when not determined. Mirrors `[doiget].oa_status`. Lets
    /// a caller distinguish a paywalled work (`closed` + `pdf_leg: NoOaUrl`)
    /// from one that is openly available.
    pub oa_status: Option<String>,
    /// Absolute path of the artifact actually written
    /// (`<root>/<safekey>.pdf` on success, `<root>/.metadata/<safekey>.toml`
    /// on metadata-only fallback).
    pub path: Utf8PathBuf,
    /// Stored PDF size in bytes; `0` on the metadata-only fallback
    /// (`docs/REDIRECT_ALLOWLIST.md` §3.5).
    pub size_bytes: u64,
    /// The schema version of the metadata TOML written
    /// (always [`crate::SCHEMA_VERSION`] for this build).
    pub schema_version: String,
    /// What happened on the PDF leg (issue #118). `Fetched` /
    /// `NoOaUrl` are clean outcomes; `Blocked` carries the structured
    /// reason an OA PDF existed but could not be retrieved, so the
    /// CLI / MCP surface it instead of a silent metadata-only success.
    pub pdf_leg: PdfLegStatus,
    /// Per-ref [`crate::Safekey`] stringified (`Ref::safekey().as_str()`).
    /// Exposed on the outcome so JSON-mode CLI / MCP callers can
    /// emit a structured success body without re-parsing the input
    /// ref (#210 / `docs/ERRORS.md` §3). Always populated.
    pub safekey: String,
    /// ADR-0021 §1 canonical-digest as 64-char lowercase hex for the
    /// resolver_profile that produced this outcome's audit identity.
    /// For an arXiv fetch this is the digest under `"arxiv"`; for a
    /// DOI OA PDF leg this is under `"oa-publisher"`; for the DOI
    /// metadata-only fallback this is under the metadata source key
    /// (`"crossref"` / `"unpaywall"`). Always populated.
    pub canonical_digest: String,
    /// Title of the fetched work, mirrored from the resolved metadata so a
    /// caller can confirm the RIGHT paper landed in one call (#344). A ref-id
    /// placeholder when the resolver supplied no title.
    pub title: String,
    /// Authors of the fetched work (empty when the resolver supplied none).
    pub authors: Vec<String>,
    /// Publication year, when known (#344 identity confirmation).
    pub year: Option<i32>,
    /// One [`SourceAttempt`] per optional source, consulted or not (#445).
    ///
    /// #413 attached this trace to `NotFound` only, so the question it
    /// exists to answer — *did anything else have this paper?* — went
    /// unanswered on the outcome where a user is most likely to ask it: an
    /// OA copy was located at a host that then refused to serve it. "Found
    /// nowhere" and "found at one host that refused me" have the same next
    /// step, so they get the same trace.
    ///
    /// Empty for an arXiv ref, which has no optional chain.
    pub attempts: Vec<SourceAttempt>,
}

impl FetchPaperOutcome {
    /// Test-only constructor for downstream crates (`doiget-cli`,
    /// `doiget-mcp`) that need to drive classification / rendering
    /// logic without running the full orchestrator. Produces a
    /// minimal but structurally-valid outcome — all required fields
    /// populated with defensible stubs — so unit tests can assert
    /// the surrounding behavior (JSONL shape, exit-code mapping,
    /// PDF-leg branching) in isolation.
    ///
    /// `#[doc(hidden)]` because this is not a stable public API; the
    /// signature may change to fit test needs without a CHANGELOG
    /// `[BREAKING]` callout.
    #[doc(hidden)]
    pub fn for_test_synthetic(
        safekey: impl Into<String>,
        source: impl Into<String>,
        pdf_leg: PdfLegStatus,
    ) -> Self {
        let safekey: String = safekey.into();
        let source: String = source.into();
        Self {
            source: source.clone(),
            resolver_profile: source.clone(),
            license: "unknown".to_string(),
            oa_status: None,
            path: Utf8PathBuf::from(format!("/tmp/{safekey}.pdf")),
            size_bytes: 0,
            schema_version: SCHEMA_VERSION.to_string(),
            pdf_leg,
            safekey: safekey.clone(),
            // 32 bytes of `0x00` → a stable, non-secret digest stub
            // that's still 64 chars of lowercase hex.
            canonical_digest: "00".repeat(32),
            title: String::new(),
            authors: Vec::new(),
            year: None,
            attempts: Vec::new(),
        }
    }

    /// [`Self::for_test_synthetic`] carrying a resolution trace.
    ///
    /// #471: the plain constructor hard-codes `attempts: Vec::new()`, so a
    /// test driving `classify_joined` with a `Blocked` outcome could not
    /// observe a trace even if it looked -- and none did. Reverting the one
    /// line that threads `outcome.attempts` into `build_jsonl_failure` left
    /// the whole suite green, silently dropping the trace from `--json`,
    /// which is the regression #459 exists to prevent.
    ///
    /// `#[doc(hidden)]` for the same reason as its sibling: not a stable
    /// public API.
    #[doc(hidden)]
    pub fn for_test_synthetic_with_attempts(
        safekey: impl Into<String>,
        source: impl Into<String>,
        pdf_leg: PdfLegStatus,
        attempts: Vec<SourceAttempt>,
    ) -> Self {
        Self {
            attempts,
            ..Self::for_test_synthetic(safekey, source, pdf_leg)
        }
    }
}

/// Resolve a [`Ref`] to a PDF (or metadata-only fallback) and write it
/// through `store`.
///
/// Binding spec: `docs/MCP_TOOLS.md` §4 (`doiget_fetch_paper`),
/// `docs/REDIRECT_ALLOWLIST.md` §3 (informed-best-effort posture for the
/// DOI OA PDF leg), `docs/PROVENANCE_LOG.md` §3 (per-attempt `Fetch` rows
/// emitted by the source impls; `StoreWrite` row emitted by this
/// orchestrator).
///
/// # Dispatch
///
/// - `Ref::Arxiv(_)` → [`ArxivSource::fetch`]; the source returns PDF
///   bytes + Atom-feed metadata. The orchestrator writes both the PDF
///   and the metadata TOML.
/// - `Ref::Doi(_)` → Crossref metadata + Unpaywall license/OA-URL
///   enrichment + (when the OA URL host is on the `oa-publisher`
///   allowlist) a publisher PDF leg. A failure on the PDF leg is
///   non-fatal: the metadata is still written and the orchestrator
///   returns `Ok(...)` with `source` set to the metadata source.
///
/// # Side effects
///
/// Each consulted source emits one `LogEvent::Fetch` row via
/// `ctx.log.append`. The orchestrator additionally emits one
/// `LogEvent::StoreWrite` row on the successful write. Session bookend
/// rows are the caller's responsibility (the CLI's
/// `commands::fetch::run_with_options` wraps the call; the MCP server's
/// `doiget_fetch_paper` tool method wraps it too).
///
/// # Errors
///
/// Returns [`FetchError`] from the underlying [`Source`] dispatch. The
/// MCP boundary converts these to the closed [`crate::ErrorCode`] set
/// via the existing `From<FetchError> for ErrorCode` impl.
pub async fn fetch_paper(
    ref_: &Ref,
    profile: &CapabilityProfile,
    ctx: &FetchContext,
    store: &dyn Store,
    store_root: &Utf8Path,
) -> Result<FetchPaperOutcome, FetchError> {
    let safekey = ref_.safekey();
    match ref_ {
        Ref::Arxiv(id) => {
            fetch_paper_arxiv(id, ref_, profile, ctx, store, store_root, &safekey).await
        }
        Ref::Doi(doi) => {
            fetch_paper_doi(doi, ref_, profile, ctx, store, store_root, &safekey).await
        }
    }
}

/// Build the dry-run preview ([`FetchPlan`]) for a single ref without
/// touching the network, store, or provenance log. Thin re-export of
/// [`crate::dry_run::build_fetch_plan`] under the slice-2 naming the
/// MCP tool surfaces use; kept here so the MCP `doiget_fetch_paper`
/// tool method does not have to reach across two modules.
pub fn fetch_paper_plan(ref_: &Ref, store_root: &Utf8Path) -> FetchPlan {
    build_fetch_plan(ref_, store_root)
}

/// Fallible sibling of [`fetch_paper_plan`] — propagates an internal
/// allowlist-contract drift as a typed [`FetchError::SourceSchema`]
/// instead of degrading to an empty `candidate_hosts` list (issue
/// #156 ②). Thin re-export of [`crate::dry_run::try_build_fetch_plan`].
/// Added alongside the infallible [`fetch_paper_plan`] rather than
/// changing its signature, because `fetch_paper_plan` is `pub` and
/// called from `doiget-mcp`, which is out of scope for this batch.
///
/// # Errors
///
/// See [`crate::dry_run::try_build_fetch_plan`].
pub fn try_fetch_paper_plan(ref_: &Ref, store_root: &Utf8Path) -> Result<FetchPlan, FetchError> {
    try_build_fetch_plan(ref_, store_root)
}

/// arXiv branch of [`fetch_paper`]. Internal — public callers go
/// through `fetch_paper`.
async fn fetch_paper_arxiv(
    id: &ArxivId,
    ref_: &Ref,
    profile: &CapabilityProfile,
    ctx: &FetchContext,
    store: &dyn Store,
    store_root: &Utf8Path,
    safekey: &Safekey,
) -> Result<FetchPaperOutcome, FetchError> {
    let source = arxiv_source_from_env();
    if !source.can_serve(profile, ref_) {
        return Err(FetchError::NotEligible {
            source_key: source.name().to_string(),
        });
    }

    let FetchResult {
        license,
        pdf_bytes,
        final_url,
        metadata_json,
        ..
    } = source.fetch(ref_, profile, ctx).await?;
    let pdf = pdf_bytes.ok_or_else(|| FetchError::SourceSchema {
        hint: "arxiv source returned no PDF bytes".to_string(),
    })?;
    let size_bytes = pdf.len() as u64;

    // Real bibliographic metadata from the Atom feed that `fetch` already
    // retrieved (issue #303). The Atom leg is best-effort — `metadata_json`
    // is `None` when the feed fetch failed — so an absent/empty title falls
    // back to the id placeholder, guaranteeing a successful PDF fetch always
    // stores a VALID entry. `bib` / `info` on the result now show the real
    // title / authors / year / categories instead of `arxiv:<id>`.
    let (title, authors, year, arxiv_categories) = match &metadata_json {
        Some(atom) => (
            extract_metadata_title(atom).unwrap_or_else(|| format!("arxiv:{}", id.as_str())),
            extract_metadata_authors(atom),
            atom.get("published")
                .and_then(Value::as_str)
                .and_then(parse_rfc3339_year),
            extract_arxiv_categories(atom),
        ),
        None => (
            format!("arxiv:{}", id.as_str()),
            Vec::new(),
            None,
            Vec::new(),
        ),
    };

    let metadata = Metadata {
        schema_version: SCHEMA_VERSION.to_string(),
        title,
        authors,
        year,
        doi: None,
        arxiv_id: Some(id.clone()),
        arxiv_categories,
        abstract_: None,
        venue: None,
        volume: None,
        issue: None,
        pages: None,
        publisher: None,
        issn: None,
        isbn: None,
        type_: None,
        keywords: Vec::new(),
        url: final_url.as_ref().map(|u| u.to_string()),
        pdf_path: Some(format!("{}.pdf", safekey.as_str())),
        doiget: Some(DoigetExtension {
            fetched_at: Utc::now(),
            source: "arxiv".to_string(),
            license: license.clone(),
            // arXiv preprints are green OA by definition.
            oa_status: Some("green".to_string()),
            size_bytes,
            mcp_call_id: None,
            tags: Vec::new(),
            collections: Vec::new(),
            annotation: None,
        }),
        other: BTreeMap::new(),
    };

    let tmp = stage_pdf_to_tempfile(&pdf)?;
    let pdf_src = Utf8Path::from_path(tmp.path())
        .ok_or_else(|| FetchError::SourceSchema {
            hint: "staging tempfile path is not UTF-8".to_string(),
        })?
        .to_path_buf();
    write_metadata_and_pdf(store, safekey, &metadata, Some(&pdf_src), ctx)?;
    drop(tmp);

    let path = store_root.join(format!("{}.pdf", safekey.as_str()));
    let canonical_digest =
        crate::CanonicalRef::new(crate::SourceType::Arxiv, id.as_str(), "arxiv", None).digest_hex();
    Ok(FetchPaperOutcome {
        source: "arxiv".to_string(),
        resolver_profile: "arxiv".to_string(),
        license,
        oa_status: Some("green".to_string()),
        path,
        size_bytes,
        schema_version: SCHEMA_VERSION.to_string(),
        // arXiv always delivers the PDF (or the whole fn already
        // returned Err above) — there is no metadata-only fallback.
        pdf_leg: PdfLegStatus::Fetched,
        safekey: safekey.as_str().to_string(),
        canonical_digest,
        title: metadata.title.clone(),
        authors: metadata.authors.clone(),
        year: metadata.year,
        // arXiv resolves directly; the optional chain is a DOI concept.
        attempts: Vec::new(),
    })
}

/// DOI branch of [`fetch_paper`] — Crossref + Unpaywall + (when allowed)
/// OA-publisher PDF leg. Mirrors the CLI's `fetch_doi` implementation
/// (`crates/doiget-cli/src/commands/fetch.rs`) — the CLI now delegates
/// here so both surfaces share one source of truth.
async fn fetch_paper_doi(
    doi: &Doi,
    ref_: &Ref,
    profile: &CapabilityProfile,
    ctx: &FetchContext,
    store: &dyn Store,
    store_root: &Utf8Path,
    safekey: &Safekey,
) -> Result<FetchPaperOutcome, FetchError> {
    let contact = contact_email_from_env();
    let unpaywall_contact = unpaywall_email_from_env(&contact);
    let crossref = crossref_source_from_env(&contact);
    // Issue #120: Crossref is NON-fatal. A transient Crossref failure
    // must not abort the whole DOI fetch when Unpaywall alone can
    // still deliver the OA PDF. We keep the error and only surface it
    // if nothing usable comes back (see the both-failed guard below).
    let (cross, crossref_err) = match crossref.fetch(ref_, profile, ctx).await {
        Ok(r) => (Some(r), None),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "crossref fetch failed; continuing with unpaywall-only metadata + OA leg"
            );
            (None, Some(e))
        }
    };
    let crossref_meta = cross
        .as_ref()
        .and_then(|c| c.metadata_json.clone())
        .unwrap_or(Value::Null);
    // `mut` only in `metadata` builds, where the DataCite fallback below may
    // reassign it; without that feature nothing writes to it.
    #[allow(unused_mut)]
    let mut extracted = extract_crossref_fields(&crossref_meta);

    // #413 / ADR-0040: the optional resolution chain, strictly AFTER
    // Crossref and consulted only when Crossref produced nothing. That
    // ordering is what makes every source additive — a Crossref-registered
    // DOI never reaches them, so enabling a flag cannot change a
    // resolution that already works.
    //
    // Every source records a `SourceAttempt`, including the ones that were
    // NOT consulted. That is the point: before this, a failed DOI fetch
    // returned the Crossref error and said nothing about the rest of the
    // chain, so "HAL was asked and had nothing" and "HAL was never asked
    // because the flag is unset" were the same observable. They are
    // different problems with different fixes.
    #[allow(unused_mut)]
    let mut attempts: Vec<SourceAttempt> = Vec::new();

    // Tier 3 before Tier 2 (#442). For a DOI its publisher registered,
    // the publisher's own API is the authoritative record — and it is the
    // one the user went and got credentials for. Prefix-scoped, so an
    // enabled TDM source is only ever told about DOIs its publisher
    // already knows it issued.
    // Every `tdm-*` feature must be named here. #457 added `tdm-ieee`
    // to the chain body and not to these two gates, so a build with
    // `--features tdm-ieee` alone compiled the source, the allowlist and
    // the capability grant, and then `#[cfg]`-ed away the only code that
    // calls any of it. Fourth instance of #442's shape.
    #[cfg(any(
        feature = "tdm-elsevier",
        feature = "tdm-aps",
        feature = "tdm-springer",
        feature = "tdm-ieee"
    ))]
    let tdm_meta = resolve_tdm_chain(ref_, profile, ctx, cross.is_some(), &mut attempts).await;
    // The `not(...)` half has to name the same four features as the
    // `any(...)` above, or a build with only the omitted one compiles
    // BOTH arms: the real call, then this shadow over it. That is what
    // `--features tdm-ieee` did — `unused variable: tdm_meta`, and the
    // chain's result silently discarded.
    #[cfg(not(any(
        feature = "tdm-elsevier",
        feature = "tdm-aps",
        feature = "tdm-springer",
        feature = "tdm-ieee"
    )))]
    let tdm_meta: Option<Value> = None;

    // The resolved pair is KEPT, not mapped away. When Crossref failed this
    // pass ran for real, and the OA fallback below needs its payload — the
    // old code discarded it and re-ran the whole chain to get it back,
    // paying a second live round against five APIs (#468 review).
    #[cfg(feature = "metadata")]
    let optional_resolved = resolve_optional_chain(
        ref_,
        profile,
        ctx,
        cross.is_some() || tdm_meta.is_some(),
        &mut extracted,
        &mut attempts,
    )
    .await;
    #[cfg(feature = "metadata")]
    let optional_meta = optional_resolved
        .as_ref()
        .map(|(_, m)| m.clone())
        .or(tdm_meta);
    #[cfg(not(feature = "metadata"))]
    let optional_meta: Option<Value> = tdm_meta;
    let _ = &optional_meta;

    // Unpaywall second — license enrichment + OA URL chain discovery.
    // A failure here is non-fatal: we still write the Crossref-
    // derived metadata.
    let unpaywall = unpaywall_source_from_env(&unpaywall_contact);
    let upw_result = unpaywall.fetch(ref_, profile, ctx).await;
    let (mut license, source_label, oa_chain, oa_status) = match upw_result {
        Ok(r) => {
            let chain = extract_oa_url_chain(r.metadata_json.as_ref());
            // OA status describes the WORK (gold/green/closed/…), not the
            // fetch — surfaced even when the PDF leg is later blocked, so an
            // agent can tell "paywalled" from "we couldn't reach it" (#281
            // item 4).
            let oa_status = r
                .metadata_json
                .as_ref()
                .and_then(extract_unpaywall_oa_status);
            let label = if r.license != "unknown" {
                "unpaywall".to_string()
            } else {
                "crossref".to_string()
            };
            (r.license, label, chain, oa_status)
        }
        Err(e) => {
            // Unpaywall unreachable / errored. We continue with the
            // Crossref-only metadata, but the resulting empty OA
            // chain will be reported downstream as
            // `PdfLegStatus::NoOaUrl` — semantically distinct from
            // "Unpaywall confirmed no OA URL". The provenance log
            // already carries an Unpaywall Fetch err row (the
            // Unpaywall source impl logged its own attempt before
            // returning), so the audit trail captures the cause; the
            // tracing line below makes the orchestrator-level signal
            // loud as well. Surfacing the distinction at the
            // `PdfLegStatus` level (a new variant like
            // `MetadataSourceUnavailable`) is a deliberate
            // follow-up — see CHANGELOG `[0.4.0]` Notes.
            tracing::warn!(
                error = %e,
                doi = %doi.as_str(),
                "unpaywall fetch failed; OA chain will be empty (downstream PdfLegStatus::NoOaUrl \
                 is conservative — Unpaywall was unreachable, not authoritatively oa-free)"
            );
            (
                "unknown".to_string(),
                "crossref".to_string(),
                Vec::new(),
                None,
            )
        }
    };

    // OA PDF leg — ADR-0029 fetch chain. Walk the candidate URL list
    // in order; first successful PDF wins, all-failed surfaces as
    // `PdfLegStatus::Blocked` with the LAST attempt's error (the most
    // informative for the operator — typically the network /
    // allowlist reason the chain could not be exhausted). Each
    // `try_fetch_oa_pdf` call already emits its own per-attempt
    // provenance row (`oa-publisher` Fetch ok / err), so the audit
    // trail captures every external request without orchestrator-
    // side bookkeeping.
    //
    // Issue #118: a failure here is NEVER silently turned into a
    // clean metadata-only success — the structured reason is carried
    // out on `PdfLegStatus::Blocked`.
    let (pdf_leg, pdf_bytes) = if oa_chain.is_empty() {
        (PdfLegStatus::NoOaUrl, None)
    } else {
        let mut succeeded: Option<Vec<u8>> = None;
        let mut last_err: Option<HttpError> = None;
        let total = oa_chain.len();
        for (idx, candidate) in oa_chain.iter().enumerate() {
            let attempt = idx + 1;
            tracing::debug!(
                attempt,
                total,
                url = %candidate,
                "trying OA PDF candidate (ADR-0029 chain)"
            );
            match try_fetch_oa_pdf(doi, candidate, ctx).await {
                Ok((bytes, _final_url)) => {
                    if attempt > 1 {
                        tracing::info!(
                            attempt,
                            total,
                            url = %candidate,
                            "OA PDF chain succeeded on fallback candidate (ADR-0029)"
                        );
                    }
                    succeeded = Some(bytes);
                    break;
                }
                Err(e) => {
                    tracing::warn!(
                        attempt,
                        total,
                        url = %candidate,
                        error = %e,
                        "OA PDF candidate failed; advancing to next (ADR-0029 chain)"
                    );
                    last_err = Some(e);
                }
            }
        }
        match (succeeded, last_err) {
            (Some(bytes), _) => (PdfLegStatus::Fetched, Some(bytes)),
            (None, Some(e)) => {
                let fe = FetchError::Http(e);
                let denial: Option<crate::DenialContext> = (&fe).into();
                let message = fe.to_string();
                let code: crate::ErrorCode = fe.into();
                let suggested_arxiv_id = oa_chain.iter().find_map(extract_arxiv_id_from_url);
                (
                    PdfLegStatus::Blocked {
                        code,
                        message,
                        denial,
                        suggested_arxiv_id,
                    },
                    None,
                )
            }
            // Defensive fallback. `oa_chain` is non-empty in this
            // branch, so structurally at least one iteration must set
            // either `succeeded` or `last_err`. If a future refactor
            // breaks the invariant we fail CLOSED — surface a
            // `Blocked` outcome with a self-describing message
            // rather than `NoOaUrl` (which would falsely tell the
            // caller no candidate URL was ever discovered). Routes
            // to `INTERNAL_ERROR` so the CLI's exit-code mapping
            // signals a doiget bug, not a remote failure.
            (None, None) => {
                tracing::error!(
                    total = oa_chain.len(),
                    "OA PDF chain walker exhausted without recording success or error \
                     (defensive fallback — should be unreachable)"
                );
                (
                    PdfLegStatus::Blocked {
                        code: crate::ErrorCode::InternalError,
                        message:
                            "OA PDF chain walker exhausted without recording success or error \
                             (orchestrator bug — please report)"
                                .to_string(),
                        denial: None,
                        suggested_arxiv_id: None,
                    },
                    None,
                )
            }
        }
    };

    // Issue #325: auto preprint fallback. If the OA chain was blocked but
    // Unpaywall hinted at an arXiv preprint, attempt that fetch and store
    // it under the DOI safekey instead of returning Blocked.
    let (pdf_leg, pdf_bytes, arxiv_id_for_metadata, fallback_license) =
        try_arxiv_preprint_fallback(doi, pdf_leg, pdf_bytes, profile, ctx).await;

    // #445: and if that did not help either, ask whoever else is switched
    // on. Additive by construction — see the fn docs.
    #[cfg(feature = "metadata")]
    let (pdf_leg, pdf_bytes) = try_optional_source_oa_fallback(
        doi,
        pdf_leg,
        pdf_bytes,
        profile,
        ctx,
        &mut attempts,
        optional_resolved.as_ref().map(|(n, m)| (*n, m)),
    )
    .await;

    // #458: and if even that found nothing, ask the publisher itself.
    //
    // This is the second Tier-3 consultation point, and the one that
    // matters. `resolve_tdm_chain` above runs when *Crossref* missed,
    // which is a question about metadata. The gap a TDM agreement is
    // obtained to close is about bytes, and it opens here -- after
    // Crossref answered perfectly well and the content leg still failed.
    #[cfg(any(
        feature = "tdm-elsevier",
        feature = "tdm-aps",
        feature = "tdm-springer",
        feature = "tdm-ieee"
    ))]
    let (pdf_leg, pdf_bytes) =
        try_tdm_content_fallback(doi, pdf_leg, pdf_bytes, profile, ctx, &mut attempts).await;

    if let Some(fl) = fallback_license {
        license = fl;
    }

    // #458: the licence tracks the artifact that actually landed, not the
    // work. That is already how the arXiv fallback behaves -- it overwrites
    // `license` with the preprint's, because a CC-BY record about the
    // published version says nothing about the file on disk.
    //
    // A TDM copy came from the publisher under an agreement the user
    // signed, by a route the OA licence does not describe; carrying
    // Unpaywall's `cc-by` forward would put an open-licence claim on it.
    // doiget does not guess licences (`docs/SOURCES.md` -- Tier-2 sources
    // report `unknown` rather than infer), and the APS record's licence
    // field describes the article, not this retrieval. So: `unknown`.
    #[cfg(any(
        feature = "tdm-elsevier",
        feature = "tdm-aps",
        feature = "tdm-springer",
        feature = "tdm-ieee"
    ))]
    if matches!(pdf_leg, PdfLegStatus::TdmFetched { .. }) {
        license = "unknown".to_string();
    }

    // Issue #120: Crossref is non-fatal, but if it failed AND the OA
    // PDF leg produced nothing, writing a DOI-only stub entry would
    // mask a total failure and violate the "explain why" promise.
    // Surface the Crossref error so the caller reports a real reason.
    if let Some(e) = crossref_err {
        if pdf_bytes.is_none() {
            // #413: attach the resolution trace. Returning the bare
            // Crossref error was the whole problem — it said nothing about
            // whether the optional chain had been consulted and come up
            // empty, or had never run because its flags are unset. Those
            // need different fixes, so the message has to tell them apart.
            if !attempts.is_empty() {
                let trace = render_attempts(&attempts);
                let lead = if nothing_was_consulted(&attempts) {
                    "no optional source was consulted for this DOI"
                } else {
                    "the optional sources were consulted and did not resolve it"
                };
                return Err(FetchError::NotFound {
                    hint: format!(
                        "{e}
  = note: {lead}:
{trace}"
                    ),
                });
            }
            return Err(e);
        }
    }

    let (final_source_label, size_bytes, pdf_path_relative, pdf_staged) = match &pdf_bytes {
        Some(bytes) => {
            let staged = stage_pdf_to_tempfile(bytes)?;
            // Derived from the leg rather than from a boolean: #458 added a
            // third way to end up holding bytes, and a two-valued flag
            // would have quietly labelled the publisher's TDM copy
            // `oa-publisher`.
            let label = match &pdf_leg {
                PdfLegStatus::PreprintFallback { .. } => "arxiv".to_string(),
                PdfLegStatus::TdmFetched { source, .. } => source.clone(),
                _ => "oa-publisher".to_string(),
            };
            (
                label,
                bytes.len() as u64,
                Some(format!("{}.pdf", safekey.as_str())),
                Some(staged),
            )
        }
        None => (source_label, 0u64, None, None),
    };

    let metadata = Metadata {
        schema_version: SCHEMA_VERSION.to_string(),
        title: extracted.title.unwrap_or_else(|| doi.as_str().to_string()),
        authors: extracted.authors,
        year: extracted.year,
        doi: Some(doi.clone()),
        arxiv_id: arxiv_id_for_metadata,
        // DOI-fetch path: no arXiv id, so no arXiv categories.
        arxiv_categories: Vec::new(),
        abstract_: None,
        venue: extracted.venue,
        volume: extracted.volume,
        issue: extracted.issue,
        pages: extracted.pages,
        publisher: None,
        issn: None,
        isbn: None,
        type_: extracted.type_,
        keywords: Vec::new(),
        url: cross
            .as_ref()
            .and_then(|c| c.final_url.as_ref())
            .map(|u| u.to_string()),
        pdf_path: pdf_path_relative,
        doiget: Some(DoigetExtension {
            fetched_at: Utc::now(),
            source: final_source_label.clone(),
            license: license.clone(),
            oa_status: oa_status.clone(),
            size_bytes,
            mcp_call_id: None,
            tags: Vec::new(),
            collections: Vec::new(),
            annotation: None,
        }),
        other: BTreeMap::new(),
    };

    let pdf_src_path = pdf_staged
        .as_ref()
        .and_then(|tmp| Utf8Path::from_path(tmp.path()).map(|p| p.to_path_buf()));
    write_metadata_and_pdf(store, safekey, &metadata, pdf_src_path.as_deref(), ctx)?;
    drop(pdf_staged);

    let path = if pdf_bytes.is_some() {
        store_root.join(format!("{}.pdf", safekey.as_str()))
    } else {
        store_root
            .join(".metadata")
            .join(format!("{}.toml", safekey.as_str()))
    };
    let canonical_digest = crate::CanonicalRef::new(
        crate::SourceType::Doi,
        doi.as_str(),
        &final_source_label,
        None,
    )
    .digest_hex();
    Ok(FetchPaperOutcome {
        source: final_source_label.clone(),
        resolver_profile: final_source_label,
        license,
        oa_status,
        path,
        size_bytes,
        schema_version: SCHEMA_VERSION.to_string(),
        pdf_leg,
        safekey: safekey.as_str().to_string(),
        canonical_digest,
        title: metadata.title.clone(),
        authors: metadata.authors.clone(),
        year: metadata.year,
        attempts,
    })
}

/// Which OA content URL an optional source reported, if any (#445).
///
/// Only the three sources that publish a direct document URL contribute.
/// OpenAIRE's `urls[]` and DataCite's `url` point at a DOI resolver or a
/// landing page, not a file — handing those to the OA chain would spend a
/// request to arrive at a page the chain cannot read, and report a
/// confusing failure. A source that contributes nothing is not a silent
/// gap: it still appears in the attempt trace with its own outcome.
#[cfg(feature = "metadata")]
fn optional_source_oa_url<'a>(source: &str, meta: &'a Value) -> Option<&'a str> {
    match source {
        "core" => crate::sources::core_oa::open_access_pdf_url(meta),
        "hal" => crate::sources::hal::open_access_pdf_url(meta),
        "europe-pmc" => crate::sources::europepmc::open_access_pdf_url(meta),
        _ => None,
    }
}

/// Last resort for a blocked content leg: ask the enabled optional sources
/// whether anyone else holds a copy (#445).
///
/// The OA chain already tries every location Unpaywall returned, advancing
/// past each failure — so a 429 does not stop it. What stopped the reported
/// run is that the candidate list can only ever contain Unpaywall's
/// locations. Crossref had answered, so the optional chain was skipped
/// entirely, and a rate limit on the single AMS URL ended a run with four
/// other indexes switched on.
///
/// Each of those sources already surfaces a document URL and each module
/// says, in its own docs, that the fetch belongs to the `oa-publisher`
/// leg — `europepmc::open_access_pdf_url` was written for exactly this and
/// had no caller. This is that caller.
///
/// Deliberately last, after the #325 arXiv fallback, so the change is
/// purely additive: every run that succeeded before still succeeds by the
/// same route. It costs a request only when the content leg has ALREADY
/// failed and the user has switched a source on, so a default build with
/// no flags set is byte-identical.
#[cfg(feature = "metadata")]
async fn try_optional_source_oa_fallback(
    doi: &Doi,
    pdf_leg: PdfLegStatus,
    pdf_bytes: Option<Vec<u8>>,
    profile: &CapabilityProfile,
    ctx: &FetchContext,
    attempts: &mut Vec<SourceAttempt>,
    already_resolved: Option<(&'static str, &Value)>,
) -> (PdfLegStatus, Option<Vec<u8>>) {
    if pdf_bytes.is_some() || !matches!(pdf_leg, PdfLegStatus::Blocked { .. }) {
        return (pdf_leg, pdf_bytes);
    }

    // Two cases, and conflating them was the bug the #468 review caught.
    //
    // An earlier comment here claimed "reaching this point means Crossref
    // answered, because a Crossref failure returns NotFound further up".
    // That is false: the NotFound short-circuit is further DOWN, after this
    // call. `pdf_leg` comes from the Unpaywall-derived OA chain, which is
    // independent of Crossref (#120 — Crossref is non-fatal), so this runs
    // in both cases.
    //
    // Crossref FAILED: the chain at the top of `fetch_paper_doi` already ran
    // for real and `attempts` holds its genuine outcomes. Re-running cost a
    // second live round against five third-party APIs and then overwrote
    // those outcomes with a second, possibly different answer. Reuse what it
    // resolved instead.
    //
    // Crossref ANSWERED: the chain short-circuited, so every Tier-2 row is
    // `NotNeeded` and carries nothing. Run it for real — but swap out only
    // those rows. The old `*attempts = fresh` replaced the WHOLE vector,
    // deleting the Tier-3 rows `resolve_tdm_chain` had recorded. In a
    // `metadata` + `tdm-*` build that erased the answer to "was tdm-ieee
    // consulted?" from the trace, from the MCP envelope and from
    // `batch --json` — the exact question #413/#445 added the trace to
    // answer.
    let (name, meta) = match already_resolved {
        Some((n, m)) => (n, m.clone()),
        None => {
            let ref_ = Ref::Doi(doi.clone());
            let mut discard = CrossrefFields::default();
            let mut fresh: Vec<SourceAttempt> = Vec::new();
            let r =
                resolve_optional_chain(&ref_, profile, ctx, false, &mut discard, &mut fresh).await;
            if !fresh.is_empty() {
                attempts.retain(|a| !fresh.iter().any(|f| f.source == a.source));
                attempts.extend(fresh);
            }
            match r {
                Some((n, m)) => (n, m),
                None => return (pdf_leg, pdf_bytes),
            }
        }
    };
    let Some(raw) = optional_source_oa_url(name, &meta) else {
        tracing::debug!(
            source = name,
            doi = %doi.as_str(),
            "optional source resolved but reported no document URL"
        );
        return (pdf_leg, pdf_bytes);
    };
    let Ok(url) = url::Url::parse(raw) else {
        tracing::warn!(
            source = name,
            url = raw,
            "optional source reported an unparsable document URL; keeping Blocked"
        );
        return (pdf_leg, pdf_bytes);
    };

    tracing::info!(
        source = name,
        doi = %doi.as_str(),
        url = %url,
        "OA chain exhausted; trying a copy reported by an optional source (#445)"
    );
    match try_fetch_oa_pdf(doi, &url, ctx).await {
        Ok((bytes, _final_url)) => (PdfLegStatus::Fetched, Some(bytes)),
        Err(e) => {
            // Keep the ORIGINAL block. The publisher's 429 is what the user
            // needs to see; a second failure from a repository would bury it.
            tracing::warn!(
                source = name,
                error = %e,
                "optional-source copy also failed; keeping the original block"
            );
            (pdf_leg, pdf_bytes)
        }
    }
}

/// #458: the publisher's own copy, once the open routes are exhausted.
///
/// Triggered by a blocked *content* leg -- the trigger #445 already built
/// for the optional OA chain, and the one Tier 3 always needed.
/// `resolve_tdm_chain` fires on a Crossref miss instead, so for the DOIs
/// these sources exist to serve -- ones Crossref resolves readily -- it
/// recorded `NotNeeded` for every entry and no request ever went out.
///
/// Additive, like its two siblings: on any failure the ORIGINAL block is
/// kept. The publisher's refusal on the open route is what the user has to
/// act on; burying it under a second failure from the TDM endpoint would
/// answer a question they did not ask.
///
/// Disclosure stays bounded exactly as ADR-0041 bounds it -- a source is
/// only ever told about DOIs its own publisher registered. What rises is
/// the frequency of consultation, not its scope.
#[cfg(any(
    feature = "tdm-elsevier",
    feature = "tdm-aps",
    feature = "tdm-springer",
    feature = "tdm-ieee"
))]
#[allow(clippy::vec_init_then_push)]
async fn try_tdm_content_fallback(
    doi: &Doi,
    pdf_leg: PdfLegStatus,
    pdf_bytes: Option<Vec<u8>>,
    profile: &CapabilityProfile,
    ctx: &FetchContext,
    attempts: &mut Vec<SourceAttempt>,
) -> (PdfLegStatus, Option<Vec<u8>>) {
    struct ContentEntry<'a> {
        name: &'static str,
        /// What the user must set, rendered verbatim into the trace.
        enable_hint: &'static [&'static str],
        /// DOI prefixes this publisher registered (ADR-0041).
        prefixes: &'static [&'static str],
        /// Human name, for the wrong-publisher message.
        publisher: &'static str,
        src: &'a dyn crate::source::Source,
    }

    if pdf_bytes.is_some() {
        return (pdf_leg, pdf_bytes);
    }
    let PdfLegStatus::Blocked {
        message: ref blocked_message,
        ..
    } = pdf_leg
    else {
        return (pdf_leg, pdf_bytes);
    };
    let original_block = blocked_message.clone();

    let ref_ = Ref::Doi(doi.clone());

    #[cfg(feature = "tdm-aps")]
    let aps = optional_base("DOIGET_APS_BASE").map_or_else(
        crate::sources::tdm_aps::TdmApsSource::new,
        crate::sources::tdm_aps::TdmApsSource::with_base,
    );
    #[cfg(feature = "tdm-elsevier")]
    let elsevier = optional_base("DOIGET_ELSEVIER_BASE").map_or_else(
        crate::sources::tdm_elsevier::TdmElsevierSource::new,
        crate::sources::tdm_elsevier::TdmElsevierSource::with_base,
    );
    #[cfg(feature = "tdm-springer")]
    let springer = optional_base("DOIGET_SPRINGER_BASE").map_or_else(
        crate::sources::tdm_springer::TdmSpringerSource::new,
        crate::sources::tdm_springer::TdmSpringerSource::with_base,
    );
    #[cfg(feature = "tdm-ieee")]
    let ieee = optional_base("DOIGET_IEEE_BASE").map_or_else(
        crate::sources::tdm_ieee::TdmIeeeSource::new,
        crate::sources::tdm_ieee::TdmIeeeSource::with_base,
    );

    // Not a `vec![]` literal: each entry is `#[cfg]`-gated on its own
    // publisher feature, and attribute-per-element inside a vec literal is
    // not expressible. Same shape as `resolve_tdm_chain`; the
    // `vec_init_then_push` allow is on the fn because the lint's span runs
    // from the `let` across every push, so a statement-level attribute does
    // not cover it.
    #[allow(unused_mut)]
    let mut chain: Vec<ContentEntry<'_>> = Vec::new();
    #[cfg(feature = "tdm-aps")]
    chain.push(ContentEntry {
        name: "tdm-aps",
        enable_hint: &["DOIGET_KEY_APS", "DOIGET_AGREE_TDM_APS"],
        prefixes: crate::sources::tdm_aps::PUBLISHER_PREFIXES,
        publisher: "American Physical Society (APS)",
        src: &aps,
    });
    #[cfg(feature = "tdm-elsevier")]
    chain.push(ContentEntry {
        name: "tdm-elsevier",
        enable_hint: &["DOIGET_KEY_ELSEVIER", "DOIGET_AGREE_TDM_ELSEVIER"],
        prefixes: crate::sources::tdm_elsevier::PUBLISHER_PREFIXES,
        publisher: "Elsevier BV",
        src: &elsevier,
    });
    #[cfg(feature = "tdm-springer")]
    chain.push(ContentEntry {
        name: "tdm-springer",
        enable_hint: &["DOIGET_KEY_SPRINGER", "DOIGET_AGREE_TDM_SPRINGER"],
        prefixes: crate::sources::tdm_springer::PUBLISHER_PREFIXES,
        publisher: "Springer Nature",
        src: &springer,
    });
    #[cfg(feature = "tdm-ieee")]
    chain.push(ContentEntry {
        name: "tdm-ieee",
        enable_hint: &["DOIGET_KEY_IEEE", "DOIGET_AGREE_TDM_IEEE"],
        prefixes: crate::sources::tdm_ieee::PUBLISHER_PREFIXES,
        publisher: "IEEE",
        src: &ieee,
    });

    // Replace the metadata-stage row rather than appending a second one.
    // That row says `NotNeeded`, which was true of the metadata question
    // and is now false of the one being asked.
    fn record(attempts: &mut Vec<SourceAttempt>, name: &'static str, outcome: AttemptOutcome) {
        attempts.retain(|a| a.source != name);
        attempts.push(SourceAttempt::new(name, outcome));
    }

    for e in chain {
        debug_assert_eq!(e.name, e.src.name(), "chain name must match Source::name");

        // Prefix BEFORE credentials, per ADR-0041: a DOI this publisher
        // never registered is not a configuration problem, and reporting
        // it as one would send the user after an API key that would not
        // have helped.
        if !e.prefixes.contains(&doi.prefix()) {
            record(
                attempts,
                e.name,
                AttemptOutcome::WrongPublisher {
                    detail: format!("DOI prefix {} is not {}", doi.prefix(), e.publisher),
                },
            );
            continue;
        }
        if !e.src.can_serve(profile, &ref_) {
            record(
                attempts,
                e.name,
                AttemptOutcome::Disabled { env: e.enable_hint },
            );
            continue;
        }

        match e.src.fetch_content(&ref_, profile, ctx).await {
            // A metadata-only source. It has nothing to say about the
            // content question, so its metadata-stage row is left alone
            // rather than overwritten with a verdict it never gave.
            Ok(None) => {}
            Ok(Some(bytes)) => {
                tracing::info!(
                    source = e.name,
                    doi = %doi.as_str(),
                    size = bytes.len(),
                    "OA routes exhausted; the publisher served its own copy under the user's TDM agreement (#458)"
                );
                record(attempts, e.name, AttemptOutcome::Resolved);
                return (
                    PdfLegStatus::TdmFetched {
                        source: e.name.to_string(),
                        original_block,
                    },
                    Some(bytes.to_vec()),
                );
            }
            Err(err) => {
                tracing::warn!(
                    source = e.name,
                    error = %err,
                    "TDM content leg failed; keeping the original block"
                );
                record(attempts, e.name, classify_attempt(&err));
            }
        }
    }

    (pdf_leg, pdf_bytes)
}

/// Auto preprint fallback (issue #325). Called after the DOI OA PDF chain
/// walk. When `pdf_leg` is `PdfLegStatus::Blocked` with a
/// `suggested_arxiv_id`, the arXiv source is tried; on success the caller
/// receives the PDF bytes, the parsed [`ArxivId`], the arXiv license, and
/// a [`PdfLegStatus::PreprintFallback`] leg status so the PDF is stored
/// under the DOI safekey with full audit provenance.
///
/// When no fallback is applicable (leg is not `Blocked`, or no
/// `suggested_arxiv_id`), `pdf_leg` and `oa_pdf_bytes` are returned
/// unchanged. On any fetch failure the original `Blocked` leg is kept.
async fn try_arxiv_preprint_fallback(
    doi: &Doi,
    pdf_leg: PdfLegStatus,
    oa_pdf_bytes: Option<Vec<u8>>,
    profile: &CapabilityProfile,
    ctx: &FetchContext,
) -> (
    PdfLegStatus,
    Option<Vec<u8>>,
    Option<ArxivId>,
    Option<String>,
) {
    let (arxiv_id_str, original_block) = match &pdf_leg {
        PdfLegStatus::Blocked {
            suggested_arxiv_id: Some(s),
            message,
            ..
        } => (s.clone(), message.clone()),
        _ => return (pdf_leg, oa_pdf_bytes, None, None),
    };

    let arxiv_id = match ArxivId::parse(&arxiv_id_str) {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!(
                error = %e,
                arxiv_id = %arxiv_id_str,
                doi = %doi.as_str(),
                "preprint fallback: could not parse suggested_arxiv_id; keeping Blocked"
            );
            return (pdf_leg, oa_pdf_bytes, None, None);
        }
    };

    tracing::info!(
        doi = %doi.as_str(),
        arxiv_id = %arxiv_id.as_str(),
        "OA PDF blocked; attempting arXiv preprint fallback (issue #325)"
    );

    let arxiv_ref = Ref::Arxiv(arxiv_id.clone());
    let arxiv_source = arxiv_source_from_env();
    match arxiv_source.fetch(&arxiv_ref, profile, ctx).await {
        Ok(result) => match result.pdf_bytes {
            Some(bytes) => {
                tracing::info!(
                    doi = %doi.as_str(),
                    arxiv_id = %arxiv_id.as_str(),
                    size = bytes.len(),
                    "arXiv preprint fallback succeeded; storing under DOI safekey (issue #325)"
                );
                let license = result.license;
                (
                    PdfLegStatus::PreprintFallback {
                        arxiv_id: arxiv_id.as_str().to_string(),
                        original_block,
                    },
                    Some(bytes.to_vec()),
                    Some(arxiv_id),
                    Some(license),
                )
            }
            None => {
                tracing::warn!(
                    doi = %doi.as_str(),
                    arxiv_id = %arxiv_id.as_str(),
                    "preprint fallback: arXiv source returned no PDF bytes; keeping Blocked"
                );
                (pdf_leg, oa_pdf_bytes, None, None)
            }
        },
        Err(e) => {
            tracing::warn!(
                error = %e,
                doi = %doi.as_str(),
                arxiv_id = %arxiv_id.as_str(),
                "preprint fallback: arXiv fetch also failed; keeping Blocked"
            );
            (pdf_leg, oa_pdf_bytes, None, None)
        }
    }
}

/// Stage PDF bytes to a tempfile so the existing `Store::write` atomic-
/// rename code path applies (the store takes a path, not bytes).
fn stage_pdf_to_tempfile(bytes: &[u8]) -> Result<tempfile::NamedTempFile, FetchError> {
    let tmp = tempfile::NamedTempFile::new().map_err(|e| FetchError::SourceSchema {
        hint: format!("creating PDF staging tempfile: {e}"),
    })?;
    std::fs::write(tmp.path(), bytes).map_err(|e| FetchError::SourceSchema {
        hint: format!("staging PDF bytes: {e}"),
    })?;
    Ok(tmp)
}

/// Persist `metadata` (and optionally a PDF at `pdf_src`) through the
/// trait-object [`Store`] and emit a `StoreWrite` provenance row.
fn write_metadata_and_pdf(
    store: &dyn Store,
    safekey: &Safekey,
    metadata: &Metadata,
    pdf_src: Option<&Utf8Path>,
    ctx: &FetchContext,
) -> Result<(), FetchError> {
    let store_path_relative = if pdf_src.is_some() {
        format!("{}.pdf", safekey.as_str())
    } else {
        format!(".metadata/{}.toml", safekey.as_str())
    };
    let size_bytes = metadata.doiget.as_ref().map(|d| d.size_bytes).unwrap_or(0);
    let license = metadata.doiget.as_ref().map(|d| d.license.as_str());
    let source_name = metadata.doiget.as_ref().map(|d| d.source.as_str());

    // ADR-0021 §1 canonical-digest for the StoreWrite row. The store
    // entry is keyed on the ref + the resolver that produced its
    // metadata (already captured in `metadata.doiget.source`). Build a
    // CanonicalRef from whichever id slot is populated.
    let canonical_digest: Option<String> = match (metadata.doi.as_ref(), metadata.arxiv_id.as_ref())
    {
        (Some(d), _) => source_name.map(|s| {
            crate::CanonicalRef::new(crate::SourceType::Doi, d.as_str(), s, None).digest_hex()
        }),
        (None, Some(a)) => source_name.map(|s| {
            crate::CanonicalRef::new(crate::SourceType::Arxiv, a.as_str(), s, None).digest_hex()
        }),
        (None, None) => None,
    };

    match store.write(safekey, metadata, pdf_src) {
        Ok(()) => {
            ctx.log.append(RowInput {
                event: LogEvent::StoreWrite,
                result: LogResult::Ok,
                capability: Capability::Oa,
                ref_: metadata
                    .doi
                    .as_ref()
                    .map(|d| d.as_str())
                    .or_else(|| metadata.arxiv_id.as_ref().map(|a| a.as_str())),
                source: source_name,
                error_code: None,
                size_bytes: Some(size_bytes),
                license,
                store_path: Some(&store_path_relative),
                canonical_digest: canonical_digest.as_deref(),
            })?;
            Ok(())
        }
        Err(e) => {
            // Best-effort: record the StoreWrite failure before
            // propagating the store.write error. We do NOT
            // propagate the log-append error itself here — we're
            // already in an error state from the store, and the
            // primary failure is what the caller needs to act on.
            // But the log-append failure is observable via tracing
            // so an operator can spot a broken hash chain when
            // both fail. Surface as `SourceSchema` so the
            // FetchError -> ErrorCode collapse routes it to
            // `INTERNAL_ERROR` (closest closed-set fit; `StoreError`
            // does not have a direct closed-set arm).
            if let Err(log_err) = ctx.log.append(RowInput {
                event: LogEvent::StoreWrite,
                result: LogResult::Err,
                capability: Capability::Oa,
                ref_: metadata
                    .doi
                    .as_ref()
                    .map(|d| d.as_str())
                    .or_else(|| metadata.arxiv_id.as_ref().map(|a| a.as_str())),
                source: source_name,
                error_code: Some("STORE_ERROR"),
                size_bytes: None,
                license: None,
                store_path: Some(&store_path_relative),
                canonical_digest: canonical_digest.as_deref(),
            }) {
                tracing::error!(
                    store_err = %e,
                    log_err = %log_err,
                    "BOTH store.write AND provenance log append failed; \
                     audit trail is broken for this attempt"
                );
            }
            Err(FetchError::SourceSchema {
                hint: format!("store write failed: {e}"),
            })
        }
    }
}

/// Attempt the OA PDF fetch under the `"oa-publisher"` source key.
async fn try_fetch_oa_pdf(
    doi: &Doi,
    url: &url::Url,
    ctx: &FetchContext,
) -> Result<(Vec<u8>, url::Url), HttpError> {
    const SOURCE: &str = "oa-publisher";
    let _permit = ctx.rate_limiter.acquire(SOURCE).await;
    // ADR-0021 §1: the oa-publisher PDF leg is a DISTINCT audit
    // identity from the Crossref/Unpaywall metadata legs even though
    // the ref is the same DOI — that's the whole point of carrying
    // `resolver_profile` into the digest. Compute once and re-use for
    // both the ok and err row variants below.
    let canonical =
        crate::CanonicalRef::new(crate::SourceType::Doi, doi.as_str(), SOURCE, None).digest_hex();

    // Pre-fetch host allowlist check on the metadata-discovered OA URL
    // (issue #145; `docs/REDIRECT_ALLOWLIST.md` §1 — NORMATIVE). The
    // per-source `redirect_hosts` allowlist is, by §1, consulted "on the
    // OA URL discovered through metadata sources before the actual PDF
    // fetch is issued", not only on redirect hops. The redirect closure in
    // `crate::http` only fires when an *actual redirect* occurs; an OA URL
    // whose host is off the `oa-publisher` allowlist that resolves WITHOUT
    // a redirect would otherwise reach connect and be misclassified as a
    // transport error, violating §1. This is scoped strictly to the
    // `"oa-publisher"` PDF leg — §6 explicitly exempts the initial
    // template-constructed URL, and `fetch_bytes`/metadata-only/resolve-
    // only paths (which never follow the OA URL) are deliberately NOT
    // touched. On a host MISS we return the *same* `HttpError::RedirectDenied`
    // value the redirect closure produces (same `source_key`, lowercased
    // `host`, and `expected_hosts` snapshot), reusing the identical
    // allowlist the closure captured (queried via `source_allowlist`, not
    // re-derived) so the single source of truth cannot drift. Returning
    // that exact variant means the existing `Err(e)` arm below, the
    // `From<&HttpError> for Option<DenialContext>` mapping
    // (`DenialReason::RedirectNotInAllowlist`), the `PdfLegStatus::Blocked`
    // construction in the caller, and PR #162's CLI classification all see
    // a byte-identical downstream shape with no new code path.
    if let Some(allowlist) = ctx.http.source_allowlist(SOURCE) {
        // `Url::host_str()` is `None` for hostless URLs (e.g. `data:`);
        // treat that exactly as the redirect closure does (an allowlist
        // miss with an empty host string).
        let host = url
            .host_str()
            .map(|h| h.to_ascii_lowercase())
            .unwrap_or_default();
        if !allowlist.matches(&host) {
            let e = HttpError::RedirectDenied {
                source_key: SOURCE.to_string(),
                host: host.clone(),
                expected_hosts: allowlist.redirect_hosts.clone(),
            };
            tracing::info!(
                oa_url = %url,
                denied_host = %host,
                "OA URL host outside oa-publisher allowlist (pre-fetch check, \
                 docs/REDIRECT_ALLOWLIST.md §1 / issue #145)"
            );
            // Emit the SAME provenance row the post-fetch redirect-denied
            // path emits: a `Fetch` `Err` row under the `oa-publisher`
            // source key with the closed-set `NETWORK_ERROR` code and the
            // same canonical digest. Mirrors the `Err(e)` arm below so the
            // audit trail is indistinguishable from a redirect-time denial.
            let _ = ctx.log.append(RowInput {
                event: LogEvent::Fetch,
                result: LogResult::Err,
                capability: Capability::Oa,
                ref_: Some(doi.as_str()),
                source: Some(SOURCE),
                error_code: Some(crate::ErrorCode::NetworkError.as_wire()),
                size_bytes: None,
                license: None,
                store_path: None,
                canonical_digest: Some(&canonical),
            });
            return Err(e);
        }
    }

    match ctx.http.fetch_pdf(SOURCE, url.clone()).await {
        Ok((body, final_url)) => {
            let size_bytes = body.len() as u64;
            if let Err(e) = ctx.log.append(RowInput {
                event: LogEvent::Fetch,
                result: LogResult::Ok,
                capability: Capability::Oa,
                ref_: Some(doi.as_str()),
                source: Some(SOURCE),
                error_code: None,
                size_bytes: Some(size_bytes),
                license: None,
                store_path: None,
                canonical_digest: Some(&canonical),
            }) {
                tracing::warn!(error = %e, "appending oa-publisher Fetch ok row failed");
            }
            Ok((body.to_vec(), final_url))
        }
        Err(e) => {
            match &e {
                HttpError::RedirectDenied { host, .. } => {
                    tracing::info!(
                        oa_url = %url,
                        denied_host = %host,
                        "OA URL host outside oa-publisher allowlist"
                    );
                }
                HttpError::NotAPdf { .. } => {
                    tracing::info!(
                        oa_url = %url,
                        "OA URL did not return a PDF magic byte"
                    );
                }
                other => {
                    tracing::warn!(
                        oa_url = %url,
                        error = %other,
                        "OA PDF fetch failed"
                    );
                }
            }
            // Provenance `error_code` is the CLOSED-set code. Every
            // `HttpError` collapses to `NETWORK_ERROR` through the
            // canonical `From<FetchError> for ErrorCode` (the closed
            // set has no finer transport code by design) — so this is
            // the correct mapped value, not the misattribution the
            // previous hardcode implied. The *fine* reason
            // (RedirectDenied vs NotAPdf vs …) is preserved for the
            // user via `PdfLegStatus::Blocked.denial` / `.message`
            // built by the caller from the returned `HttpError`
            // (issue #118). Rendered via `ErrorCode::as_wire` so the
            // token can never drift from the enum.
            let _ = ctx.log.append(RowInput {
                event: LogEvent::Fetch,
                result: LogResult::Err,
                capability: Capability::Oa,
                ref_: Some(doi.as_str()),
                source: Some(SOURCE),
                error_code: Some(crate::ErrorCode::NetworkError.as_wire()),
                size_bytes: None,
                license: None,
                store_path: None,
                canonical_digest: Some(&canonical),
            });
            Err(e)
        }
    }
}

/// Subset of Crossref `message` fields populated into the on-disk metadata.
#[derive(Default)]
pub(crate) struct CrossrefFields {
    pub(crate) title: Option<String>,
    pub(crate) authors: Vec<String>,
    pub(crate) year: Option<i32>,
    pub(crate) venue: Option<String>,
    pub(crate) volume: Option<String>,
    pub(crate) issue: Option<String>,
    pub(crate) pages: Option<String>,
    pub(crate) type_: Option<String>,
}

/// Map a DataCite `data.attributes` object onto [`CrossrefFields`].
///
/// Issue #414. DataCite is a different registration agency with a
/// different schema, but downstream only consumes [`CrossrefFields`], so
/// the shapes are reconciled here rather than by widening every consumer.
///
/// `type_` deliberately carries `types.resourceTypeGeneral` — on Zenodo,
/// dataset plus software plus image outnumber `JournalArticle`, and an
/// agent that cannot tell them apart will treat a software release as a
/// paper. Every field degrades to `None` rather than guessing.
#[cfg(feature = "metadata")]
pub(crate) fn extract_datacite_fields(attributes: &Value) -> CrossrefFields {
    let title = attributes
        .get("titles")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|t| t.get("title"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let authors = attributes
        .get("creators")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|c| {
                    // `name` is the canonical field; fall back to the
                    // given/family pair when a depositor supplied only that.
                    c.get("name")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                        .or_else(|| {
                            let given = c.get("givenName").and_then(|v| v.as_str());
                            let family = c.get("familyName").and_then(|v| v.as_str());
                            match (given, family) {
                                (Some(g), Some(f)) => Some(format!("{g} {f}")),
                                (None, Some(f)) => Some(f.to_string()),
                                _ => None,
                            }
                        })
                })
                .collect()
        })
        .unwrap_or_default();
    let year = attributes
        .get("publicationYear")
        .and_then(serde_json::Value::as_i64)
        .and_then(|y| i32::try_from(y).ok());
    let venue = attributes.get("publisher").and_then(|v| {
        // DataCite 4.5 made `publisher` an object; earlier records
        // carry a bare string.
        v.as_str()
            .map(str::to_string)
            .or_else(|| v.get("name").and_then(|n| n.as_str()).map(str::to_string))
    });
    let type_ = crate::sources::datacite::resource_type_general(attributes).map(str::to_string);
    CrossrefFields {
        title,
        authors,
        year,
        venue,
        volume: None,
        issue: None,
        pages: None,
        type_,
    }
}

/// Defensively pull bibliographic fields out of a Crossref envelope's
/// message object. Every field is optional; malformed shapes degrade
/// to None rather than panicking.
pub(crate) fn extract_crossref_fields(msg: &Value) -> CrossrefFields {
    let title = msg
        .get("title")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let authors = msg
        .get("author")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|a| {
                    let family = a.get("family").and_then(|v| v.as_str());
                    let given = a.get("given").and_then(|v| v.as_str());
                    match (family, given) {
                        (Some(f), Some(g)) => Some(format!("{f}, {g}")),
                        (Some(f), None) => Some(f.to_string()),
                        (None, Some(g)) => Some(g.to_string()),
                        _ => None,
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    let year = msg
        .get("issued")
        .and_then(|v| v.get("date-parts"))
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_i64())
        .and_then(|n| i32::try_from(n).ok());

    let venue = msg
        .get("container-title")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let type_ = msg
        .get("type")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let volume = msg
        .get("volume")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let issue = msg
        .get("issue")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // Crossref `page` uses a single hyphen for ranges (`477-528`); BibTeX
    // convention is an en-dash (`477--528`). See `normalize_page_range`.
    let pages = msg
        .get("page")
        .and_then(|v| v.as_str())
        .map(normalize_page_range);

    CrossrefFields {
        title,
        authors,
        year,
        venue,
        volume,
        issue,
        pages,
        type_,
    }
}

/// Pull the ordered chain of candidate OA URLs out of an Unpaywall
/// `metadata_json` envelope per ADR-0029 D2.
///
/// Order is `best_oa_location` first (when present), then every
/// distinct entry in `oa_locations[]`. Duplicate URLs are deduped by
/// exact string match so a candidate that appears as both the "best"
/// entry and an array element is fetched at most once.
///
/// Each location's URL is resolved via the same `url_for_pdf` →
/// `url` fallback the single-URL extractor uses.
///
/// Returns `Vec::new()` when no OA location was reported (the chain
/// is empty and the caller surfaces [`PdfLegStatus::NoOaUrl`]).
fn extract_oa_url_chain(meta: Option<&Value>) -> Vec<url::Url> {
    let meta = match meta {
        Some(m) => m,
        None => return Vec::new(),
    };
    let mut out: Vec<url::Url> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut push_unique = |u: url::Url| {
        let key = u.as_str().to_string();
        if seen.insert(key) {
            out.push(u);
        }
    };

    // Priority 1: best_oa_location (Unpaywall's own quality-ordered
    // pick — ADR-0029 D2 NORMATIVE: defer to the metadata source's
    // ordering).
    if let Some(best) = meta.get("best_oa_location") {
        if let Some(u) = pull_oa_url_from_location(best) {
            push_unique(u);
        }
    }
    // Priority 2: every entry in oa_locations[] after the best one.
    // The fallback target this ADR exists to enable is precisely the
    // arXiv preprint that lives here when `best_oa_location` is a
    // WAF-blocked publisher URL.
    if let Some(arr) = meta.get("oa_locations").and_then(|v| v.as_array()) {
        for loc in arr {
            if let Some(u) = pull_oa_url_from_location(loc) {
                push_unique(u);
            }
        }
    }
    out
}

/// Resolve a single OA location object to a `url::Url`. Tries
/// `url_for_pdf` first (the direct PDF link Unpaywall annotates when
/// it knows one), falling back to `url` (the landing page). Returns
/// `None` if neither field is present or parses.
fn pull_oa_url_from_location(loc: &Value) -> Option<url::Url> {
    let candidate = loc
        .get("url_for_pdf")
        .and_then(|v| v.as_str())
        .or_else(|| loc.get("url").and_then(|v| v.as_str()))?;
    url::Url::parse(candidate).ok()
}

/// Helper to parse clean arXiv IDs from URLs like arxiv.org/pdf/1901.12345.pdf.
///
/// Strips the trailing `.pdf` extension and any version suffix (`v1`, `v2`, …)
/// so the returned ID refers to the latest version rather than pinning a
/// specific one. Returns `None` for non-arXiv hosts or unrecognised path shapes.
fn extract_arxiv_id_from_url(url: &url::Url) -> Option<String> {
    let host = url.host_str()?;
    let is_arxiv = matches!(
        host,
        "arxiv.org" | "www.arxiv.org" | "export.arxiv.org" | "e-print.arxiv.org"
    );
    if !is_arxiv {
        return None;
    }
    let path = url.path();
    let raw = if path.starts_with("/pdf/") {
        let s = path.strip_prefix("/pdf/")?;
        s.strip_suffix(".pdf").unwrap_or(s)
    } else if path.starts_with("/abs/") {
        path.strip_prefix("/abs/")?
    } else {
        return None;
    };
    Some(strip_arxiv_version(raw).to_string())
}

/// Strip a trailing arXiv version suffix (`v1`, `v2`, …) from an ID string.
///
/// Recognises the suffix only when the `v` is **preceded by a digit** (ruling
/// out category fragments like `quant-ph`) and followed by one or more ASCII
/// digits. Leaves IDs without a recognisable version suffix unchanged.
fn strip_arxiv_version(id: &str) -> &str {
    if let Some(v_pos) = id.rfind('v') {
        let before_v = id[..v_pos].chars().next_back();
        let suffix = &id[v_pos + 1..];
        if before_v.is_some_and(|c| c.is_ascii_digit())
            && !suffix.is_empty()
            && suffix.bytes().all(|b| b.is_ascii_digit())
        {
            return &id[..v_pos];
        }
    }
    id
}

fn unpaywall_email_from_env(fallback_contact: &str) -> String {
    std::env::var("DOIGET_UNPAYWALL_EMAIL").unwrap_or_else(|_| fallback_contact.to_string())
}

// ---------------------------------------------------------------------------
// batch_fetch — multi-ref orchestrator (Slice 2)
// ---------------------------------------------------------------------------

/// Per-ref outcome carried inside [`BatchOutcome::results`].
///
/// Each entry's `outcome` is independent — a single `Err(...)` does not
/// abort sibling refs. The MCP `doiget_batch_fetch` tool method
/// serializes the success-or-error per row inside `results[]`.
#[derive(Debug)]
pub struct BatchResultEntry {
    /// The parsed ref this entry describes.
    pub ref_: Ref,
    /// `Ok(...)` on a successful fetch through [`fetch_paper`];
    /// `Err(...)` on a per-ref failure (the outer call still returned
    /// `Ok(BatchOutcome)`).
    pub outcome: Result<FetchPaperOutcome, FetchError>,
}

/// Outcome of a successful [`batch_fetch`] call.
///
/// The outer call returns `Err(_)` only on whole-call failures (the
/// only such variant in Slice 2 is [`FetchError::TooManyRefs`]). Each
/// per-ref result lives inside `results[]` so the agent can see every
/// outcome without losing sibling successes.
#[derive(Debug)]
#[non_exhaustive]
pub struct BatchOutcome {
    /// One entry per supplied ref, in input order.
    pub results: Vec<BatchResultEntry>,
}

/// Iterate over `refs` through [`fetch_paper`], collecting one
/// [`BatchResultEntry`] per ref.
///
/// **Cap**: caller must supply at most [`MAX_BATCH_REFS`] refs; otherwise
/// the function returns `Err(FetchError::TooManyRefs { got, max })`
/// before any fetch is attempted. The cap mirrors the CLI's
/// `commands::batch` enforcement (`MCP_BATCH_MAX_SIZE`).
///
/// **Concurrency**: Slice 2 dispatches refs serially through
/// [`fetch_paper`]. The CLI's existing `commands::batch::run_with_options`
/// keeps its bounded-concurrency `JoinSet`+semaphore path for backward
/// compatibility; the MCP server uses this serial loop because the MCP
/// tool boundary already serializes calls per session.
///
/// **Session bookkeeping**: this function does NOT emit `SessionStart`
/// / `SessionEnd` rows — that is the caller's responsibility.
pub async fn batch_fetch(
    refs: &[Ref],
    profile: &CapabilityProfile,
    ctx: &FetchContext,
    store: &dyn Store,
    store_root: &Utf8Path,
) -> Result<BatchOutcome, FetchError> {
    if refs.len() > MAX_BATCH_REFS {
        return Err(FetchError::TooManyRefs {
            got: refs.len(),
            max: MAX_BATCH_REFS,
        });
    }
    let mut results = Vec::with_capacity(refs.len());
    for ref_ in refs {
        let outcome = fetch_paper(ref_, profile, ctx, store, store_root).await;
        results.push(BatchResultEntry {
            ref_: ref_.clone(),
            outcome,
        });
    }
    Ok(BatchOutcome { results })
}

/// Dry-run preview for a batch — one [`FetchPlan`] per ref. Enforces
/// the same [`MAX_BATCH_REFS`] cap [`batch_fetch`] does.
///
/// Returns `Err(FetchError::TooManyRefs)` when over the cap, or
/// `Err(FetchError::SourceSchema)` if the dry-run allowlist invariant
/// has drifted (issue #156 ②: this now propagates as a typed error via
/// [`try_build_fetch_plan`] rather than silently emitting an empty
/// `candidate_hosts` list — the signature already returned `Result`, so
/// this is an in-crate behavior tightening with no caller-visible type
/// change). Otherwise `Ok(Vec<(Ref, FetchPlan)>)` parallel to the input
/// order.
pub fn batch_fetch_plans(
    refs: &[Ref],
    store_root: &Utf8Path,
) -> Result<Vec<(Ref, FetchPlan)>, FetchError> {
    if refs.len() > MAX_BATCH_REFS {
        return Err(FetchError::TooManyRefs {
            got: refs.len(),
            max: MAX_BATCH_REFS,
        });
    }
    refs.iter()
        .map(|r| try_build_fetch_plan(r, store_root).map(|p| (r.clone(), p)))
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    /// A Crossref `message` envelope (the shape `metadata_only_doi` stores
    /// in `MetadataOnlyOutcome.metadata`: `CrossrefSource` returns
    /// `envelope.message`, not the outer `{status, message}` wrapper).
    fn crossref_outcome() -> MetadataOnlyOutcome {
        MetadataOnlyOutcome {
            source: "crossref".to_string(),
            resolver_profile: "crossref".to_string(),
            license: None,
            oa_url: None,
            oa_status: None,
            metadata: serde_json::json!({
                "title": ["Rigorous results on valence-bond ground states"],
                "author": [
                    { "family": "Affleck", "given": "Ian" },
                    { "family": "Lieb", "given": "Elliott H." },
                ],
                "issued": { "date-parts": [[1988, 6, 1]] },
                "container-title": ["Physical Review Letters"],
                "publisher": "American Physical Society",
                "ISSN": ["0031-9007", "1079-7114"],
                "volume": "59",
                "issue": "7",
                "page": "799-802",
                "type": "journal-article",
            }),
        }
    }

    #[test]
    fn cite_metadata_enriches_from_crossref_envelope() {
        let ref_ = Ref::parse("10.1103/PhysRevLett.59.799").unwrap();
        let m = cite_metadata(&ref_, &crossref_outcome());
        assert_eq!(m.title, "Rigorous results on valence-bond ground states");
        assert_eq!(m.authors, vec!["Affleck, Ian", "Lieb, Elliott H."]);
        assert_eq!(m.year, Some(1988));
        assert_eq!(m.venue.as_deref(), Some("Physical Review Letters"));
        assert_eq!(m.publisher.as_deref(), Some("American Physical Society"));
        // Crossref `ISSN` is an array; the first entry is taken.
        assert_eq!(m.issn.as_deref(), Some("0031-9007"));
        assert_eq!(m.volume.as_deref(), Some("59"));
        assert_eq!(m.issue.as_deref(), Some("7"));
        // Single hyphen normalized to a BibTeX en-dash.
        assert_eq!(m.pages.as_deref(), Some("799--802"));
        assert_eq!(m.type_.as_deref(), Some("journal-article"));
    }

    #[test]
    fn cite_metadata_non_crossref_keeps_minimal_baseline() {
        // An arXiv outcome must NOT be mined with the Crossref extractor:
        // its envelope shape differs, so year/venue/publisher stay None
        // rather than being fabricated.
        let ref_ = Ref::parse("arxiv:2401.12345").unwrap();
        let outcome = MetadataOnlyOutcome {
            source: "arxiv".to_string(),
            resolver_profile: "arxiv".to_string(),
            license: Some("arxiv-default".to_string()),
            oa_url: None,
            oa_status: Some("green".to_string()),
            metadata: serde_json::json!({ "title": "An arXiv Preprint" }),
        };
        let m = cite_metadata(&ref_, &outcome);
        assert_eq!(m.title, "An arXiv Preprint");
        assert_eq!(m.year, None);
        assert_eq!(m.venue, None);
        assert_eq!(m.publisher, None);
        assert_eq!(m.issn, None);
        assert!(m.arxiv_id.is_some());
    }

    #[test]
    fn cite_metadata_arxiv_overlay_fills_year_and_categories() {
        // Issue #303: an arXiv outcome whose Atom payload carries `published`
        // + `categories` populates year + arxiv_categories via the overlay
        // (not the Crossref extractor). Review #318: this path was untested.
        let ref_ = Ref::parse("arxiv:2401.12345").unwrap();
        let outcome = MetadataOnlyOutcome {
            source: "arxiv".to_string(),
            resolver_profile: "arxiv".to_string(),
            license: Some("arxiv-default".to_string()),
            oa_url: None,
            oa_status: Some("green".to_string()),
            metadata: serde_json::json!({
                "title": "An arXiv Preprint",
                "published": "2024-03-15T00:00:00Z",
                "categories": ["cond-mat.str-el", "cond-mat.dis-nn"],
            }),
        };
        let m = cite_metadata(&ref_, &outcome);
        assert_eq!(m.year, Some(2024));
        assert_eq!(
            m.arxiv_categories,
            vec!["cond-mat.str-el".to_string(), "cond-mat.dis-nn".to_string()]
        );

        // A malformed `published` omits the year rather than fabricating one.
        let bad = MetadataOnlyOutcome {
            metadata: serde_json::json!({ "title": "x", "published": "not-a-date" }),
            ..outcome
        };
        assert_eq!(cite_metadata(&ref_, &bad).year, None);
    }

    #[test]
    fn test_extract_arxiv_id_from_url() {
        let urls = [
            // Basic new-style ID
            ("https://arxiv.org/pdf/1901.12345.pdf", Some("1901.12345")),
            ("https://arxiv.org/abs/1901.12345", Some("1901.12345")),
            // Version suffix is stripped
            ("https://arxiv.org/pdf/1901.12345v2.pdf", Some("1901.12345")),
            ("https://arxiv.org/abs/1901.12345v3", Some("1901.12345")),
            // Old-style category/ID
            (
                "https://www.arxiv.org/pdf/cond-mat/9501001.pdf",
                Some("cond-mat/9501001"),
            ),
            (
                "https://export.arxiv.org/abs/cond-mat/9501001",
                Some("cond-mat/9501001"),
            ),
            // Old-style with version stripped
            (
                "https://arxiv.org/pdf/cond-mat/9501001v1.pdf",
                Some("cond-mat/9501001"),
            ),
            // e-print subdomain
            (
                "https://e-print.arxiv.org/pdf/2401.12345.pdf",
                Some("2401.12345"),
            ),
            // Non-arXiv host
            ("https://example.org/pdf/1901.12345.pdf", None),
        ];
        for (url_str, expected) in urls {
            let url = url::Url::parse(url_str).unwrap();
            assert_eq!(
                extract_arxiv_id_from_url(&url),
                expected.map(String::from),
                "url: {url_str}"
            );
        }
    }

    #[test]
    fn test_strip_arxiv_version() {
        assert_eq!(strip_arxiv_version("2401.12345v2"), "2401.12345");
        assert_eq!(strip_arxiv_version("2401.12345v10"), "2401.12345");
        assert_eq!(strip_arxiv_version("2401.12345"), "2401.12345");
        assert_eq!(
            strip_arxiv_version("cond-mat/9501001v3"),
            "cond-mat/9501001"
        );
        // "v" not followed by digits — unchanged
        assert_eq!(strip_arxiv_version("quant-phv5"), "quant-phv5");
    }

    #[test]
    fn extract_crossref_oa_url_finds_first_url() {
        let msg = serde_json::json!({
            "link": [
                {"URL": "https://example.org/free.pdf"},
                {"URL": "https://example.org/alt.pdf"}
            ]
        });
        assert_eq!(
            extract_crossref_oa_url(&msg),
            Some("https://example.org/free.pdf".to_string())
        );
    }

    #[test]
    fn extract_crossref_oa_url_returns_none_when_absent() {
        let msg = serde_json::json!({});
        assert!(extract_crossref_oa_url(&msg).is_none());
    }

    #[test]
    fn extract_crossref_oa_url_skips_empty_url_strings() {
        let msg = serde_json::json!({
            "link": [
                {"URL": ""},
                {"URL": "https://example.org/real.pdf"}
            ]
        });
        assert_eq!(
            extract_crossref_oa_url(&msg),
            Some("https://example.org/real.pdf".to_string())
        );
    }

    #[test]
    fn extract_unpaywall_oa_url_prefers_url_for_pdf() {
        let meta = serde_json::json!({
            "best_oa_location": {
                "url_for_pdf": "https://example.org/pdf",
                "url": "https://example.org/landing"
            }
        });
        assert_eq!(
            extract_unpaywall_oa_url(&meta),
            Some("https://example.org/pdf".to_string())
        );
    }

    #[test]
    fn extract_unpaywall_oa_url_falls_back_to_url() {
        let meta = serde_json::json!({
            "best_oa_location": {
                "url": "https://example.org/landing"
            }
        });
        assert_eq!(
            extract_unpaywall_oa_url(&meta),
            Some("https://example.org/landing".to_string())
        );
    }

    #[test]
    fn extract_unpaywall_oa_url_returns_none_when_absent() {
        let meta = serde_json::json!({});
        assert!(extract_unpaywall_oa_url(&meta).is_none());
    }

    #[test]
    fn extract_unpaywall_oa_status_present_absent_and_empty() {
        // Present → Some; absent → None; empty string → None ("not
        // determined", never a meaningful status — review #284).
        assert_eq!(
            extract_unpaywall_oa_status(&serde_json::json!({"oa_status": "gold"})).as_deref(),
            Some("gold")
        );
        assert!(extract_unpaywall_oa_status(&serde_json::json!({})).is_none());
        assert!(extract_unpaywall_oa_status(&serde_json::json!({"oa_status": ""})).is_none());
    }

    // ---------------------------------------------------------------
    // Slice 2: fetch_paper / batch_fetch coverage. The wiremock-driven
    // happy-path tests live in `crates/doiget-mcp/tests/...` (they need
    // a real `Store` impl and an HTTP client wired to `FetchContext`,
    // both of which the MCP integration tests already stand up). The
    // unit tests here pin the pure-function pieces (extractors, cap
    // enforcement, plan-shape preservation).
    // ---------------------------------------------------------------

    #[test]
    fn extract_crossref_fields_parses_minimal_shape() {
        let msg = serde_json::json!({
            "title": ["Example Title"],
            "author": [{ "family": "Smith", "given": "Alice" }],
            "issued": { "date-parts": [[2024, 1, 15]] },
            "container-title": ["Phys. Rev. X"],
            "type": "journal-article"
        });
        let f = extract_crossref_fields(&msg);
        assert_eq!(f.title.as_deref(), Some("Example Title"));
        assert_eq!(f.authors, vec!["Smith, Alice".to_string()]);
        assert_eq!(f.year, Some(2024));
        assert_eq!(f.venue.as_deref(), Some("Phys. Rev. X"));
        assert_eq!(f.type_.as_deref(), Some("journal-article"));
    }

    #[test]
    fn extract_crossref_fields_tolerates_missing() {
        let f = extract_crossref_fields(&serde_json::json!({}));
        assert!(f.title.is_none());
        assert!(f.authors.is_empty());
        assert!(f.year.is_none());
        assert!(f.venue.is_none());
        assert!(f.type_.is_none());
    }

    #[test]
    fn extract_oa_url_chain_prefers_best_url_for_pdf() {
        // `best_oa_location.url_for_pdf` is the highest-priority
        // candidate (ADR-0029 D2 — defer to the metadata source's
        // ordering). Falls back to `best_oa_location.url` only when
        // no PDF link is annotated.
        let meta = serde_json::json!({
            "best_oa_location": {
                "url_for_pdf": "https://example.org/pdf",
                "url": "https://example.org/landing"
            }
        });
        let chain = extract_oa_url_chain(Some(&meta));
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].as_str(), "https://example.org/pdf");
    }

    #[test]
    fn extract_oa_url_chain_falls_back_to_url_when_url_for_pdf_absent() {
        let meta = serde_json::json!({
            "best_oa_location": {
                "url": "https://example.org/landing"
            }
        });
        let chain = extract_oa_url_chain(Some(&meta));
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].as_str(), "https://example.org/landing");
    }

    #[test]
    fn extract_oa_url_chain_is_empty_when_no_locations() {
        let meta = serde_json::json!({});
        assert!(extract_oa_url_chain(Some(&meta)).is_empty());
        assert!(extract_oa_url_chain(None).is_empty());
    }

    #[test]
    fn extract_oa_url_chain_appends_oa_locations_after_best() {
        // ADR-0029 D2: best_oa_location first, then the rest of
        // oa_locations in metadata-source order. This is the load-
        // bearing test: it pins the fact that an arXiv preprint
        // listed *after* a WAF-blocked publisher in oa_locations[]
        // becomes a fallback candidate the chain walker can reach.
        let meta = serde_json::json!({
            "best_oa_location": {
                "url_for_pdf": "https://publisher.example.org/pdf"
            },
            "oa_locations": [
                {"url_for_pdf": "https://publisher.example.org/pdf"},
                {"url_for_pdf": "https://arxiv.org/pdf/2401.12345"},
                {"url": "https://repo.example.edu/handle/123"}
            ]
        });
        let chain = extract_oa_url_chain(Some(&meta));
        let strs: Vec<&str> = chain.iter().map(|u| u.as_str()).collect();
        assert_eq!(
            strs,
            vec![
                "https://publisher.example.org/pdf",
                "https://arxiv.org/pdf/2401.12345",
                "https://repo.example.edu/handle/123",
            ],
            "chain ordering MUST be best_oa_location first, oa_locations[] verbatim after"
        );
    }

    #[test]
    fn extract_oa_url_chain_dedupes_repeated_urls() {
        // A URL that appears as both `best_oa_location` and an entry
        // in `oa_locations[]` is fetched at most once. Without this,
        // a publisher whose record has the same URL in both slots
        // would consume two HTTP requests + two rate-limit ticks.
        let meta = serde_json::json!({
            "best_oa_location": {
                "url_for_pdf": "https://example.org/pdf"
            },
            "oa_locations": [
                {"url_for_pdf": "https://example.org/pdf"},
                {"url_for_pdf": "https://example.org/pdf"},
                {"url_for_pdf": "https://arxiv.org/pdf/2401.12345"}
            ]
        });
        let chain = extract_oa_url_chain(Some(&meta));
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].as_str(), "https://example.org/pdf");
        assert_eq!(chain[1].as_str(), "https://arxiv.org/pdf/2401.12345");
    }

    #[test]
    fn extract_oa_url_chain_skips_unparsable_urls() {
        // A malformed URL in oa_locations[] is dropped silently
        // rather than aborting the chain — the metadata source can
        // emit a stray entry without poisoning the whole fetch.
        let meta = serde_json::json!({
            "best_oa_location": {
                "url_for_pdf": "https://good.example.org/pdf"
            },
            "oa_locations": [
                {"url_for_pdf": "not a url"},
                {"url_for_pdf": "https://arxiv.org/pdf/2401.12345"}
            ]
        });
        let chain = extract_oa_url_chain(Some(&meta));
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].as_str(), "https://good.example.org/pdf");
        assert_eq!(chain[1].as_str(), "https://arxiv.org/pdf/2401.12345");
    }

    #[test]
    fn fetch_paper_plan_matches_build_fetch_plan() {
        // The slice-2-named alias is a thin pass-through to
        // `dry_run::build_fetch_plan`. Pin behavioral equivalence so
        // a future refactor that diverges them surfaces here.
        use crate::{ArxivId, Doi};
        let r = Ref::Doi(Doi("10.1234/example".to_string()));
        let root = Utf8PathBuf::from("/tmp/doiget-test");
        let plan_a = fetch_paper_plan(&r, &root);
        let plan_b = build_fetch_plan(&r, &root);
        assert_eq!(plan_a.metadata_sources, plan_b.metadata_sources);
        assert_eq!(plan_a.target_pdf_path, plan_b.target_pdf_path);
        assert_eq!(plan_a.target_metadata_path, plan_b.target_metadata_path);

        let r2 = Ref::Arxiv(ArxivId("2401.12345".to_string()));
        let plan_c = fetch_paper_plan(&r2, &root);
        let plan_d = build_fetch_plan(&r2, &root);
        assert_eq!(plan_c.pdf_sources[0].key, plan_d.pdf_sources[0].key);
    }

    #[test]
    fn batch_fetch_plans_returns_plan_per_ref_in_order() {
        use crate::{ArxivId, Doi};
        let refs = vec![
            Ref::Doi(Doi("10.1234/alpha".to_string())),
            Ref::Arxiv(ArxivId("2401.12345".to_string())),
        ];
        let root = Utf8PathBuf::from("/tmp/doiget-batch-test");
        let plans = batch_fetch_plans(&refs, &root).expect("under cap returns Ok");
        assert_eq!(plans.len(), 2);
        // Order preserved.
        assert!(matches!(plans[0].0, Ref::Doi(_)));
        assert!(matches!(plans[1].0, Ref::Arxiv(_)));
        // DOI plan carries the crossref + unpaywall metadata sources.
        assert_eq!(plans[0].1.metadata_sources, vec!["crossref", "unpaywall"]);
        // arXiv plan has the arxiv PDF source key.
        assert_eq!(plans[1].1.pdf_sources[0].key, "arxiv");
    }

    #[test]
    fn batch_fetch_plans_too_many_refs_returns_err() {
        use crate::Doi;
        // Build MAX_BATCH_REFS + 1 entries — boundary case.
        let n = MAX_BATCH_REFS + 1;
        let refs: Vec<Ref> = (0..n)
            .map(|i| Ref::Doi(Doi(format!("10.1234/n{}", i))))
            .collect();
        let root = Utf8PathBuf::from("/tmp/doiget-toomany");
        let err = batch_fetch_plans(&refs, &root).expect_err("over cap returns Err");
        match err {
            FetchError::TooManyRefs { got, max } => {
                assert_eq!(got, n);
                assert_eq!(max, MAX_BATCH_REFS);
            }
            other => panic!("expected TooManyRefs, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn batch_fetch_too_many_refs_returns_err_before_any_fetch() {
        // The cap is enforced before any per-ref work, so we don't need
        // a working store/network here — pass a sentinel store_root and
        // a dummy FetchContext that would panic on use.
        use crate::http::{tier_1_allowlist, HttpClient};
        use crate::provenance::ProvenanceLog;
        use crate::rate_limiter::RateLimiter;
        use crate::store::FsStore;
        use crate::{Doi, RateLimits};
        use std::sync::Arc;

        let td = tempfile::TempDir::new().expect("tempdir");
        let log_path = Utf8Path::from_path(td.path())
            .expect("utf-8")
            .join("log.jsonl");
        let store_root = Utf8Path::from_path(td.path())
            .expect("utf-8")
            .join("papers");

        let ctx = FetchContext {
            http: Arc::new(HttpClient::new(tier_1_allowlist()).expect("http client")),
            rate_limiter: Arc::new(RateLimiter::new(RateLimits::HARD_CODED)),
            log: Arc::new(
                ProvenanceLog::open(log_path, "01J0000000000000000000TEST".into())
                    .expect("provenance log"),
            ),
            session_id: "01J0000000000000000000TEST".into(),
            cache_root: None,
        };
        let profile = CapabilityProfile::for_tests();
        let store = FsStore::new(store_root.clone()).expect("fs store");

        let n = MAX_BATCH_REFS + 1;
        let refs: Vec<Ref> = (0..n)
            .map(|i| Ref::Doi(Doi(format!("10.1234/n{}", i))))
            .collect();

        let err = batch_fetch(&refs, &profile, &ctx, &store, &store_root)
            .await
            .expect_err("over cap returns Err");
        match err {
            FetchError::TooManyRefs { got, max } => {
                assert_eq!(got, n);
                assert_eq!(max, MAX_BATCH_REFS);
            }
            other => panic!("expected TooManyRefs, got: {other:?}"),
        }
    }

    // Issue #118: a non-PDF OA body must surface as `Err(HttpError)`
    // from `try_fetch_oa_pdf` (previously silently flattened to
    // `None`, which `fetch_paper_doi` then reported as a clean
    // metadata-only success). The compiler-checked `Err(e) =>
    // PdfLegStatus::Blocked` arm in `fetch_paper_doi` does the rest.
    #[tokio::test]
    async fn try_fetch_oa_pdf_non_pdf_body_is_err_not_silent_none() {
        use crate::http::HttpClient;
        use crate::provenance::ProvenanceLog;
        use crate::rate_limiter::RateLimiter;
        use crate::{Doi, RateLimits};
        use std::sync::Arc;
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200).set_body_bytes(b"<html>not a pdf</html>".to_vec()),
            )
            .mount(&server)
            .await;
        let host = server
            .uri()
            .parse::<url::Url>()
            .expect("uri")
            .host_str()
            .expect("host")
            .to_string();

        let td = tempfile::TempDir::new().expect("tempdir");
        let log_path = Utf8Path::from_path(td.path())
            .expect("utf-8")
            .join("log.jsonl");
        let ctx = FetchContext {
            http: Arc::new(HttpClient::new_for_tests_allow_http("oa-publisher", &host)),
            rate_limiter: Arc::new(RateLimiter::new(RateLimits::HARD_CODED)),
            log: Arc::new(
                ProvenanceLog::open(log_path, "01J0000000000000000000TEST".into())
                    .expect("provenance log"),
            ),
            session_id: "01J0000000000000000000TEST".into(),
            cache_root: None,
        };

        let doi = Doi("10.1234/example".to_string());
        let url: url::Url = format!("{}/oa.pdf", server.uri()).parse().expect("url");
        let res = try_fetch_oa_pdf(&doi, &url, &ctx).await;
        match res {
            Err(HttpError::NotAPdf { .. }) => {}
            other => panic!("expected Err(NotAPdf), got: {other:?}"),
        }
    }

    // Issue #145 / `docs/REDIRECT_ALLOWLIST.md` §1: the `oa-publisher`
    // host allowlist MUST be consulted on the metadata-discovered OA URL
    // *before the actual PDF fetch is issued*, not only on redirect hops.
    // An OA URL whose host is OFF the allowlist and that resolves WITHOUT
    // a redirect previously slipped past the redirect closure entirely and
    // was misclassified as a transport error. This test pins the fix: the
    // pre-fetch check rejects it with the SAME `HttpError::RedirectDenied`
    // the redirect closure produces, the OA fetch is NEVER issued (the
    // wiremock origin records ZERO requests, proving no PDF bytes were
    // requested / written), and the provenance trail is the byte-identical
    // `Fetch`/`err`/`oa-publisher`/`NETWORK_ERROR` row the redirect-denied
    // path emits.
    #[tokio::test]
    async fn try_fetch_oa_pdf_off_allowlist_host_no_redirect_is_redirect_denied_145() {
        use crate::http::HttpClient;
        use crate::provenance::ProvenanceLog;
        use crate::rate_limiter::RateLimiter;
        use crate::{DenialContext, DenialReason, Doi, RateLimits};
        use std::sync::Arc;
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // The wiremock origin would serve a valid PDF with NO redirect —
        // if the pre-check were absent the fetch would *succeed* against
        // an off-allowlist host, which is exactly the §1 violation.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"%PDF-1.7 real pdf".to_vec()))
            .mount(&server)
            .await;

        // Register a DIFFERENT host as the `oa-publisher` allowlist so the
        // wiremock origin (127.0.0.1) is OFF it. `evil.example.com` is a
        // valid host string the allowlist will not match.
        let td = tempfile::TempDir::new().expect("tempdir");
        let log_path = Utf8Path::from_path(td.path())
            .expect("utf-8")
            .join("log.jsonl");
        let ctx = FetchContext {
            http: Arc::new(HttpClient::new_for_tests_allow_http(
                "oa-publisher",
                "allowed-publisher.example.com",
            )),
            rate_limiter: Arc::new(RateLimiter::new(RateLimits::HARD_CODED)),
            log: Arc::new(
                ProvenanceLog::open(log_path.clone(), "01J0000000000000000000TEST".into())
                    .expect("provenance log"),
            ),
            session_id: "01J0000000000000000000TEST".into(),
            cache_root: None,
        };

        let doi = Doi("10.1234/example".to_string());
        // The OA URL Unpaywall handed back resolves to the wiremock host,
        // which is OFF the `oa-publisher` allowlist.
        let off_host_url: url::Url = format!("{}/oa.pdf", server.uri()).parse().expect("url");
        let res = try_fetch_oa_pdf(&doi, &off_host_url, &ctx).await;

        // 1. Same error variant the redirect closure produces.
        let err = match res {
            Err(e @ HttpError::RedirectDenied { .. }) => e,
            other => {
                panic!("expected Err(RedirectDenied) from the pre-fetch check, got: {other:?}")
            }
        };
        match &err {
            HttpError::RedirectDenied {
                source_key,
                host,
                expected_hosts,
            } => {
                assert_eq!(source_key, "oa-publisher");
                // The host is lowercased, exactly as the redirect closure
                // would record it.
                assert_eq!(
                    host,
                    off_host_url
                        .host_str()
                        .expect("wiremock host")
                        .to_ascii_lowercase()
                        .as_str()
                );
                assert_eq!(
                    expected_hosts,
                    &vec!["allowed-publisher.example.com".to_string()]
                );
            }
            _ => unreachable!(),
        }

        // 2. The OA fetch was NEVER issued — the wiremock origin saw zero
        //    requests, so no PDF bytes were requested or written.
        assert!(
            server
                .received_requests()
                .await
                .unwrap_or_default()
                .is_empty(),
            "the off-allowlist OA URL must NOT be fetched: the pre-check \
             (REDIRECT_ALLOWLIST.md §1) rejects it before any request is \
             issued; wiremock recorded request(s)",
        );

        // 3. The structured denial side-channel is byte-identical to the
        //    redirect-closure path: `RedirectNotInAllowlist`, source key,
        //    attempted host, expected allowlist snapshot.
        let dc: Option<DenialContext> = (&err).into();
        let dc = dc.expect("pre-fetch RedirectDenied -> Some(DenialContext)");
        assert_eq!(dc.reason, DenialReason::RedirectNotInAllowlist);
        assert_eq!(dc.source.as_deref(), Some("oa-publisher"));
        assert_eq!(
            dc.attempted,
            Some(off_host_url.host_str().expect("host").to_ascii_lowercase()),
            "attempted host must be the rejected OA URL host, lowercased — \
             identical to what the redirect closure records",
        );
        assert_eq!(
            dc.expected,
            Some(vec!["allowed-publisher.example.com".to_string()]),
        );

        // 4. Provenance: exactly the `Fetch`/`err`/`oa-publisher`/
        //    `NETWORK_ERROR` row the post-fetch redirect-denied arm emits
        //    (same row kind + source key + closed-set code).
        let log_txt = std::fs::read_to_string(&log_path).expect("read provenance log");
        let fetch_err_row = log_txt
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .find(|v| {
                v.get("event").and_then(|e| e.as_str()) == Some("fetch")
                    && v.get("result").and_then(|r| r.as_str()) == Some("err")
            })
            .expect("a Fetch/err provenance row was written");
        assert_eq!(
            fetch_err_row.get("source").and_then(|s| s.as_str()),
            Some("oa-publisher"),
        );
        assert_eq!(
            fetch_err_row.get("error_code").and_then(|c| c.as_str()),
            Some("NETWORK_ERROR"),
        );
        assert_eq!(
            fetch_err_row.get("ref").and_then(|r| r.as_str()),
            Some("10.1234/example"),
        );
    }

    // Issue #145 positive / no-regression: an ON-allowlist OA URL still
    // fetches the PDF normally. The pre-fetch check must be a pure gate —
    // it must not perturb the happy path.
    #[tokio::test]
    async fn try_fetch_oa_pdf_on_allowlist_host_still_fetches_pdf_no_regression_145() {
        use crate::http::HttpClient;
        use crate::provenance::ProvenanceLog;
        use crate::rate_limiter::RateLimiter;
        use crate::{Doi, RateLimits};
        use std::sync::Arc;
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = b"%PDF-1.7\nhello pdf".to_vec();
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
            .mount(&server)
            .await;
        // The wiremock host IS the registered `oa-publisher` allowlist, so
        // the pre-check passes and the fetch proceeds as before.
        let host = server
            .uri()
            .parse::<url::Url>()
            .expect("uri")
            .host_str()
            .expect("host")
            .to_string();

        let td = tempfile::TempDir::new().expect("tempdir");
        let log_path = Utf8Path::from_path(td.path())
            .expect("utf-8")
            .join("log.jsonl");
        let ctx = FetchContext {
            http: Arc::new(HttpClient::new_for_tests_allow_http("oa-publisher", &host)),
            rate_limiter: Arc::new(RateLimiter::new(RateLimits::HARD_CODED)),
            log: Arc::new(
                ProvenanceLog::open(log_path, "01J0000000000000000000TEST".into())
                    .expect("provenance log"),
            ),
            session_id: "01J0000000000000000000TEST".into(),
            cache_root: None,
        };

        let doi = Doi("10.1234/example".to_string());
        let url: url::Url = format!("{}/oa.pdf", server.uri()).parse().expect("url");
        let (bytes, _final_url) = try_fetch_oa_pdf(&doi, &url, &ctx)
            .await
            .expect("on-allowlist OA URL still fetches the PDF");
        assert_eq!(bytes, body, "PDF bytes must be returned unchanged");
    }

    // Issue #145: the pre-fetch denial and the redirect-closure denial
    // MUST produce a byte-identical `DenialContext` so PR #162's CLI
    // classification (CAPABILITY_DENIED / exit 3) handles both unchanged.
    // This pins the equivalence at the value level: the same source key +
    // host + allowlist snapshot map through the SAME
    // `From<&HttpError> for Option<DenialContext>` impl to equal structs.
    #[test]
    fn pre_fetch_denial_produces_byte_identical_denial_context_as_redirect_denied_145() {
        use crate::{DenialContext, DenialReason};

        // Shape produced by the pre-fetch check in `try_fetch_oa_pdf`.
        let pre_fetch = HttpError::RedirectDenied {
            source_key: "oa-publisher".to_string(),
            host: "attacker.test".to_string(),
            expected_hosts: vec!["*.springer.com".to_string(), "*.plos.org".to_string()],
        };
        // Shape produced by the redirect closure in `crate::http` for the
        // identical inputs.
        let redirect_closure = HttpError::RedirectDenied {
            source_key: "oa-publisher".to_string(),
            host: "attacker.test".to_string(),
            expected_hosts: vec!["*.springer.com".to_string(), "*.plos.org".to_string()],
        };

        let dc_pre: Option<DenialContext> = (&pre_fetch).into();
        let dc_red: Option<DenialContext> = (&redirect_closure).into();
        let dc_pre = dc_pre.expect("pre-fetch -> Some");
        let dc_red = dc_red.expect("redirect -> Some");

        // Byte-identical: same reason, same source, same attempted host,
        // same expected snapshot, all auxiliary channels None.
        assert_eq!(dc_pre, dc_red);
        assert_eq!(dc_pre.reason, DenialReason::RedirectNotInAllowlist);
        assert_eq!(dc_pre.source.as_deref(), Some("oa-publisher"));
        assert_eq!(dc_pre.attempted.as_deref(), Some("attacker.test"));
        assert_eq!(
            dc_pre.expected,
            Some(vec!["*.springer.com".to_string(), "*.plos.org".to_string()]),
        );
        assert_eq!(dc_pre.hop_index, None);
        assert_eq!(dc_pre.cap, None);
        assert_eq!(dc_pre.actual, None);
    }

    // -----------------------------------------------------------------
    // #139 — metadata_only_to_store writes the metadata TOML;
    //        resolve_only / pure metadata_only write NOTHING.
    // -----------------------------------------------------------------

    /// Build a ctx + FsStore under a fresh tempdir and point Crossref at
    /// a wiremock origin that returns one minimal `message`. Returns
    /// `(server, ctx, store, store_root, _td)` — `_td` keeps the tempdir
    /// alive for the test body.
    async fn md139_harness() -> (
        wiremock::MockServer,
        FetchContext,
        crate::store::FsStore,
        Utf8PathBuf,
        tempfile::TempDir,
    ) {
        use crate::http::HttpClient;
        use crate::provenance::ProvenanceLog;
        use crate::rate_limiter::RateLimiter;
        use crate::store::FsStore;
        use crate::RateLimits;
        use std::sync::Arc;
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"status":"ok","message":{"title":["Example Paper"],"author":[{"given":"Ada","family":"Lovelace"}]}}"#,
            ))
            .mount(&server)
            .await;
        std::env::set_var("DOIGET_CROSSREF_BASE", server.uri());

        // wiremock serves http://127.0.0.1:PORT; the production client is
        // https_only, so the test ctx uses the allow-http test client
        // scoped to the crossref/unpaywall source keys + the wiremock host.
        let host = server
            .uri()
            .parse::<url::Url>()
            .expect("uri")
            .host_str()
            .expect("host")
            .to_string();

        let td = tempfile::TempDir::new().expect("tempdir");
        let base = Utf8Path::from_path(td.path()).expect("utf-8");
        let log_path = base.join("log.jsonl");
        let store_root = base.join("papers");
        let ctx = FetchContext {
            http: Arc::new(HttpClient::new_for_tests_allow_http_multi(&[
                ("crossref", &host),
                ("unpaywall", &host),
            ])),
            rate_limiter: Arc::new(RateLimiter::new(RateLimits::HARD_CODED)),
            log: Arc::new(
                ProvenanceLog::open(log_path, "01J0000000000000000000TEST".into())
                    .expect("provenance log"),
            ),
            session_id: "01J0000000000000000000TEST".into(),
            cache_root: None,
        };
        let store = FsStore::new(store_root.clone()).expect("fs store");
        (server, ctx, store, store_root, td)
    }

    fn metadata_dir_tomls(store_root: &Utf8Path) -> Vec<Utf8PathBuf> {
        let md = store_root.join(".metadata");
        match std::fs::read_dir(md.as_std_path()) {
            Ok(rd) => rd
                .filter_map(|e| e.ok())
                .filter_map(|e| Utf8PathBuf::from_path_buf(e.path()).ok())
                .filter(|p| p.extension() == Some("toml"))
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn metadata_only_to_store_writes_metadata_toml_139() {
        let (_server, ctx, store, store_root, _td) = md139_harness().await;
        let profile = CapabilityProfile::from_env().expect("clean env");
        let ref_ = Ref::Doi(Doi("10.1234/example".to_string()));

        let outcome = metadata_only_to_store(&ref_, &profile, &ctx, &store)
            .await
            .expect("metadata_only_to_store ok");
        assert_eq!(outcome.source, "crossref");

        let tomls = metadata_dir_tomls(&store_root);
        assert_eq!(
            tomls.len(),
            1,
            "exactly one .metadata/*.toml must be written (MCP_TOOLS.md §11 SIDE EFFECT, #139); got {tomls:?}"
        );
        let body = std::fs::read_to_string(&tomls[0]).expect("read metadata toml");
        let meta: crate::store::Metadata = toml::from_str(&body).expect("parse metadata toml");
        assert_eq!(meta.title, "Example Paper");
        assert_eq!(
            meta.doi.as_ref().map(|d| d.as_str()),
            Some("10.1234/example")
        );
        let ext = meta.doiget.expect("[doiget] table present");
        assert_eq!(ext.source, "crossref");
        assert_eq!(ext.size_bytes, 0, "metadata-only entry has no PDF");

        std::env::remove_var("DOIGET_CROSSREF_BASE");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn resolve_only_and_pure_metadata_only_write_nothing_139() {
        let (_server, ctx, _store, store_root, _td) = md139_harness().await;
        let profile = CapabilityProfile::from_env().expect("clean env");
        let ref_ = Ref::Doi(Doi("10.1234/example".to_string()));

        // resolve_only: contractually MUST NOT touch the store.
        let r = resolve_only(&ref_, &profile, &ctx)
            .await
            .expect("resolve_only ok");
        assert_eq!(r.source, "crossref");
        assert!(
            metadata_dir_tomls(&store_root).is_empty(),
            "resolve_only MUST NOT write a metadata TOML (docs/MCP_TOOLS.md §1; #139)"
        );

        // The pure metadata_only is also write-free (the store-write
        // lives only in metadata_only_to_store).
        let m = metadata_only(&ref_, &profile, &ctx)
            .await
            .expect("metadata_only ok");
        assert_eq!(m.source, "crossref");
        assert!(
            metadata_dir_tomls(&store_root).is_empty(),
            "pure metadata_only MUST NOT write to the store (#139)"
        );

        std::env::remove_var("DOIGET_CROSSREF_BASE");
    }

    /// #139 — the arXiv branch of `metadata_only_to_store` must also
    /// write the metadata TOML (different code path: Atom feed,
    /// source="arxiv", license="arxiv-default", doi=None). Review I3/C1.
    #[tokio::test]
    #[serial_test::serial]
    async fn metadata_only_to_store_arxiv_writes_metadata_toml_139() {
        use crate::http::HttpClient;
        use crate::provenance::ProvenanceLog;
        use crate::rate_limiter::RateLimiter;
        use crate::store::FsStore;
        use crate::RateLimits;
        use std::sync::Arc;
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let atom = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <entry>
    <id>http://arxiv.org/abs/2401.12345v1</id>
    <published>2024-01-15T00:00:00Z</published>
    <title>Example arXiv Paper Title</title>
    <summary>Example abstract.</summary>
    <author><name>Jane Doe</name></author>
    <category term="cs.LG" scheme="http://arxiv.org/schemas/atom"/>
  </entry>
</feed>"#;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(atom))
            .mount(&server)
            .await;
        std::env::set_var("DOIGET_ARXIV_BASE", server.uri());
        let host = server
            .uri()
            .parse::<url::Url>()
            .expect("uri")
            .host_str()
            .expect("host")
            .to_string();

        let td = tempfile::TempDir::new().expect("tempdir");
        let base = Utf8Path::from_path(td.path()).expect("utf-8");
        let store_root = base.join("papers");
        let ctx = FetchContext {
            http: Arc::new(HttpClient::new_for_tests_allow_http("arxiv", &host)),
            rate_limiter: Arc::new(RateLimiter::new(RateLimits::HARD_CODED)),
            log: Arc::new(
                ProvenanceLog::open(base.join("log.jsonl"), "01J0000000000000000000TEST".into())
                    .expect("provenance log"),
            ),
            session_id: "01J0000000000000000000TEST".into(),
            cache_root: None,
        };
        let store = FsStore::new(store_root.clone()).expect("fs store");
        let profile = CapabilityProfile::from_env().expect("clean env");
        let ref_ = Ref::Arxiv(crate::ArxivId::parse("2401.12345").expect("arxiv id"));

        let outcome = metadata_only_to_store(&ref_, &profile, &ctx, &store)
            .await
            .expect("metadata_only_to_store (arxiv) ok");
        assert_eq!(outcome.source, "arxiv");

        let tomls = metadata_dir_tomls(&store_root);
        assert_eq!(
            tomls.len(),
            1,
            "arXiv metadata-only must write one TOML; got {tomls:?}"
        );
        let meta: crate::store::Metadata =
            toml::from_str(&std::fs::read_to_string(&tomls[0]).expect("read")).expect("parse");
        assert_eq!(meta.title, "Example arXiv Paper Title");
        assert_eq!(
            meta.arxiv_id.as_ref().map(|a| a.as_str()),
            Some("2401.12345")
        );
        assert!(meta.doi.is_none(), "arXiv entry has no DOI");
        let ext = meta.doiget.expect("[doiget] table");
        assert_eq!(ext.source, "arxiv");
        assert_eq!(ext.license, "arxiv-default");

        std::env::remove_var("DOIGET_ARXIV_BASE");
    }

    // ----- pure-function unit tests for the #139 extraction helpers ----

    #[test]
    fn extract_metadata_title_handles_string_array_missing_blank() {
        use serde_json::json;
        // bare string (arXiv/Unpaywall shape)
        assert_eq!(
            extract_metadata_title(&json!({"title": "Hello"})),
            Some("Hello".to_string())
        );
        // single-element array (Crossref `message.title` in practice)
        assert_eq!(
            extract_metadata_title(&json!({"title": ["Real Title"]})),
            Some("Real Title".to_string())
        );
        // missing key -> None (caller falls back to ref id)
        assert_eq!(extract_metadata_title(&json!({"x": 1})), None);
        // blank string -> None (must not persist an empty title)
        assert_eq!(extract_metadata_title(&json!({"title": "   "})), None);
        // empty array -> None
        assert_eq!(extract_metadata_title(&json!({"title": []})), None);
        // A leading blank/whitespace array element is SKIPPED — the first
        // non-blank element is taken (a stray leading empty element must
        // not mask the real Crossref title).
        assert_eq!(
            extract_metadata_title(&json!({"title": ["  ", "Real Title"]})),
            Some("Real Title".to_string())
        );
        // all-blank array -> None (caller falls back to ref id)
        assert_eq!(extract_metadata_title(&json!({"title": ["  ", ""]})), None);
    }

    #[test]
    fn extract_metadata_authors_handles_each_resolver_shape() {
        use serde_json::json;
        // arXiv: authors: [String]
        assert_eq!(
            extract_metadata_authors(&json!({"authors": ["Jane Doe", "John Roe"]})),
            vec!["Jane Doe".to_string(), "John Roe".to_string()]
        );
        // Crossref: author: [{given,family}]
        assert_eq!(
            extract_metadata_authors(&json!({"author": [{"given": "Ada", "family": "Lovelace"}]})),
            vec!["Ada Lovelace".to_string()]
        );
        // family-only (given absent) -> trimmed, no leading space
        assert_eq!(
            extract_metadata_authors(&json!({"author": [{"family": "Onsager"}]})),
            vec!["Onsager".to_string()]
        );
        // `name` fallback when given+family both absent
        assert_eq!(
            extract_metadata_authors(&json!({"author": [{"name": "K. Wilson"}]})),
            vec!["K. Wilson".to_string()]
        );
        // z_authors fallback shape (forward-compat branch)
        assert_eq!(
            extract_metadata_authors(&json!({"z_authors": [{"given": "L", "family": "Kadanoff"}]})),
            vec!["L Kadanoff".to_string()]
        );
        // nothing parseable -> empty (still a valid TOML)
        assert!(extract_metadata_authors(&json!({"x": 1})).is_empty());
        assert!(extract_metadata_authors(&json!({"authors": []})).is_empty());
    }
}

// ---------------------------------------------------------------------------
// Source-attempt trace (#413 follow-up)
// ---------------------------------------------------------------------------

/// Why a given optional source did or did not contribute.
///
/// The distinction this type exists to make: **"we asked and it had
/// nothing" is not the same failure as "we never asked".** Before this,
/// both looked identical from outside — a DOI fetch that failed returned
/// the Crossref error and said nothing about the rest of the chain, so a
/// user could not tell whether HAL had been consulted and come up empty,
/// or whether `DOIGET_ENABLE_HAL` was simply unset. One means the paper is
/// not there; the other means you have not turned the source on. They need
/// completely different actions.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AttemptOutcome {
    /// Runtime flag unset — **not consulted**. Carries the env var so the
    /// message can name the exact thing the user has to change.
    Disabled {
        /// Every variable that has to be set, e.g.
        /// `["DOIGET_ENABLE_HAL"]` or
        /// `["DOIGET_KEY_APS", "DOIGET_AGREE_TDM_APS"]`.
        ///
        /// A list rather than a string because Tier 3 needs two, and
        /// joining them into `"A + B"` put a separator on the #459 wire
        /// that a consumer would have had to split on — which is the
        /// thing the `detail()` / `wire()` split exists to avoid (#470).
        env: &'static [&'static str],
    },
    /// This source cannot serve this kind of ref at all (e.g. an arXiv id
    /// handed to a DOI-only resolver). Not a misconfiguration.
    NotApplicable,
    /// **Not consulted.** A publisher-specific Tier-3 source was asked
    /// about a DOI its publisher did not register (#442).
    ///
    /// Distinct from [`Self::Disabled`] on purpose: the credentials are
    /// fine and there is nothing for the user to switch on. Telling them
    /// to set `DOIGET_KEY_APS` because an Elsevier DOI did not resolve
    /// would send them after the wrong problem.
    WrongPublisher {
        /// e.g. `"DOI prefix 10.1016 is not American Physical Society (APS)"`.
        detail: String,
    },
    /// An earlier source in the chain already answered, so this one was
    /// deliberately skipped. Not a failure.
    NotNeeded,
    /// **Consulted.** The source has no record for this ref.
    NoRecord,
    /// **Consulted.** A record exists but is not open access — the source
    /// knows the paper and still cannot give it to us.
    NotOpenAccess {
        /// Source-specific detail, e.g. the access-rights code observed.
        detail: String,
    },
    /// **Consulted**, and refused by a policy control with a structured
    /// reason: a redirect off the allowlist, an insecure redirect, an
    /// oversized body, a not-a-PDF (ADR-0023).
    ///
    /// Distinct from [`Self::Failed`] because the [`DenialContext`] is what
    /// [`crate::remediation::for_denial`] consumes. `PdfLegStatus::Blocked`
    /// kept it end to end and the MCP layer turned it into a remediation;
    /// per-source rows flattened the same information to prose, so the
    /// richest and most actionable case degraded to text on a wire that
    /// #459 advertises as machine-readable (#470).
    Denied {
        /// The structured denial, verbatim.
        denial: DenialContext,
    },
    /// **Consulted.** The request itself failed (transport, auth, schema).
    Failed {
        /// Rendered error.
        detail: String,
    },
    /// **Consulted**, and it answered.
    Resolved,
}

impl AttemptOutcome {
    /// Whether a request actually went out for this source.
    ///
    /// This is the predicate the reachability tests assert on: a source
    /// reporting `was_consulted() == false` is one the production path
    /// never reached, which is exactly the condition that used to be
    /// invisible.
    #[must_use]
    pub fn was_consulted(&self) -> bool {
        matches!(
            self,
            Self::NoRecord
                | Self::NotOpenAccess { .. }
                | Self::Denied { .. }
                | Self::Failed { .. }
                | Self::Resolved
        )
    }

    /// Stable machine token for this outcome (#459).
    ///
    /// [`Self::render`] is prose and may be reworded; this is the thing a
    /// consumer branches on. Kept separate for that reason — the CLI has
    /// already reworded the trace twice (#413, #438) and a caller keying
    /// off the sentence would have broken both times.
    ///
    /// The two halves of the vocabulary mirror [`Self::was_consulted`]:
    /// `not_consulted_*` means no request went out, `consulted_*` means one
    /// did. That distinction is the entire reason this type exists.
    #[must_use]
    pub fn wire(&self) -> &'static str {
        match self {
            Self::Disabled { .. } => "not_consulted_disabled",
            Self::NotApplicable => "not_consulted_not_applicable",
            Self::WrongPublisher { .. } => "not_consulted_wrong_publisher",
            Self::NotNeeded => "not_consulted_not_needed",
            Self::NoRecord => "consulted_no_record",
            Self::NotOpenAccess { .. } => "consulted_not_open_access",
            Self::Denied { .. } => "consulted_denied",
            Self::Failed { .. } => "consulted_failed",
            Self::Resolved => "consulted_resolved",
        }
    }

    /// The variant's free-text payload, when it has one.
    ///
    /// Carried separately from [`Self::wire`] so a consumer gets the
    /// actionable specifics — which env var, which prefix, which error —
    /// without parsing them back out of the rendered sentence.
    #[must_use]
    pub fn detail(&self) -> Option<&str> {
        match self {
            // `Disabled` and `Denied` carry structure, not a string.
            // See `required_env` and `denial`; `attempts_to_value` renders
            // both, and still emits a joined `detail` for each so the #459
            // wire stays backwards compatible.
            Self::Disabled { .. } | Self::Denied { .. } => None,
            Self::WrongPublisher { detail }
            | Self::NotOpenAccess { detail }
            | Self::Failed { detail } => Some(detail),
            _ => None,
        }
    }

    /// The variables a `Disabled` row needs set, in the order the user
    /// should set them. `None` for every other outcome.
    #[must_use]
    pub fn required_env(&self) -> Option<&'static [&'static str]> {
        match self {
            Self::Disabled { env } => Some(env),
            _ => None,
        }
    }

    /// The structured denial behind a `Denied` row, which is what
    /// [`crate::remediation::for_denial`] takes. `None` otherwise.
    #[must_use]
    pub fn denial(&self) -> Option<&DenialContext> {
        match self {
            Self::Denied { denial } => Some(denial),
            _ => None,
        }
    }

    /// One-line rendering, phrased so consulted and not-consulted cannot
    /// be misread for one another.
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            Self::Disabled { env } => {
                format!("not consulted (set {} to enable)", env.join(" + "))
            }
            Self::NotApplicable => "not consulted (cannot serve this ref kind)".to_string(),
            Self::WrongPublisher { detail } => format!("not consulted ({detail})"),
            Self::NotNeeded => "not consulted (an earlier source answered)".to_string(),
            Self::NoRecord => "consulted: no record".to_string(),
            Self::NotOpenAccess { detail } => {
                format!("consulted: found, not open access ({detail})")
            }
            // `{:?}` on the reason: `render` is explicitly prose (`wire`
            // is the stable token), and the attempted host is the part a
            // human acts on.
            Self::Denied { denial } => match &denial.attempted {
                Some(a) => format!("consulted: refused ({:?}, {a})", denial.reason),
                None => format!("consulted: refused ({:?})", denial.reason),
            },
            Self::Failed { detail } => format!("consulted: failed ({detail})"),
            Self::Resolved => "consulted: resolved".to_string(),
        }
    }
}

/// One row of the resolution trace.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SourceAttempt {
    /// Source key, matching [`crate::source::Source::name`].
    pub source: &'static str,
    /// What happened.
    pub outcome: AttemptOutcome,
}

impl SourceAttempt {
    /// Construct a row.
    #[must_use]
    pub fn new(source: &'static str, outcome: AttemptOutcome) -> Self {
        Self { source, outcome }
    }
}

/// The trace as JSON, for the machine-readable surfaces (#459).
///
/// One object per source: `{ source, outcome, detail?, consulted }`.
///
/// `consulted` is redundant with `outcome` and present anyway. It is the
/// single question every consumer of this array actually has — "did anyone
/// else look?" — and making them memorise which of eight tokens implies it
/// invites the exact confusion the type was introduced to end.
///
/// Built here rather than by `#[derive(Serialize)]` on the types: both are
/// `#[non_exhaustive]` public API, and deriving would make every future
/// variant a wire change by default instead of by decision.
#[must_use]
pub fn attempts_to_value(attempts: &[SourceAttempt]) -> serde_json::Value {
    serde_json::Value::Array(
        attempts
            .iter()
            .map(|a| {
                let mut o = serde_json::Map::new();
                o.insert("source".into(), serde_json::json!(a.source));
                o.insert("outcome".into(), serde_json::json!(a.outcome.wire()));
                if let Some(d) = a.outcome.detail() {
                    o.insert("detail".into(), serde_json::json!(d));
                }
                if let Some(env) = a.outcome.required_env() {
                    // `detail` stays a joined string so a #459-era consumer
                    // reads the same field it always did; `required_env` is
                    // the form that does not need splitting.
                    o.insert("detail".into(), serde_json::json!(env.join(" + ")));
                    o.insert("required_env".into(), serde_json::json!(env));
                }
                if let Some(dc) = a.outcome.denial() {
                    o.insert("detail".into(), serde_json::json!(a.outcome.render()));
                    o.insert("denial_context".into(), serde_json::json!(dc));
                    let rem = crate::remediation::for_denial(dc);
                    if !rem.is_empty() {
                        o.insert("remediation".into(), serde_json::json!(rem));
                    }
                }
                o.insert(
                    "consulted".into(),
                    serde_json::json!(a.outcome.was_consulted()),
                );
                serde_json::Value::Object(o)
            })
            .collect(),
    )
}

/// Render a whole trace as an `= note:`-style block.
///
/// Ordered as the chain ran, so reading top to bottom tells you how far
/// resolution actually got.
#[must_use]
pub fn render_attempts(attempts: &[SourceAttempt]) -> String {
    attempts
        .iter()
        .map(|a| format!("  {:<12} {}", a.source, a.outcome.render()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// True when no source in the trace was actually reached.
///
/// Distinguishes "this DOI is genuinely not findable" from "nothing was
/// even switched on" — a data problem versus a configuration problem.
#[must_use]
pub fn nothing_was_consulted(attempts: &[SourceAttempt]) -> bool {
    !attempts.is_empty() && attempts.iter().all(|a| !a.outcome.was_consulted())
}

/// Map a source's terminal error onto an [`AttemptOutcome`].
///
/// Keeps the classification in one place so every source in the chain
/// reports the same way: a clean miss must not be recorded as a failure,
/// and an access refusal must not be recorded as a miss.
#[cfg(any(
    feature = "metadata",
    feature = "tdm-elsevier",
    feature = "tdm-aps",
    feature = "tdm-springer",
    feature = "tdm-ieee"
))]
fn classify_attempt(e: &FetchError) -> AttemptOutcome {
    match e {
        FetchError::NotFound { .. } => AttemptOutcome::NoRecord,
        // Sources signal "found it, cannot give it to you" through
        // SourceSchema with an explicit hint (OpenAIRE access rights,
        // Europe PMC isOpenAccess, HAL openAccess_bool). The hint is
        // carried verbatim so the reason survives into the message.
        FetchError::SourceSchema { hint } if is_access_refusal(hint) => {
            AttemptOutcome::NotOpenAccess {
                detail: hint.clone(),
            }
        }
        // A policy refusal before the untyped fallback (#470). The
        // conversion already exists and yields exactly the reason /
        // attempted / expected / hop_index that `remediation::for_denial`
        // consumes; `classify_attempt` used to walk straight past it and
        // stringify.
        //
        // `CapabilityNotGranted` is deliberately excluded. It is produced
        // by a source's defensive gate BEFORE any request goes out, and
        // `Denied` reports `was_consulted() == true` — which is the exact
        // predicate the #442 reachability tests assert on. Routing it here
        // would make a source that was never contacted claim it was. The
        // orchestrator already reports that state as `Disabled`, which
        // also names the variables to set.
        other => match Option::<DenialContext>::from(other) {
            Some(denial) if denial.reason != crate::DenialReason::CapabilityNotGranted => {
                AttemptOutcome::Denied { denial }
            }
            _ => AttemptOutcome::Failed {
                detail: other.to_string(),
            },
        },
    }
}

// Gated exactly as `classify_attempt` is, not more narrowly. Gating these
// on `metadata` alone left the new code compiled but untested under the
// coverage job's feature set (`tdm-*` without `metadata`) -- a smaller copy
// of the same "is this actually exercised?" question #442/#458 are about,
// and the reason `codecov/patch` failed on this PR.
#[cfg(all(
    test,
    any(
        feature = "metadata",
        feature = "tdm-elsevier",
        feature = "tdm-aps",
        feature = "tdm-springer",
        feature = "tdm-ieee"
    )
))]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod attempt_denial_tests {
    use super::*;

    use crate::http::HttpError;
    use crate::DenialReason;

    fn redirect_denial() -> FetchError {
        FetchError::Http(HttpError::RedirectDenied {
            source_key: "hal".to_string(),
            host: "cdn.example.org".to_string(),
            expected_hosts: vec!["hal.science".to_string()],
        })
    }

    /// #470. A redirect denial on a metadata-chain source used to flatten
    /// to `Failed { detail: "..." }` -- an untyped string on a wire #459
    /// advertises as machine-readable, for the one case that is richest and
    /// most actionable.
    #[test]
    fn a_policy_refusal_keeps_its_denial_context() {
        let outcome = classify_attempt(&redirect_denial());
        let denial = outcome
            .denial()
            .expect("a redirect denial must survive classification");
        assert_eq!(denial.reason, DenialReason::RedirectNotInAllowlist);
        assert_eq!(denial.attempted.as_deref(), Some("cdn.example.org"));
        assert_eq!(outcome.wire(), "consulted_denied");
        assert!(
            outcome.was_consulted(),
            "a refusal means a request went out"
        );
    }

    /// The point of keeping it: `remediation::for_denial` becomes reachable
    /// from a per-source row, so the row can tell the user what to change.
    /// `PdfLegStatus::Blocked` could already do this; the rows could not.
    #[test]
    fn a_denied_row_carries_a_remediation_on_the_wire() {
        let attempts = vec![SourceAttempt::new(
            "hal",
            classify_attempt(&redirect_denial()),
        )];
        let v = attempts_to_value(&attempts);
        let row = &v[0];

        assert_eq!(row["outcome"], serde_json::json!("consulted_denied"));
        assert_eq!(
            row["denial_context"]["reason"],
            serde_json::json!("redirect_not_in_allowlist"),
            "row: {row}"
        );
        let rem = row["remediation"]
            .as_array()
            .unwrap_or_else(|| panic!("a redirect denial has a config channel; row: {row}"));
        assert!(!rem.is_empty());
        assert!(
            row["detail"].is_string(),
            "`detail` must stay populated for a #459-era consumer; row: {row}"
        );
    }

    /// `CapabilityNotGranted` is produced BEFORE any request goes out, so
    /// routing it to `Denied` would make a source that was never contacted
    /// report `was_consulted() == true` -- the predicate the #442
    /// reachability tests rest on.
    #[test]
    fn an_ungranted_capability_is_not_reported_as_consulted_and_denied() {
        let outcome = classify_attempt(&FetchError::NotEligible {
            source_key: "tdm-aps".into(),
        });
        assert!(
            outcome.denial().is_none(),
            "got {outcome:?}: this never reached the network"
        );
        assert_eq!(outcome.wire(), "consulted_failed");
    }

    /// The accessors are a narrowing, so the negative case matters as much
    /// as the positive one: a caller that treated `denial()` as
    /// "is this a failure" would mishandle every `Failed` row.
    #[test]
    fn the_accessors_narrow_rather_than_generalise() {
        let failed = AttemptOutcome::Failed {
            detail: "connection reset".to_string(),
        };
        assert!(failed.denial().is_none());
        assert!(failed.required_env().is_none());
        assert!(failed.detail().is_some());

        let disabled = AttemptOutcome::Disabled {
            env: &["DOIGET_ENABLE_HAL"],
        };
        assert!(disabled.denial().is_none());
        assert!(
            disabled.detail().is_none(),
            "`Disabled` carries structure; the joined string is built at the wire"
        );
        assert!(!disabled.was_consulted());
        assert_eq!(
            disabled.render(),
            "not consulted (set DOIGET_ENABLE_HAL to enable)"
        );
    }

    /// `attempted` is optional on a `DenialContext`, and the size cap has
    /// no host to name. The prose has to hold either way.
    #[test]
    fn a_denial_without_an_attempted_host_still_renders() {
        let outcome = AttemptOutcome::Denied {
            denial: DenialContext {
                reason: DenialReason::SizeCapExceeded,
                source: Some("core".to_string()),
                attempted: None,
                expected: None,
                hop_index: None,
                cap: None,
                actual: None,
            },
        };
        assert_eq!(outcome.render(), "consulted: refused (SizeCapExceeded)");

        // No config channel for a size cap, so no remediation key -- as
        // opposed to an empty array, which would read as "we looked and
        // there is nothing you can do" without saying so.
        let v = attempts_to_value(&[SourceAttempt::new("core", outcome)]);
        assert!(v[0].get("remediation").is_none(), "row: {}", v[0]);
        assert!(v[0].get("denial_context").is_some(), "row: {}", v[0]);
    }

    /// #470's second half. Tier 3 needs two variables, and joining them
    /// into `"A + B"` meant a consumer had to split on the separator --
    /// exactly what the `detail()` / `wire()` split exists to avoid.
    #[test]
    fn a_disabled_row_lists_its_variables_instead_of_joining_them() {
        let attempts = vec![SourceAttempt::new(
            "tdm-aps",
            AttemptOutcome::Disabled {
                env: &["DOIGET_KEY_APS", "DOIGET_AGREE_TDM_APS"],
            },
        )];
        let v = attempts_to_value(&attempts);
        let row = &v[0];

        assert_eq!(
            row["required_env"],
            serde_json::json!(["DOIGET_KEY_APS", "DOIGET_AGREE_TDM_APS"]),
            "row: {row}"
        );
        // Unchanged for anyone already reading it.
        assert_eq!(
            row["detail"],
            serde_json::json!("DOIGET_KEY_APS + DOIGET_AGREE_TDM_APS"),
            "row: {row}"
        );
    }
}

/// Whether a `SourceSchema` hint describes an access refusal rather than a
/// malformed response.
#[cfg(any(
    feature = "metadata",
    feature = "tdm-elsevier",
    feature = "tdm-aps",
    feature = "tdm-springer",
    feature = "tdm-ieee"
))]
fn is_access_refusal(hint: &str) -> bool {
    hint.contains("not open access") || hint.contains("openAccess")
}

/// Run the optional resolution chain and record one [`SourceAttempt`] per
/// source, consulted or not.
///
/// Order is #413's priority order: DataCite (resolution for the second
/// registration agency) first, then the OA aggregators, then CORE as the
/// broadest and therefore last fallback. Each is skipped — with a recorded
/// reason — once something has answered, so the common case costs at most
/// one extra request.
///
/// `extracted` is overwritten by the first source that answers, so the
/// caller sees the bibliographic fields of whichever source resolved.
#[cfg(feature = "metadata")]
async fn resolve_optional_chain(
    ref_: &Ref,
    profile: &CapabilityProfile,
    ctx: &FetchContext,
    crossref_answered: bool,
    extracted: &mut CrossrefFields,
    attempts: &mut Vec<SourceAttempt>,
) -> Option<(&'static str, Value)> {
    // Built eagerly so the trace lists every source in a fixed order even
    // when none is consulted — an empty trace would be indistinguishable
    // from "no chain exists", which is the confusion this whole mechanism
    // is here to remove.
    // Base URLs come from the env, mirroring `crossref_source_from_env`
    // and friends. Without this the chain could only ever talk to
    // production, which is how it shipped unreachable in the first place:
    // no test could point it anywhere, so no test could prove it was
    // reached.
    let datacite = optional_base("DOIGET_DATACITE_BASE").map_or_else(
        crate::sources::datacite::DataCiteSource::new,
        crate::sources::datacite::DataCiteSource::with_base,
    );
    let epmc = optional_base("DOIGET_EUROPE_PMC_BASE").map_or_else(
        crate::sources::europepmc::EuropePmcSource::new,
        crate::sources::europepmc::EuropePmcSource::with_base,
    );
    let openaire = optional_base("DOIGET_OPENAIRE_BASE").map_or_else(
        crate::sources::openaire::OpenAireSource::new,
        crate::sources::openaire::OpenAireSource::with_base,
    );
    let hal = optional_base("DOIGET_HAL_BASE").map_or_else(
        crate::sources::hal::HalSource::new,
        crate::sources::hal::HalSource::with_base,
    );
    let core = optional_base("DOIGET_CORE_BASE").map_or_else(
        crate::sources::core_oa::CoreSource::new,
        crate::sources::core_oa::CoreSource::with_base,
    );

    // The name is carried explicitly rather than read from `src.name()`:
    // that borrows the source, while a `SourceAttempt` holds a `'static`
    // key so the trace can outlive the chain. A test asserts the two agree,
    // so this cannot drift into a lie.
    // `&'static [&'static str]` rather than a single var: `AttemptOutcome::
    // Disabled` now carries the whole list, because Tier 3 needs two and a
    // joined string put a separator on the #459 wire (#470). Tier 2 needs
    // one, which is a one-element list.
    let chain: Vec<(
        &'static str,
        &'static [&'static str],
        &dyn crate::source::Source,
    )> = vec![
        ("datacite", &["DOIGET_ENABLE_DATACITE"], &datacite),
        ("europe-pmc", &["DOIGET_ENABLE_EUROPE_PMC"], &epmc),
        ("openaire", &["DOIGET_ENABLE_OPENAIRE"], &openaire),
        ("hal", &["DOIGET_ENABLE_HAL"], &hal),
        ("core", &["DOIGET_ENABLE_CORE"], &core),
    ];

    let mut resolved: Option<(&'static str, Value)> = None;

    for (name, env, src) in chain {
        debug_assert_eq!(name, src.name(), "chain name must match Source::name");

        // Crossref already answered: nothing in this chain is needed.
        if crossref_answered || resolved.is_some() {
            attempts.push(SourceAttempt::new(name, AttemptOutcome::NotNeeded));
            continue;
        }
        // `can_serve` folds two very different reasons together, so split
        // them: a disabled flag is a configuration problem the user can
        // fix; an inapplicable ref kind is not.
        if !src.can_serve(profile, ref_) {
            let outcome = if matches!(ref_, Ref::Doi(_)) {
                AttemptOutcome::Disabled { env }
            } else {
                AttemptOutcome::NotApplicable
            };
            attempts.push(SourceAttempt::new(name, outcome));
            continue;
        }

        match src.fetch(ref_, profile, ctx).await {
            Ok(r) => {
                if let Some(meta) = r.metadata_json.as_ref() {
                    *extracted = extract_optional_fields(name, meta);
                }
                attempts.push(SourceAttempt::new(name, AttemptOutcome::Resolved));
                resolved = r.metadata_json.map(|m| (name, m));
            }
            Err(e) => {
                tracing::debug!(source = name, error = %e, "optional source did not resolve");
                attempts.push(SourceAttempt::new(name, classify_attempt(&e)));
            }
        }
    }
    resolved
}

/// Run the Tier-3 TDM chain and record one [`SourceAttempt`] per
/// compiled-in publisher, consulted or not.
///
/// #442: these sources shipped with no caller at all. Implemented,
/// feature-gated, allowlisted, tested — and unreachable, so satisfying
/// all three documented gates changed nothing. This is the caller.
///
/// Separate from [`resolve_optional_chain`] rather than folded into it,
/// for two reasons. The `tdm-*` features are independent of `metadata`
/// (ADR-0002), so a `--features tdm-aps` build must reach its source
/// without dragging in the Tier-2 chain. And the gate semantics differ:
/// Tier 2 is one `DOIGET_ENABLE_*` flag, Tier 3 is a key plus a
/// recorded agreement.
///
/// Ordered before the Tier-2 OA aggregators: for a DOI its publisher
/// registered, the publisher's own API is the authoritative record, and
/// the user paid and agreed specifically to use it.
///
/// Like the Tier-2 chain this runs strictly AFTER Crossref and only when
/// Crossref produced nothing, so enabling a TDM source can never change
/// a resolution that already works.
///
/// `CrossrefFields` are deliberately NOT extracted here. Each publisher
/// returns its own shape and guessing a mapping risks a wrong title,
/// which is worse than a missing one — the same stance the OA
/// aggregators take. The payload is returned as `metadata_json` only.
#[cfg(any(
    feature = "tdm-elsevier",
    feature = "tdm-aps",
    feature = "tdm-springer",
    feature = "tdm-ieee"
))]
// `chain` is built by `#[cfg]`-gated pushes rather than a `vec![]`
// literal: an attribute per element inside a vec literal is not
// expressible, and which entries exist depends on the feature set.
#[allow(clippy::vec_init_then_push)]
async fn resolve_tdm_chain(
    ref_: &Ref,
    profile: &CapabilityProfile,
    ctx: &FetchContext,
    crossref_answered: bool,
    attempts: &mut Vec<SourceAttempt>,
) -> Option<Value> {
    struct Entry<'a> {
        name: &'static str,
        /// What the user must set, rendered verbatim into the trace.
        enable_hint: &'static [&'static str],
        /// DOI prefixes this publisher registered.
        prefixes: &'static [&'static str],
        /// Human name, for the wrong-publisher message.
        publisher: &'static str,
        src: &'a dyn crate::source::Source,
    }

    // Base overrides mirror `crossref_source_from_env` and the Tier-2
    // chain. Tier 3 had none, which is precisely why no test could point
    // one of these anywhere and therefore why none could prove reach.
    #[cfg(feature = "tdm-aps")]
    let aps = optional_base("DOIGET_APS_BASE").map_or_else(
        crate::sources::tdm_aps::TdmApsSource::new,
        crate::sources::tdm_aps::TdmApsSource::with_base,
    );
    #[cfg(feature = "tdm-elsevier")]
    let elsevier = optional_base("DOIGET_ELSEVIER_BASE").map_or_else(
        crate::sources::tdm_elsevier::TdmElsevierSource::new,
        crate::sources::tdm_elsevier::TdmElsevierSource::with_base,
    );
    #[cfg(feature = "tdm-springer")]
    let springer = optional_base("DOIGET_SPRINGER_BASE").map_or_else(
        crate::sources::tdm_springer::TdmSpringerSource::new,
        crate::sources::tdm_springer::TdmSpringerSource::with_base,
    );
    #[cfg(feature = "tdm-ieee")]
    let ieee = optional_base("DOIGET_IEEE_BASE").map_or_else(
        crate::sources::tdm_ieee::TdmIeeeSource::new,
        crate::sources::tdm_ieee::TdmIeeeSource::with_base,
    );

    // Not a `vec![]` literal: each entry is `#[cfg]`-gated on its own
    // publisher feature, and attribute-per-element inside a vec literal
    // is not expressible.
    #[allow(unused_mut)]
    let mut chain: Vec<Entry<'_>> = Vec::new();
    #[cfg(feature = "tdm-aps")]
    chain.push(Entry {
        name: "tdm-aps",
        enable_hint: &["DOIGET_KEY_APS", "DOIGET_AGREE_TDM_APS"],
        prefixes: crate::sources::tdm_aps::PUBLISHER_PREFIXES,
        publisher: "American Physical Society (APS)",
        src: &aps,
    });
    #[cfg(feature = "tdm-elsevier")]
    chain.push(Entry {
        name: "tdm-elsevier",
        enable_hint: &["DOIGET_KEY_ELSEVIER", "DOIGET_AGREE_TDM_ELSEVIER"],
        prefixes: crate::sources::tdm_elsevier::PUBLISHER_PREFIXES,
        publisher: "Elsevier BV",
        src: &elsevier,
    });
    #[cfg(feature = "tdm-springer")]
    chain.push(Entry {
        name: "tdm-springer",
        enable_hint: &["DOIGET_KEY_SPRINGER", "DOIGET_AGREE_TDM_SPRINGER"],
        prefixes: crate::sources::tdm_springer::PUBLISHER_PREFIXES,
        publisher: "Springer Nature",
        src: &springer,
    });
    #[cfg(feature = "tdm-ieee")]
    chain.push(Entry {
        name: "tdm-ieee",
        enable_hint: &["DOIGET_KEY_IEEE", "DOIGET_AGREE_TDM_IEEE"],
        prefixes: crate::sources::tdm_ieee::PUBLISHER_PREFIXES,
        publisher: "IEEE",
        src: &ieee,
    });

    let mut resolved: Option<Value> = None;

    for e in chain {
        debug_assert_eq!(e.name, e.src.name(), "chain name must match Source::name");

        if crossref_answered || resolved.is_some() {
            attempts.push(SourceAttempt::new(e.name, AttemptOutcome::NotNeeded));
            continue;
        }
        let Ref::Doi(doi) = ref_ else {
            attempts.push(SourceAttempt::new(e.name, AttemptOutcome::NotApplicable));
            continue;
        };
        // Prefix BEFORE credentials. A DOI this publisher never
        // registered is not a configuration problem, and reporting it as
        // one would tell the user to go find an API key that would not
        // have helped.
        if !e.prefixes.contains(&doi.prefix()) {
            attempts.push(SourceAttempt::new(
                e.name,
                AttemptOutcome::WrongPublisher {
                    detail: format!("DOI prefix {} is not {}", doi.prefix(), e.publisher),
                },
            ));
            continue;
        }
        if !e.src.can_serve(profile, ref_) {
            attempts.push(SourceAttempt::new(
                e.name,
                AttemptOutcome::Disabled { env: e.enable_hint },
            ));
            continue;
        }

        match e.src.fetch(ref_, profile, ctx).await {
            Ok(r) => {
                attempts.push(SourceAttempt::new(e.name, AttemptOutcome::Resolved));
                resolved = r.metadata_json;
            }
            Err(err) => {
                tracing::debug!(source = e.name, error = %err, "TDM source did not resolve");
                attempts.push(SourceAttempt::new(e.name, classify_attempt(&err)));
            }
        }
    }
    resolved
}

/// Read a `DOIGET_*_BASE` override, warning rather than failing on a
/// malformed value — a bad override must not take the source offline.
#[cfg(any(
    feature = "metadata",
    feature = "tdm-elsevier",
    feature = "tdm-aps",
    feature = "tdm-springer",
    feature = "tdm-ieee"
))]
fn optional_base(env: &str) -> Option<url::Url> {
    let raw = std::env::var(env).ok()?;
    match url::Url::parse(&raw) {
        Ok(u) => Some(u),
        Err(e) => {
            tracing::warn!(value = %raw, error = %e, env, "base override is not a valid URL; using the default");
            None
        }
    }
}

/// Map an optional source's payload onto [`CrossrefFields`].
///
/// Only DataCite has a documented field mapping today
/// ([`extract_datacite_fields`]); the OA aggregators return
/// source-specific shapes whose bibliographic mapping is separate work.
/// Rather than guess, those keep whatever Crossref produced and contribute
/// their payload as `metadata_json` only — an empty title is a visible
/// gap, a wrong title is a silent corruption.
#[cfg(feature = "metadata")]
fn extract_optional_fields(source: &str, meta: &Value) -> CrossrefFields {
    match source {
        "datacite" => extract_datacite_fields(meta),
        _ => CrossrefFields::default(),
    }
}

// ---------------------------------------------------------------------------
// #413: chain reachability + "never asked" vs "asked and empty"
// ---------------------------------------------------------------------------
//
// These tests exist because four of the five sources added in 0.8.8 were
// initially unreachable: implemented, capability-gated, allowlisted — and
// never called by anything. Every unit test passed, because each one drove
// its own `Source` impl directly. Nothing asserted that the PRODUCTION
// path reaches them.
//
// So the assertions here are deliberately about reach, not behaviour:
// `server.received_requests()` is the only evidence that a source was
// actually consulted, and the attempt trace is the only way an operator
// can tell "consulted and empty" from "never consulted".

#[cfg(all(test, feature = "metadata"))]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod chain_tests {
    use super::*;

    use std::sync::Arc;

    use camino::Utf8PathBuf;
    use tempfile::TempDir;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::http::HttpClient;
    use crate::provenance::ProvenanceLog;
    use crate::rate_limiter::RateLimiter;
    use crate::{CapabilityProfile, Doi, MetadataAccess, RateLimits, Ref};

    /// A context whose HTTP client points EVERY optional source key at the
    /// same mock server, so "was this source reached?" is answerable by
    /// asking the server what it received.
    /// Point every optional source at the mock. Restored on drop; these
    /// tests are serialised because the vars are process-global.
    struct BaseGuard(Vec<(&'static str, Option<String>)>);
    impl BaseGuard {
        fn to(uri: &str) -> Self {
            const VARS: &[&str] = &[
                "DOIGET_DATACITE_BASE",
                "DOIGET_EUROPE_PMC_BASE",
                "DOIGET_OPENAIRE_BASE",
                "DOIGET_HAL_BASE",
                "DOIGET_CORE_BASE",
            ];
            Self(
                VARS.iter()
                    .map(|v| {
                        let old = std::env::var(v).ok();
                        std::env::set_var(v, uri);
                        (*v, old)
                    })
                    .collect(),
            )
        }
    }
    impl Drop for BaseGuard {
        fn drop(&mut self) {
            for (v, old) in &self.0 {
                match old {
                    Some(o) => std::env::set_var(v, o),
                    None => std::env::remove_var(v),
                }
            }
        }
    }

    fn ctx_for(host: &str) -> (TempDir, FetchContext) {
        let td = TempDir::new().expect("tempdir");
        let dir = Utf8PathBuf::try_from(td.path().to_path_buf()).expect("utf-8");
        let http = Arc::new(HttpClient::new_for_tests_allow_http_multi(&[
            ("datacite", host),
            ("europe-pmc", host),
            ("openaire", host),
            ("hal", host),
            ("core", host),
        ]));
        let session_id = "01J0000000000000000000TEST".to_string();
        let log = Arc::new(
            ProvenanceLog::open(dir.join("t.jsonl"), session_id.clone()).expect("log opens"),
        );
        (
            td,
            FetchContext {
                http,
                rate_limiter: Arc::new(RateLimiter::new(RateLimits::HARD_CODED)),
                log,
                session_id,
                cache_root: None,
            },
        )
    }

    fn all_off() -> CapabilityProfile {
        let mut p = CapabilityProfile::for_tests();
        p.metadata = MetadataAccess {
            openalex: false,
            semantic_scholar: false,
            doaj: false,
            datacite: false,
            hal: false,
            openaire: false,
            core: false,
            europe_pmc: false,
        };
        p
    }

    fn all_on() -> CapabilityProfile {
        let mut p = all_off();
        p.metadata.datacite = true;
        p.metadata.hal = true;
        p.metadata.openaire = true;
        p.metadata.core = true;
        p.metadata.europe_pmc = true;
        p
    }

    fn outcome<'a>(attempts: &'a [SourceAttempt], name: &str) -> &'a AttemptOutcome {
        &attempts
            .iter()
            .find(|a| a.source == name)
            .unwrap_or_else(|| panic!("no attempt recorded for {name}; got {attempts:?}"))
            .outcome
    }

    /// THE regression for the bug this whole change exists to fix.
    ///
    /// With every flag on and Crossref having produced nothing, all five
    /// sources must actually be REACHED — proven by the mock server having
    /// received five requests, not by any assertion about return values.
    #[tokio::test]
    #[serial_test::serial]
    async fn every_optional_source_is_actually_reached_by_the_production_chain() {
        let server = MockServer::start().await;
        let _bases = BaseGuard::to(&server.uri());
        // Every source gets a syntactically valid but empty answer, so the
        // chain runs to the end instead of stopping at the first hit.
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"results":[],"response":{"docs":[]},"resultList":{"result":[]}}"#,
            ))
            .mount(&server)
            .await;

        let (_td, ctx) = ctx_for(&server.address().to_string());
        let ref_ = Ref::Doi(Doi::parse("10.1234/example").expect("doi"));
        let mut fields = CrossrefFields::default();
        let mut attempts = Vec::new();

        resolve_optional_chain(&ref_, &all_on(), &ctx, false, &mut fields, &mut attempts).await;

        let names: Vec<&str> = attempts.iter().map(|a| a.source).collect();
        assert_eq!(
            names,
            vec!["datacite", "europe-pmc", "openaire", "hal", "core"],
            "the trace must list every source, in chain order"
        );
        for a in &attempts {
            assert!(
                a.outcome.was_consulted(),
                "{} was NOT reached by the production chain: {:?}",
                a.source,
                a.outcome
            );
        }
        assert_eq!(
            server.received_requests().await.expect("recorded").len(),
            5,
            "each enabled source must issue exactly one request"
        );
    }

    /// The distinction the trace exists for: flags off means **no request
    /// is made at all**, and the trace says so in a way that cannot be
    /// confused with an empty result.
    #[tokio::test]
    #[serial_test::serial]
    async fn flags_off_means_never_consulted_and_says_which_var_to_set() {
        let server = MockServer::start().await;
        let _bases = BaseGuard::to(&server.uri());
        let (_td, ctx) = ctx_for(&server.address().to_string());
        let ref_ = Ref::Doi(Doi::parse("10.1234/example").expect("doi"));
        let mut fields = CrossrefFields::default();
        let mut attempts = Vec::new();

        resolve_optional_chain(&ref_, &all_off(), &ctx, false, &mut fields, &mut attempts).await;

        assert!(
            server
                .received_requests()
                .await
                .expect("recorded")
                .is_empty(),
            "a disabled chain must make NO request"
        );
        assert!(
            nothing_was_consulted(&attempts),
            "the trace must report that nothing was reached"
        );
        assert_eq!(
            outcome(&attempts, "hal"),
            &AttemptOutcome::Disabled {
                env: &["DOIGET_ENABLE_HAL"]
            },
            "a disabled source must name the variable that enables it"
        );
        let rendered = render_attempts(&attempts);
        assert!(
            rendered.contains("not consulted (set DOIGET_ENABLE_HAL to enable)"),
            "rendered trace must be actionable; got:\n{rendered}"
        );
    }

    /// The two states that used to be indistinguishable, side by side.
    /// If this ever passes with both rendering the same string, the whole
    /// mechanism is worthless.
    #[tokio::test]
    #[serial_test::serial]
    async fn never_consulted_and_consulted_but_empty_render_differently() {
        // (a) consulted, empty.
        let server = MockServer::start().await;
        let _bases = BaseGuard::to(&server.uri());
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"results":[]}"#))
            .mount(&server)
            .await;
        let (_td, ctx) = ctx_for(&server.address().to_string());
        let ref_ = Ref::Doi(Doi::parse("10.1234/example").expect("doi"));
        let mut f1 = CrossrefFields::default();
        let mut consulted = Vec::new();
        let mut on = all_off();
        on.metadata.datacite = true;
        resolve_optional_chain(&ref_, &on, &ctx, false, &mut f1, &mut consulted).await;

        // (b) never consulted.
        let mut f2 = CrossrefFields::default();
        let mut skipped = Vec::new();
        resolve_optional_chain(&ref_, &all_off(), &ctx, false, &mut f2, &mut skipped).await;

        let a = outcome(&consulted, "datacite");
        let b = outcome(&skipped, "datacite");
        assert!(a.was_consulted(), "(a) must be consulted, got {a:?}");
        assert!(!b.was_consulted(), "(b) must NOT be consulted, got {b:?}");
        assert_ne!(
            a.render(),
            b.render(),
            "the two states MUST NOT render identically"
        );
        assert!(a.render().starts_with("consulted:"), "{}", a.render());
        assert!(b.render().starts_with("not consulted"), "{}", b.render());
    }

    /// When Crossref already answered, the chain must not fire at all —
    /// and must say why, rather than looking like everything was disabled.
    #[tokio::test]
    #[serial_test::serial]
    async fn a_crossref_hit_skips_the_chain_without_pretending_it_was_disabled() {
        let server = MockServer::start().await;
        let _bases = BaseGuard::to(&server.uri());
        let (_td, ctx) = ctx_for(&server.address().to_string());
        let ref_ = Ref::Doi(Doi::parse("10.1234/example").expect("doi"));
        let mut fields = CrossrefFields::default();
        let mut attempts = Vec::new();

        resolve_optional_chain(&ref_, &all_on(), &ctx, true, &mut fields, &mut attempts).await;

        assert!(
            server
                .received_requests()
                .await
                .expect("recorded")
                .is_empty(),
            "a Crossref hit must cost no extra requests"
        );
        for a in &attempts {
            assert_eq!(
                a.outcome,
                AttemptOutcome::NotNeeded,
                "{} must be NotNeeded, not Disabled — the flags ARE on",
                a.source
            );
        }
    }

    /// An access refusal is not a miss. OpenAIRE / Europe PMC / HAL all
    /// report "found it, cannot give it to you" — that must not be recorded
    /// as `NoRecord`, or the operator concludes the paper does not exist.
    #[tokio::test]
    #[serial_test::serial]
    async fn an_access_refusal_is_recorded_distinctly_from_a_miss() {
        let server = MockServer::start().await;
        let _bases = BaseGuard::to(&server.uri());
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"resultList":{"result":[{"doi":"10.1234/x","isOpenAccess":"N","inEPMC":"Y"}]}}"#,
            ))
            .mount(&server)
            .await;
        let (_td, ctx) = ctx_for(&server.address().to_string());
        let ref_ = Ref::Doi(Doi::parse("10.1234/x").expect("doi"));
        let mut fields = CrossrefFields::default();
        let mut attempts = Vec::new();
        let mut on = all_off();
        on.metadata.europe_pmc = true;

        resolve_optional_chain(&ref_, &on, &ctx, false, &mut fields, &mut attempts).await;

        let o = outcome(&attempts, "europe-pmc");
        assert!(
            matches!(o, AttemptOutcome::NotOpenAccess { .. }),
            "a closed record must be NotOpenAccess, not NoRecord/Failed; got {o:?}"
        );
        assert!(o.was_consulted(), "it WAS reached");
        assert!(
            o.render().contains("not open access"),
            "the reason must survive into the message; got {}",
            o.render()
        );
    }

    /// A source that cannot serve the ref kind is not a misconfiguration,
    /// so it must not tell the user to set an env var that would not help.
    #[tokio::test]
    #[serial_test::serial]
    async fn an_arxiv_ref_is_not_applicable_rather_than_disabled() {
        let server = MockServer::start().await;
        let _bases = BaseGuard::to(&server.uri());
        let (_td, ctx) = ctx_for(&server.address().to_string());
        let ref_ = Ref::Arxiv(crate::ArxivId::parse("2401.12345").expect("arxiv"));
        let mut fields = CrossrefFields::default();
        let mut attempts = Vec::new();

        resolve_optional_chain(&ref_, &all_on(), &ctx, false, &mut fields, &mut attempts).await;

        for a in &attempts {
            assert_eq!(
                a.outcome,
                AttemptOutcome::NotApplicable,
                "{} must be NotApplicable for an arXiv ref",
                a.source
            );
            assert!(
                !a.outcome.render().contains("set DOIGET_"),
                "must not suggest a variable that would not help: {}",
                a.outcome.render()
            );
        }
        assert!(server
            .received_requests()
            .await
            .expect("recorded")
            .is_empty());
    }
}

// ---------------------------------------------------------------------------
// #442: is the Tier-3 chain actually reached?
// ---------------------------------------------------------------------------
//
// These sources shipped implemented, gated, allowlisted and unit-tested,
// and no production code called them. Every unit test passed, because
// each drove its own `Source` impl directly. So the assertions here are
// about REACH: `server.received_requests()` is the only evidence that a
// source was consulted, and the attempt trace is the only way an operator
// can tell "consulted and empty" from "never consulted" from "consulted
// about another publisher's DOI".
//
// Gated on all three publishers because the tests name all three. The
// single-feature CI job still compiles `resolve_tdm_chain` itself, which
// is what that job is for.

// ---------------------------------------------------------------------------
// #458: is the chain reachable with ONE publisher compiled?
// ---------------------------------------------------------------------------
//
// The suite below needs all four features, so it only ever runs in the
// union CI job — where the gates are satisfied by the other three no
// matter what the fourth is missing. That is exactly how `tdm-ieee`
// shipped with the chain `#[cfg]`-ed away from it: the source, the
// allowlist, the capability grant and the docs all existed, and the only
// code that calls any of them was compiled out.
//
// This module is gated on `any(...)`, so it compiles in every singleton
// job. It does not need a mock, a key or a grant — merely naming
// `resolve_tdm_chain` fails the build when the caller's gate omits the
// compiled publisher, which is the whole bug.

#[cfg(all(
    test,
    any(
        feature = "tdm-aps",
        feature = "tdm-elsevier",
        feature = "tdm-springer",
        feature = "tdm-ieee"
    )
))]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tdm_singleton_reach_tests {
    use super::*;

    use std::sync::Arc;

    use camino::Utf8PathBuf;
    use tempfile::TempDir;

    use crate::http::HttpClient;
    use crate::provenance::ProvenanceLog;
    use crate::rate_limiter::RateLimiter;
    use crate::{CapabilityProfile, Doi, RateLimits, Ref};

    fn ctx() -> (TempDir, FetchContext) {
        let td = TempDir::new().expect("tempdir");
        let dir = Utf8PathBuf::try_from(td.path().to_path_buf()).expect("utf-8");
        let session_id = "01J0000000000000000000SNG".to_string();
        let log = Arc::new(
            ProvenanceLog::open(dir.join("t.jsonl"), session_id.clone()).expect("log opens"),
        );
        let c = FetchContext {
            http: Arc::new(HttpClient::new_for_tests_allow_http(
                "tdm-probe",
                "127.0.0.1:1",
            )),
            rate_limiter: Arc::new(RateLimiter::new(RateLimits::HARD_CODED)),
            log,
            session_id,
            cache_root: None,
        };
        (td, c)
    }

    /// Every compiled publisher must appear in the trace for a DOI it
    /// registered — with the gates CLOSED, so no request goes out and no
    /// credential is needed. A source missing here is one the production
    /// path cannot reach, whatever its own unit tests say.
    #[tokio::test]
    #[serial_test::serial]
    #[allow(clippy::vec_init_then_push)]
    async fn every_compiled_publisher_is_in_the_chain() {
        // Pushed rather than an array literal because each entry is
        // `#[cfg]`-gated, which is not expressible inside one.
        let mut cases: Vec<(&str, &str)> = Vec::new();
        #[cfg(feature = "tdm-aps")]
        cases.push(("tdm-aps", "10.1103/PhysRevX.10.011001"));
        #[cfg(feature = "tdm-elsevier")]
        cases.push(("tdm-elsevier", "10.1016/j.example.2024.001"));
        #[cfg(feature = "tdm-springer")]
        cases.push(("tdm-springer", "10.1007/s00220-024-05001-x"));
        #[cfg(feature = "tdm-ieee")]
        cases.push(("tdm-ieee", "10.1109/TSP.2018.2812747"));
        assert!(!cases.is_empty(), "the guard must have checked something");

        let (_td, c) = ctx();
        let profile = CapabilityProfile::for_tests();

        for (name, doi) in cases {
            let ref_ = Ref::Doi(Doi::parse(doi).expect("doi"));
            let mut attempts = Vec::new();
            resolve_tdm_chain(&ref_, &profile, &c, false, &mut attempts).await;
            assert!(
                attempts.iter().any(|a| a.source == name),
                "`{name}` is compiled but absent from the chain for its own DOI {doi}; \
                 the production path cannot reach it. attempts: {attempts:?}"
            );
        }
    }
}

#[cfg(all(
    test,
    feature = "tdm-aps",
    feature = "tdm-elsevier",
    feature = "tdm-springer",
    feature = "tdm-ieee"
))]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tdm_chain_tests {
    use super::*;

    use std::sync::Arc;

    use camino::Utf8PathBuf;
    use tempfile::TempDir;
    use wiremock::matchers::{header, method, path, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::http::HttpClient;
    use crate::provenance::ProvenanceLog;
    use crate::rate_limiter::RateLimiter;
    use crate::{CapabilityProfile, Doi, RateLimits, Ref, TdmGrant};

    /// Point every TDM base at the mock; restore on drop. Process-global,
    /// so every test using it is serialised.
    struct BaseGuard(Vec<(&'static str, Option<String>)>);
    impl BaseGuard {
        fn to(uri: &str) -> Self {
            const VARS: &[&str] = &[
                "DOIGET_APS_BASE",
                "DOIGET_ELSEVIER_BASE",
                "DOIGET_SPRINGER_BASE",
                "DOIGET_IEEE_BASE",
            ];
            Self(
                VARS.iter()
                    .map(|v| {
                        let old = std::env::var(v).ok();
                        std::env::set_var(v, uri);
                        (*v, old)
                    })
                    .collect(),
            )
        }
    }
    impl Drop for BaseGuard {
        fn drop(&mut self) {
            for (v, old) in &self.0 {
                match old {
                    Some(o) => std::env::set_var(v, o),
                    None => std::env::remove_var(v),
                }
            }
        }
    }

    /// Crossref / Unpaywall / arXiv also have to be pinned, or the test
    /// silently escapes to the live internet -- which is how the first
    /// run of the #442 probe produced a real paper title from a mock that
    /// had received zero requests.
    struct OaBaseGuard(Vec<(&'static str, Option<String>)>);
    impl OaBaseGuard {
        fn to(uri: &str) -> Self {
            const VARS: &[&str] = &[
                "DOIGET_CROSSREF_BASE",
                "DOIGET_UNPAYWALL_BASE",
                "DOIGET_ARXIV_BASE",
            ];
            Self(
                VARS.iter()
                    .map(|v| {
                        let old = std::env::var(v).ok();
                        std::env::set_var(v, uri);
                        (*v, old)
                    })
                    .collect(),
            )
        }
    }
    impl Drop for OaBaseGuard {
        fn drop(&mut self) {
            for (v, old) in &self.0 {
                match old {
                    Some(o) => std::env::set_var(v, o),
                    None => std::env::remove_var(v),
                }
            }
        }
    }

    fn ctx_for(host: &str) -> (TempDir, FetchContext) {
        let td = TempDir::new().expect("tempdir");
        let dir = Utf8PathBuf::try_from(td.path().to_path_buf()).expect("utf-8");
        let http = Arc::new(HttpClient::new_for_tests_allow_http_multi(&[
            ("tdm-aps", host),
            ("tdm-elsevier", host),
            ("tdm-springer", host),
            ("tdm-ieee", host),
        ]));
        let session_id = "01J0000000000000000000TDM".to_string();
        let log = Arc::new(
            ProvenanceLog::open(dir.join("t.jsonl"), session_id.clone()).expect("log opens"),
        );
        (
            td,
            FetchContext {
                http,
                rate_limiter: Arc::new(RateLimiter::new(RateLimits::HARD_CODED)),
                log,
                session_id,
                cache_root: None,
            },
        )
    }

    fn grant(agree_var: &str) -> TdmGrant {
        TdmGrant {
            api_key: secrecy::SecretString::from("test-key".to_string()),
            agree_env_var: agree_var.to_string(),
            ..Default::default()
        }
    }

    fn all_gates_open() -> CapabilityProfile {
        let mut p = CapabilityProfile::for_tests();
        p.tdm_aps = Some(grant("DOIGET_AGREE_TDM_APS"));
        p.tdm_elsevier = Some(grant("DOIGET_AGREE_TDM_ELSEVIER"));
        p.tdm_springer = Some(grant("DOIGET_AGREE_TDM_SPRINGER"));
        p.tdm_ieee = Some(grant("DOIGET_AGREE_TDM_IEEE"));
        p
    }

    fn all_gates_closed() -> CapabilityProfile {
        let mut p = CapabilityProfile::for_tests();
        p.tdm_aps = None;
        p.tdm_elsevier = None;
        p.tdm_springer = None;
        p.tdm_ieee = None;
        p
    }

    fn outcome<'a>(attempts: &'a [SourceAttempt], name: &str) -> &'a AttemptOutcome {
        &attempts
            .iter()
            .find(|a| a.source == name)
            .unwrap_or_else(|| panic!("no attempt recorded for {name}; got {attempts:?}"))
            .outcome
    }

    /// THE regression for #442. Each publisher's own DOI must actually
    /// reach that publisher's source — proven by the mock having received
    /// a request, not by any assertion about return values.
    #[tokio::test]
    #[serial_test::serial]
    async fn every_tdm_source_is_reached_for_its_own_publishers_doi() {
        for (doi, expected) in [
            ("10.1103/PhysRevX.10.011001", "tdm-aps"),
            ("10.1016/j.example.2024.001", "tdm-elsevier"),
            ("10.1007/s00220-024-05001-x", "tdm-springer"),
            ("10.1109/TSP.2018.2812747", "tdm-ieee"),
            // The conference prefix, which is half of what #407 measured
            // and the half most likely to be dropped by a later edit.
            ("10.23919/example.2024.001", "tdm-ieee"),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
                .mount(&server)
                .await;
            let _bases = BaseGuard::to(&server.uri());
            let (_td, ctx) = ctx_for(&server.address().to_string());

            let ref_ = Ref::Doi(Doi::parse(doi).expect("doi"));
            let mut attempts = Vec::new();
            resolve_tdm_chain(&ref_, &all_gates_open(), &ctx, false, &mut attempts).await;

            let o = outcome(&attempts, expected);
            assert!(
                o.was_consulted(),
                "{expected} was NOT reached for {doi}: {o:?}"
            );
            assert_eq!(
                server.received_requests().await.expect("recorded").len(),
                1,
                "{expected} must issue exactly one request for {doi}"
            );
        }
    }

    /// A foreign DOI must be reported as the wrong publisher, NOT as a
    /// missing credential. Telling the user to go get an APS key because
    /// an Elsevier DOI failed sends them after a fix that cannot work.
    #[tokio::test]
    #[serial_test::serial]
    async fn a_foreign_doi_is_wrong_publisher_not_disabled() {
        let server = MockServer::start().await;
        let _bases = BaseGuard::to(&server.uri());
        let (_td, ctx) = ctx_for(&server.address().to_string());

        // An AMS DOI: no compiled-in TDM publisher owns 10.1090. This
        // was an IEEE DOI until #430 gave 10.1109 an owner — the test
        // needs a prefix that stays foreign, and ADR-0039 names AMS as
        // one of the publishers still without a TDM source.
        let ref_ = Ref::Doi(Doi::parse("10.1090/s0025-5718-04-01692-8").expect("doi"));
        let mut attempts = Vec::new();
        resolve_tdm_chain(&ref_, &all_gates_open(), &ctx, false, &mut attempts).await;

        for name in ["tdm-aps", "tdm-elsevier", "tdm-springer", "tdm-ieee"] {
            let o = outcome(&attempts, name);
            assert!(
                matches!(o, AttemptOutcome::WrongPublisher { .. }),
                "{name} must be WrongPublisher for an AMS DOI, got {o:?}"
            );
            assert!(
                !o.render().contains("DOIGET_KEY"),
                "must not suggest a credential that would not help: {}",
                o.render()
            );
            assert!(
                o.render().contains("10.1090"),
                "the message must name the prefix that did not match: {}",
                o.render()
            );
        }
        assert!(
            server
                .received_requests()
                .await
                .expect("recorded")
                .is_empty(),
            "a foreign DOI must cost the publisher nothing"
        );
    }

    /// Closed gates must name BOTH things the user has to set. The Tier-2
    /// chain needs one `DOIGET_ENABLE_*`; Tier 3 needs a key and a
    /// recorded agreement, and naming only one would stall the user.
    #[tokio::test]
    #[serial_test::serial]
    async fn closed_gates_name_the_key_and_the_agreement() {
        let server = MockServer::start().await;
        let _bases = BaseGuard::to(&server.uri());
        let (_td, ctx) = ctx_for(&server.address().to_string());

        let ref_ = Ref::Doi(Doi::parse("10.1103/PhysRevX.10.011001").expect("doi"));
        let mut attempts = Vec::new();
        resolve_tdm_chain(&ref_, &all_gates_closed(), &ctx, false, &mut attempts).await;

        let o = outcome(&attempts, "tdm-aps");
        assert_eq!(
            o,
            &AttemptOutcome::Disabled {
                env: &["DOIGET_KEY_APS", "DOIGET_AGREE_TDM_APS"]
            },
            "a closed Tier-3 gate must name the key AND the agreement"
        );
        assert!(!o.was_consulted());
        assert!(
            server
                .received_requests()
                .await
                .expect("recorded")
                .is_empty(),
            "closed gates must make NO request"
        );
    }

    /// A Crossref hit must cost the publisher nothing, and must not be
    /// recorded as though the credentials were missing.
    #[tokio::test]
    #[serial_test::serial]
    async fn a_crossref_hit_skips_the_tdm_chain() {
        let server = MockServer::start().await;
        let _bases = BaseGuard::to(&server.uri());
        let (_td, ctx) = ctx_for(&server.address().to_string());

        let ref_ = Ref::Doi(Doi::parse("10.1103/PhysRevX.10.011001").expect("doi"));
        let mut attempts = Vec::new();
        resolve_tdm_chain(&ref_, &all_gates_open(), &ctx, true, &mut attempts).await;

        for name in ["tdm-aps", "tdm-elsevier", "tdm-springer", "tdm-ieee"] {
            assert_eq!(
                outcome(&attempts, name),
                &AttemptOutcome::NotNeeded,
                "{name} must be NotNeeded -- the gates ARE open"
            );
        }
        assert!(server
            .received_requests()
            .await
            .expect("recorded")
            .is_empty());
    }
    /// The gap the four tests above CANNOT close.
    ///
    /// They drive `resolve_tdm_chain` directly, so they would all still
    /// pass if nothing called it — which is exactly the shape of the bug
    /// (#442, and #438 before it). This one drives the real entry point,
    /// `fetch_paper`, and asserts the publisher's API was contacted.
    #[tokio::test]
    #[serial_test::serial]
    async fn fetch_paper_actually_reaches_the_tdm_chain() {
        let server = MockServer::start().await;
        // Everything 404s: Crossref produces nothing, so the DOI path has
        // to fall through to the chain. That is the only state in which a
        // TDM source is supposed to run.
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let _bases = BaseGuard::to(&server.uri());
        let _oa = OaBaseGuard::to(&server.uri());
        let (_td, ctx) = ctx_for(&server.address().to_string());

        let store_td = TempDir::new().expect("tempdir");
        let root = Utf8PathBuf::try_from(store_td.path().to_path_buf()).expect("utf-8");
        let store = crate::store::FsStore::new(root.clone()).expect("store");

        let ref_ = Ref::Doi(Doi::parse("10.1103/PhysRevX.10.011001").expect("doi"));
        let err = fetch_paper(&ref_, &all_gates_open(), &ctx, &store, &root)
            .await
            .expect_err("everything 404s, so the fetch must fail");

        // Evidence 1: the publisher's API was contacted.
        let paths: Vec<String> = server
            .received_requests()
            .await
            .expect("recorded")
            .iter()
            .map(|r| r.url.path().to_string())
            .collect();
        // The prefix APS publishes, not the one this repo happens to
        // build. #484 shipped `/v2/article/` past every test in the tree
        // precisely because each of them was written from the
        // implementation's output.
        const APS_DOCUMENTED_PREFIX: &str = "/v2/journals/articles/";
        assert!(
            paths.iter().any(|p| p.contains(APS_DOCUMENTED_PREFIX)),
            "fetch_paper never reached tdm-aps at its documented endpoint; paths were {paths:?}"
        );

        // Evidence 2: and the trace says so, in terms an operator can act
        // on. A consult that leaves no trace is only half a fix -- the
        // whole point is being able to tell reach from non-reach by
        // reading the error.
        let hint = err.to_string();
        assert!(
            hint.contains("tdm-aps") && hint.contains("consulted:"),
            "the trace must record tdm-aps as consulted; got:
{hint}"
        );
    }
    // -----------------------------------------------------------------
    // #458 — the CONTENT leg. Everything above this line drives the
    // metadata stage (Crossref missed); these drive the state #458 was
    // actually reported in: Crossref answered fine, and the PDF is what
    // could not be had.
    // -----------------------------------------------------------------

    /// The mock has to serve Crossref, Unpaywall, the OA host AND the TDM
    /// endpoint, so every one of those source keys needs an allowlist
    /// entry. `ctx_for` above registers only the `tdm-*` keys, which is
    /// why its tests can only ever 404 their way to the chain.
    fn ctx_for_content(host: &str) -> (TempDir, FetchContext) {
        let td = TempDir::new().expect("tempdir");
        let dir = Utf8PathBuf::try_from(td.path().to_path_buf()).expect("utf-8");
        let http = Arc::new(HttpClient::new_for_tests_allow_http_multi(&[
            ("crossref", host),
            ("unpaywall", host),
            ("oa-publisher", host),
            ("tdm-aps", host),
            ("tdm-elsevier", host),
            ("tdm-springer", host),
            ("tdm-ieee", host),
        ]));
        let session_id = "01J0000000000000000000CNT".to_string();
        let log = Arc::new(
            ProvenanceLog::open(dir.join("t.jsonl"), session_id.clone()).expect("log opens"),
        );
        (
            td,
            FetchContext {
                http,
                rate_limiter: Arc::new(RateLimiter::new(RateLimits::HARD_CODED)),
                log,
                session_id,
                cache_root: None,
            },
        )
    }

    /// Minimal Crossref envelope. Its job here is simply to SUCCEED, which
    /// is what makes `resolve_tdm_chain` record `NotNeeded` for every
    /// Tier-3 entry — the short circuit #458 is about.
    fn crossref_body() -> serde_json::Value {
        serde_json::json!({
            "status": "ok",
            "message": {
                "title": ["A paper APS published"],
                "author": [{ "family": "Doe", "given": "Jane" }],
                "issued": { "date-parts": [[2026, 1, 1]] },
                "container-title": ["Physical Review X"],
                "type": "journal-article"
            }
        })
    }

    /// Unpaywall reports an OA copy that will turn out to be unreachable.
    /// `license` is deliberately `cc-by`: if the TDM copy inherited it,
    /// the store would carry an open-licence claim about a file obtained
    /// under a signed agreement.
    fn unpaywall_body(oa_url: &str) -> serde_json::Value {
        serde_json::json!({
            "doi": "10.1103/PhysRevX.10.011001",
            "is_oa": true,
            "title": "A paper APS published",
            "best_oa_location": {
                "url": oa_url,
                "url_for_pdf": oa_url,
                "license": "cc-by"
            }
        })
    }

    const PDF_BYTES: &[u8] = b"%PDF-1.7\nthe publisher's own copy\n%%EOF\n";

    /// Mount Crossref (answers), Unpaywall (points at a dead OA URL), the
    /// dead OA URL itself, and whatever APS should reply with.
    ///
    /// Registration order matters: wiremock takes the first mock that
    /// matches, so the Unpaywall catch-all goes last.
    async fn mount_oa_blocked(server: &MockServer, aps: ResponseTemplate) {
        let oa_url = format!("{}/oa/file.pdf", server.uri());
        Mock::given(method("GET"))
            .and(path_regex("^/works/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(crossref_body()))
            .mount(server)
            .await;
        // The PDF representation of the APS article endpoint. Matched on
        // `Accept` so it cannot be confused with the metadata-stage call
        // to the same path — if the two were conflated this test would
        // pass without the content leg existing at all.
        Mock::given(method("GET"))
            .and(path_regex("^/v2/journals/articles/"))
            .and(header("accept", "application/pdf"))
            .respond_with(aps)
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/oa/file.pdf"))
            .respond_with(ResponseTemplate::new(403))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(unpaywall_body(&oa_url)))
            .mount(server)
            .await;
    }

    fn aps_pdf_requests(reqs: &[wiremock::Request]) -> usize {
        reqs.iter()
            .filter(|r| {
                r.url.path().starts_with("/v2/journals/articles/")
                    && r.headers
                        .get("accept")
                        .and_then(|v| v.to_str().ok())
                        .is_some_and(|v| v.contains("application/pdf"))
            })
            .count()
    }

    /// THE regression for #458.
    ///
    /// Crossref answers, so the metadata-stage chain records `NotNeeded`
    /// for every Tier-3 source exactly as it always did. The OA copy is
    /// refused. Before this change that was the end of it — byte-identical
    /// output with the TDM gates open and closed, which is the report.
    #[tokio::test]
    #[serial_test::serial]
    async fn tdm_content_leg_serves_the_pdf_when_the_oa_route_is_blocked() {
        let server = MockServer::start().await;
        mount_oa_blocked(
            &server,
            ResponseTemplate::new(200).set_body_bytes(PDF_BYTES),
        )
        .await;

        let _bases = BaseGuard::to(&server.uri());
        let _oa = OaBaseGuard::to(&server.uri());
        let (_td, ctx) = ctx_for_content(&server.address().to_string());

        let store_td = TempDir::new().expect("tempdir");
        let root = Utf8PathBuf::try_from(store_td.path().to_path_buf()).expect("utf-8");
        let store = crate::store::FsStore::new(root.clone()).expect("store");

        let ref_ = Ref::Doi(Doi::parse("10.1103/PhysRevX.10.011001").expect("doi"));
        let outcome = fetch_paper(&ref_, &all_gates_open(), &ctx, &store, &root)
            .await
            .expect("the TDM content leg should have supplied the PDF");

        // Evidence 1: a PDF request actually went out to APS. Asserted on
        // the mock's record rather than on the return value, because
        // "reached" is the whole question (#442).
        let reqs = server.received_requests().await.expect("recorded");
        assert_eq!(
            aps_pdf_requests(&reqs),
            1,
            "expected exactly one Accept: application/pdf request to the APS article endpoint; \
             paths were {:?}",
            reqs.iter().map(|r| r.url.path()).collect::<Vec<_>>()
        );

        // Evidence 2: the leg says where the bytes came from.
        match &outcome.pdf_leg {
            PdfLegStatus::TdmFetched {
                source,
                original_block,
            } => {
                assert_eq!(source, "tdm-aps");
                assert!(
                    !original_block.is_empty(),
                    "the OA refusal must be carried forward, not discarded"
                );
            }
            other => panic!("expected TdmFetched, got {other:?}"),
        }

        // Evidence 3: provenance names the publisher, not `oa-publisher`.
        assert_eq!(outcome.source, "tdm-aps");
        assert_eq!(outcome.size_bytes, PDF_BYTES.len() as u64);

        // Evidence 4: and it does not claim the file is CC-BY. Unpaywall
        // said `cc-by` about the OA location we never got; this file came
        // from the publisher under an agreement, by a route that licence
        // does not describe.
        assert_eq!(
            outcome.license, "unknown",
            "a TDM-retrieved copy must not inherit the OA location's licence"
        );
    }

    /// Prefix scoping still holds on the new path (ADR-0041). An Elsevier
    /// DOI must not cause a request to APS — the disclosure argument for
    /// the whole feature depends on it, and the new consultation point
    /// fires far more often than the old one.
    #[tokio::test]
    #[serial_test::serial]
    async fn tdm_content_leg_is_not_consulted_for_another_publishers_doi() {
        let server = MockServer::start().await;
        mount_oa_blocked(
            &server,
            ResponseTemplate::new(200).set_body_bytes(PDF_BYTES),
        )
        .await;

        let _bases = BaseGuard::to(&server.uri());
        let _oa = OaBaseGuard::to(&server.uri());
        let (_td, ctx) = ctx_for_content(&server.address().to_string());

        let store_td = TempDir::new().expect("tempdir");
        let root = Utf8PathBuf::try_from(store_td.path().to_path_buf()).expect("utf-8");
        let store = crate::store::FsStore::new(root.clone()).expect("store");

        // An Elsevier prefix, with the APS gate wide open.
        let ref_ = Ref::Doi(Doi::parse("10.1016/j.physrep.2020.01.001").expect("doi"));
        let _ = fetch_paper(&ref_, &all_gates_open(), &ctx, &store, &root).await;

        let reqs = server.received_requests().await.expect("recorded");
        assert_eq!(
            aps_pdf_requests(&reqs),
            0,
            "an Elsevier DOI reached the APS content endpoint; paths were {:?}",
            reqs.iter().map(|r| r.url.path()).collect::<Vec<_>>()
        );
    }

    /// The zero-request test above proves nothing reached APS. It cannot
    /// prove WHICH gate stopped it: `Source::can_serve` checks the prefix
    /// too, so deleting the orchestrator check leaves that test green.
    ///
    /// What only the orchestrator produces is the DISTINCTION. ADR-0041
    /// checks the prefix before credentials precisely so a foreign DOI
    /// reads as `WrongPublisher` rather than `Disabled` -- otherwise the
    /// trace tells the user to go and find an API key that would not have
    /// helped, which is the failure #438 added the trace to prevent.
    ///
    /// Mirrors `a_foreign_doi_is_wrong_publisher_not_disabled`, which
    /// makes the same assertion about the metadata chain.
    #[tokio::test]
    #[serial_test::serial]
    async fn content_leg_reports_a_foreign_doi_as_wrong_publisher_not_disabled() {
        let server = MockServer::start().await;
        let _bases = BaseGuard::to(&server.uri());
        let (_td, ctx) = ctx_for_content(&server.address().to_string());

        let doi = Doi::parse("10.1016/j.physrep.2020.01.001").expect("doi");
        let blocked = PdfLegStatus::Blocked {
            code: crate::ErrorCode::NetworkError,
            message: "the open route refused us".to_string(),
            denial: None,
            suggested_arxiv_id: None,
        };
        let mut attempts: Vec<SourceAttempt> = Vec::new();

        let (leg, bytes) =
            try_tdm_content_fallback(&doi, blocked, None, &all_gates_open(), &ctx, &mut attempts)
                .await;

        assert!(bytes.is_none(), "no publisher owns this DOI here");
        assert!(matches!(leg, PdfLegStatus::Blocked { .. }));
        assert!(
            matches!(outcome(&attempts, "tdm-aps"), AttemptOutcome::WrongPublisher { .. }),
            "an Elsevier DOI must read as WrongPublisher for tdm-aps, not Disabled; got {attempts:?}"
        );
    }

    /// A 200 that is not a PDF must not become `<safekey>.pdf`, and must
    /// not displace the OA refusal the user has to act on.
    ///
    /// This is the case `fetch_bytes_with_headers` would have gotten
    /// wrong: publisher error pages and WAF holding responses are 200s
    /// with a body.
    #[tokio::test]
    #[serial_test::serial]
    async fn tdm_content_leg_rejects_a_non_pdf_body_and_keeps_the_original_block() {
        let server = MockServer::start().await;
        mount_oa_blocked(
            &server,
            ResponseTemplate::new(200).set_body_string("<html>Access denied</html>"),
        )
        .await;

        let _bases = BaseGuard::to(&server.uri());
        let _oa = OaBaseGuard::to(&server.uri());
        let (_td, ctx) = ctx_for_content(&server.address().to_string());

        let store_td = TempDir::new().expect("tempdir");
        let root = Utf8PathBuf::try_from(store_td.path().to_path_buf()).expect("utf-8");
        let store = crate::store::FsStore::new(root.clone()).expect("store");

        let ref_ = Ref::Doi(Doi::parse("10.1103/PhysRevX.10.011001").expect("doi"));
        let outcome = fetch_paper(&ref_, &all_gates_open(), &ctx, &store, &root)
            .await
            .expect("a metadata-only outcome is still an outcome");

        // The request went out...
        let reqs = server.received_requests().await.expect("recorded");
        assert_eq!(aps_pdf_requests(&reqs), 1);

        // ...and the HTML did not become a PDF.
        match &outcome.pdf_leg {
            PdfLegStatus::Blocked { message, .. } => {
                assert!(
                    !message.is_empty(),
                    "the ORIGINAL OA refusal must survive, not the TDM failure"
                );
            }
            other => panic!("expected the original Blocked leg to survive, got {other:?}"),
        }
        assert_eq!(outcome.size_bytes, 0, "nothing should have been stored");
    }
}

// ---------------------------------------------------------------------------
// #445: does a blocked content leg actually fall through to another source?
// ---------------------------------------------------------------------------
//
// The reported run had four indexes switched on and consulted none of them,
// because the candidate list can only contain Unpaywall's locations and
// Crossref had already answered. So the assertion that matters is REACH:
// the mock must record a request to the optional source AND to the copy it
// reported. A `Fetched` outcome alone would not prove which route produced it.

#[cfg(all(test, feature = "metadata"))]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod oa_fallthrough_tests {
    use super::*;

    use std::sync::Arc;

    use camino::Utf8PathBuf;
    use tempfile::TempDir;
    use wiremock::matchers::{path, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::http::HttpClient;
    use crate::provenance::ProvenanceLog;
    use crate::rate_limiter::RateLimiter;
    use crate::{CapabilityProfile, Doi, RateLimits, Ref};

    struct EnvSet(Vec<(&'static str, Option<String>)>);
    impl EnvSet {
        fn new(pairs: &[(&'static str, String)]) -> Self {
            Self(
                pairs
                    .iter()
                    .map(|(k, v)| {
                        let old = std::env::var(k).ok();
                        std::env::set_var(k, v);
                        (*k, old)
                    })
                    .collect(),
            )
        }
    }
    impl Drop for EnvSet {
        fn drop(&mut self) {
            for (k, old) in &self.0 {
                match old {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    /// Crossref answers, Unpaywall points at a host that 429s, CORE holds a
    /// repository copy.
    async fn server_with_a_rate_limited_publisher_and_a_repository_copy() -> MockServer {
        let server = MockServer::start().await;
        let base = server.uri();

        Mock::given(path_regex("^/works/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "{\"status\":\"ok\",\"message\":{\"title\":[\"Computing multiple roots\"]}}",
            ))
            .mount(&server)
            .await;
        Mock::given(path_regex("^/10\\.1090"))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!(
                "{{\"doi\":\"10.1090/example\",\"is_oa\":true,\"oa_status\":\"bronze\",\"best_oa_location\":\
                 {{\"url_for_pdf\":\"{base}/blocked.pdf\",\"license\":\"cc-by\"}}}}"
            )))
            .mount(&server)
            .await;
        // The publisher rate-limits. This is the failure that used to end
        // the whole run.
        Mock::given(path("/blocked.pdf"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;
        Mock::given(path("/v3/search/works"))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!(
                "{{\"totalHits\":1,\"results\":[{{\"id\":1,\
                 \"title\":\"Computing multiple roots\",\"doi\":\"10.1090/example\",\
                 \"downloadUrl\":\"{base}/repo.pdf\"}}]}}"
            )))
            .mount(&server)
            .await;
        Mock::given(path("/repo.pdf"))
            .respond_with(
                ResponseTemplate::new(200).set_body_bytes(b"%PDF-1.4\nrepository copy\n".to_vec()),
            )
            .mount(&server)
            .await;
        server
    }

    fn ctx_for(host: &str) -> (TempDir, FetchContext) {
        let td = TempDir::new().expect("tempdir");
        let dir = Utf8PathBuf::try_from(td.path().to_path_buf()).expect("utf-8");
        let host_only = host.split(':').next().unwrap_or(host);
        let http = Arc::new(HttpClient::new_for_tests_allow_http_multi(&[
            ("crossref", host),
            ("unpaywall", host),
            // The content leg compares the URL HOST, without the port,
            // so this entry must be the bare address.
            ("oa-publisher", host_only),
            ("core", host),
        ]));
        let session_id = "01J000000000000000000FALL".to_string();
        let log = Arc::new(
            ProvenanceLog::open(dir.join("t.jsonl"), session_id.clone()).expect("log opens"),
        );
        (
            td,
            FetchContext {
                http,
                rate_limiter: Arc::new(RateLimiter::new(RateLimits::HARD_CODED)),
                log,
                session_id,
                cache_root: None,
            },
        )
    }

    async fn run_fetch(
        server: &MockServer,
        core_enabled: bool,
    ) -> (FetchPaperOutcome, Vec<String>) {
        let base = server.uri();
        let mut env = vec![
            ("DOIGET_CROSSREF_BASE", base.clone()),
            ("DOIGET_UNPAYWALL_BASE", base.clone()),
            ("DOIGET_ARXIV_BASE", base.clone()),
            ("DOIGET_CORE_BASE", base.clone()),
            ("DOIGET_CONTACT_EMAIL", "test@example.org".to_string()),
        ];
        if core_enabled {
            env.push(("DOIGET_ENABLE_CORE", "1".to_string()));
        } else {
            std::env::remove_var("DOIGET_ENABLE_CORE");
        }
        let _env = EnvSet::new(&env);

        let profile = CapabilityProfile::from_env().expect("profile");
        let (_td, ctx) = ctx_for(&server.address().to_string());
        let store_td = TempDir::new().expect("tempdir");
        let root = Utf8PathBuf::try_from(store_td.path().to_path_buf()).expect("utf-8");
        let store = crate::store::FsStore::new(root.clone()).expect("store");

        let ref_ = Ref::Doi(Doi::parse("10.1090/example").expect("doi"));
        let outcome = fetch_paper(&ref_, &profile, &ctx, &store, &root)
            .await
            .expect("crossref answered, so the fetch resolves either way");
        let paths = server
            .received_requests()
            .await
            .expect("recorded")
            .iter()
            .map(|r| r.url.path().to_string())
            .collect();
        (outcome, paths)
    }

    /// THE regression for #445. A 429 on the only Unpaywall location must
    /// not end a run with another source switched on.
    #[tokio::test]
    #[serial_test::serial]
    async fn a_rate_limited_publisher_falls_through_to_an_enabled_source() {
        let server = server_with_a_rate_limited_publisher_and_a_repository_copy().await;
        let (outcome, paths) = run_fetch(&server, true).await;

        assert!(
            paths.iter().any(|p| p == "/v3/search/works"),
            "CORE was never consulted; paths were {paths:?}"
        );
        assert!(
            paths.iter().any(|p| p == "/repo.pdf"),
            "the copy CORE reported was never fetched; paths were {paths:?}; leg={:?}",
            outcome.pdf_leg
        );
        assert!(
            matches!(outcome.pdf_leg, PdfLegStatus::Fetched),
            "the run should have recovered; got {:?}",
            outcome.pdf_leg
        );
    }

    /// The other half of the contract: with no flag set, behaviour is
    /// unchanged. The fall-through must not spend a request the user did
    /// not ask for.
    #[tokio::test]
    #[serial_test::serial]
    async fn with_no_source_enabled_the_run_is_unchanged() {
        let server = server_with_a_rate_limited_publisher_and_a_repository_copy().await;
        let (outcome, paths) = run_fetch(&server, false).await;

        assert!(
            !paths.iter().any(|p| p == "/v3/search/works"),
            "a disabled source must cost nothing; paths were {paths:?}"
        );
        assert!(
            matches!(outcome.pdf_leg, PdfLegStatus::Blocked { .. }),
            "without a fallback source this must still be Blocked; got {:?}",
            outcome.pdf_leg
        );
    }
    /// #468 review: the fall-through used to do `*attempts = fresh`, which
    /// replaced the WHOLE trace and deleted the Tier-3 rows
    /// `resolve_tdm_chain` had already recorded.
    ///
    /// The row's outcome does not matter here — what matters is that the
    /// row still EXISTS. Its absence is what made "was tdm-ieee consulted?"
    /// unanswerable from `attempts`, from the MCP envelope and from
    /// `batch --json`, which is the question the trace was added to answer.
    ///
    /// Needs `metadata` AND a `tdm-*` feature, because the bug only appears
    /// when both chains have written to the same vector. CI builds exactly
    /// that combination.
    #[cfg(feature = "tdm-ieee")]
    #[tokio::test]
    #[serial_test::serial]
    async fn the_fallback_preserves_the_tier_3_rows_it_used_to_delete() {
        let server = server_with_a_rate_limited_publisher_and_a_repository_copy().await;
        let (outcome, paths) = run_fetch(&server, true).await;

        // The fall-through must actually have fired, or this proves nothing.
        assert!(
            paths.iter().any(|p| p == "/v3/search/works"),
            "the fallback did not run, so this test cannot see the bug; paths were {paths:?}"
        );

        let sources: Vec<&str> = outcome.attempts.iter().map(|a| a.source).collect();
        assert!(
            sources.contains(&"tdm-ieee"),
            "the Tier-3 row was dropped from the trace by the fallback; trace held {sources:?}"
        );
        // And the Tier-2 rows the fallback re-ran are still there too.
        assert!(
            sources.contains(&"core"),
            "the refreshed Tier-2 rows are missing; trace held {sources:?}"
        );
    }
}
