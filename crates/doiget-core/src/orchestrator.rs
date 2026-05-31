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
#[derive(Debug, Clone)]
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
    match ref_ {
        Ref::Doi(doi) => metadata_only_doi(doi, ref_, profile, ctx).await,
        Ref::Arxiv(id) => {
            let arxiv = arxiv_source_from_env();
            let metadata = arxiv.fetch_metadata_only(id, ctx).await?;
            // Pure resolver — no store write here (see fn doc); the
            // store-write side effect lives in `metadata_only_to_store`.
            Ok(MetadataOnlyOutcome {
                source: arxiv.name().to_string(),
                resolver_profile: arxiv.name().to_string(),
                license: Some("arxiv-default".to_string()),
                oa_url: None,
                metadata,
            })
        }
    }
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
        abstract_: None,
        venue: None,
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
            size_bytes: 0,
            mcp_call_id: None,
        }),
        other: BTreeMap::new(),
    }
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
        if let Ok(url) = url::Url::parse(&s) {
            return ArxivSource::with_base(url);
        }
    }
    ArxivSource::new()
}

fn crossref_source_from_env(contact: &str) -> CrossrefSource {
    if let Ok(s) = std::env::var("DOIGET_CROSSREF_BASE") {
        if let Ok(url) = url::Url::parse(&s) {
            return CrossrefSource::with_base(url, contact.to_string());
        }
    }
    CrossrefSource::new(contact.to_string())
}

fn unpaywall_source_from_env(contact: &str) -> UnpaywallSource {
    if let Ok(s) = std::env::var("DOIGET_UNPAYWALL_BASE") {
        if let Ok(url) = url::Url::parse(&s) {
            return UnpaywallSource::with_base(url, contact.to_string());
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
            path: Utf8PathBuf::from(format!("/tmp/{safekey}.pdf")),
            size_bytes: 0,
            schema_version: SCHEMA_VERSION.to_string(),
            pdf_leg,
            safekey: safekey.clone(),
            // 32 bytes of `0x00` → a stable, non-secret digest stub
            // that's still 64 chars of lowercase hex.
            canonical_digest: "00".repeat(32),
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
        ..
    } = source.fetch(ref_, profile, ctx).await?;
    let pdf = pdf_bytes.ok_or_else(|| FetchError::SourceSchema {
        hint: "arxiv source returned no PDF bytes".to_string(),
    })?;
    let size_bytes = pdf.len() as u64;

    // Phase 1 minimal metadata. Full Atom-feed extraction (title /
    // authors) lives in `ArxivSource::fetch_metadata_only` and the
    // metadata-only orchestrator; the fetch path keeps the placeholder
    // for now (a follow-up slice may chain in Atom-parse here).
    let metadata = Metadata {
        schema_version: SCHEMA_VERSION.to_string(),
        title: format!("arxiv:{}", id.as_str()),
        authors: Vec::new(),
        year: None,
        doi: None,
        arxiv_id: Some(id.clone()),
        abstract_: None,
        venue: None,
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
            size_bytes,
            mcp_call_id: None,
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
        path,
        size_bytes,
        schema_version: SCHEMA_VERSION.to_string(),
        // arXiv always delivers the PDF (or the whole fn already
        // returned Err above) — there is no metadata-only fallback.
        pdf_leg: PdfLegStatus::Fetched,
        safekey: safekey.as_str().to_string(),
        canonical_digest,
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
    let extracted = extract_crossref_fields(&crossref_meta);

    // Unpaywall second — license enrichment + OA URL chain discovery.
    // A failure here is non-fatal: we still write the Crossref-
    // derived metadata.
    let unpaywall = unpaywall_source_from_env(&unpaywall_contact);
    let upw_result = unpaywall.fetch(ref_, profile, ctx).await;
    let (license, source_label, oa_chain) = match upw_result {
        Ok(r) => {
            let chain = extract_oa_url_chain(r.metadata_json.as_ref());
            let label = if r.license != "unknown" {
                "unpaywall".to_string()
            } else {
                "crossref".to_string()
            };
            (r.license, label, chain)
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
            ("unknown".to_string(), "crossref".to_string(), Vec::new())
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

    // Issue #120: Crossref is non-fatal, but if it failed AND the OA
    // PDF leg produced nothing, writing a DOI-only stub entry would
    // mask a total failure and violate the "explain why" promise.
    // Surface the Crossref error so the caller reports a real reason.
    if let Some(e) = crossref_err {
        if pdf_bytes.is_none() {
            return Err(e);
        }
    }

    let (final_source_label, size_bytes, pdf_path_relative, pdf_staged) = match &pdf_bytes {
        Some(bytes) => {
            let staged = stage_pdf_to_tempfile(bytes)?;
            (
                "oa-publisher".to_string(),
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
        arxiv_id: None,
        abstract_: None,
        venue: extracted.venue,
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
            size_bytes,
            mcp_call_id: None,
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
        path,
        size_bytes,
        schema_version: SCHEMA_VERSION.to_string(),
        pdf_leg,
        safekey: safekey.as_str().to_string(),
        canonical_digest,
    })
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
pub(crate) struct CrossrefFields {
    pub(crate) title: Option<String>,
    pub(crate) authors: Vec<String>,
    pub(crate) year: Option<i32>,
    pub(crate) venue: Option<String>,
    pub(crate) type_: Option<String>,
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

    CrossrefFields {
        title,
        authors,
        year,
        venue,
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

/// Helper to parse clean arXiv IDs from URLs like arxiv.org/pdf/1901.12345.pdf
fn extract_arxiv_id_from_url(url: &url::Url) -> Option<String> {
    if let Some(host) = url.host_str() {
        if host == "arxiv.org" || host == "www.arxiv.org" || host == "export.arxiv.org" {
            let path = url.path();
            if path.starts_with("/pdf/") {
                let stripped = path.strip_prefix("/pdf/")?;
                let stripped = stripped.strip_suffix(".pdf").unwrap_or(stripped);
                return Some(stripped.to_string());
            } else if path.starts_with("/abs/") {
                return Some(path.strip_prefix("/abs/")?.to_string());
            }
        }
    }
    None
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

    #[test]
    fn test_extract_arxiv_id_from_url() {
        let urls = [
            ("https://arxiv.org/pdf/1901.12345.pdf", Some("1901.12345")),
            ("https://arxiv.org/abs/1901.12345", Some("1901.12345")),
            ("https://www.arxiv.org/pdf/cond-mat/9501001.pdf", Some("cond-mat/9501001")),
            ("https://export.arxiv.org/abs/cond-mat/9501001", Some("cond-mat/9501001")),
            ("https://example.org/pdf/1901.12345.pdf", None),
        ];
        for (url_str, expected) in urls {
            let url = url::Url::parse(url_str).unwrap();
            assert_eq!(extract_arxiv_id_from_url(&url), expected.map(String::from));
        }
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
        };
        let profile = CapabilityProfile::from_env().expect("clean env");
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
