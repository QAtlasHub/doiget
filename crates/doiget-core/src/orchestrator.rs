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

use crate::dry_run::{build_fetch_plan, FetchPlan};
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
/// TODO Phase 2.x: write the metadata TOML to the store after the
/// orchestrator path is proven; the spec entry in `docs/MCP_TOOLS.md`
/// §11 lists this as a SIDE EFFECT but Phase 2 store invariants
/// (which directory, schema_version handling) are out of scope for
/// Slice 1.
///
/// # Errors
///
/// Returns [`FetchError`] from the underlying [`Source`] dispatch. The
/// MCP boundary converts these to the closed [`crate::ErrorCode`] set
/// via the existing `From<FetchError> for ErrorCode` impl.
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
            // TODO Phase 2.x: write metadata TOML to store after
            // orchestrator path is proven.
            Ok(MetadataOnlyOutcome {
                source: arxiv.name().to_string(),
                license: Some("arxiv-default".to_string()),
                oa_url: None,
                metadata,
            })
        }
    }
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
            // TODO Phase 2.x: write metadata TOML to store after
            // orchestrator path is proven.
            Ok(MetadataOnlyOutcome {
                source: crossref.name().to_string(),
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
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct FetchPaperOutcome {
    /// `Source::name()` of the resolver whose payload landed on disk:
    /// `"arxiv"` for an arXiv ref, `"oa-publisher"` when the DOI OA PDF
    /// leg succeeded, or `"crossref"` / `"unpaywall"` when the DOI path
    /// fell back to metadata-only. Mirrors the value written to
    /// `[doiget].source` in the metadata TOML.
    pub source: String,
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
    Ok(FetchPaperOutcome {
        source: "arxiv".to_string(),
        license,
        path,
        size_bytes,
        schema_version: SCHEMA_VERSION.to_string(),
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
    let cross = crossref.fetch(ref_, profile, ctx).await?;
    let crossref_meta = cross.metadata_json.unwrap_or(Value::Null);
    let extracted = extract_crossref_fields(&crossref_meta);

    // Unpaywall second — license enrichment + OA URL discovery. A
    // failure here is non-fatal: we still write the Crossref-derived
    // metadata.
    let unpaywall = unpaywall_source_from_env(&unpaywall_contact);
    let upw_result = unpaywall.fetch(ref_, profile, ctx).await;
    let (license, source_label, oa_url) = match upw_result {
        Ok(r) => {
            let oa = extract_best_oa_url_from_value(r.metadata_json.as_ref());
            let label = if r.license != "unknown" {
                "unpaywall".to_string()
            } else {
                "crossref".to_string()
            };
            (r.license, label, oa)
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "unpaywall fetch failed; continuing with crossref-only metadata"
            );
            ("unknown".to_string(), "crossref".to_string(), None)
        }
    };

    // OA PDF leg. Try the URL Unpaywall returned via the `oa-publisher`
    // source allowlist. Magic-byte check is enforced inside
    // `HttpClient::fetch_pdf`. Any failure here is logged as a distinct
    // `Fetch err` row and falls through to metadata-only success.
    let pdf_outcome = if let Some(url) = oa_url {
        try_fetch_oa_pdf(doi, &url, ctx).await
    } else {
        None
    };

    let (final_source_label, size_bytes, pdf_path_relative, pdf_staged) = match &pdf_outcome {
        Some((bytes, _final_url)) => {
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
        url: cross.final_url.as_ref().map(|u| u.to_string()),
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

    let path = if pdf_outcome.is_some() {
        store_root.join(format!("{}.pdf", safekey.as_str()))
    } else {
        store_root
            .join(".metadata")
            .join(format!("{}.toml", safekey.as_str()))
    };
    Ok(FetchPaperOutcome {
        source: final_source_label,
        license,
        path,
        size_bytes,
        schema_version: SCHEMA_VERSION.to_string(),
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
            })?;
            Ok(())
        }
        Err(e) => {
            // Best-effort: record the StoreWrite failure before
            // propagating. Surface as `SourceSchema` so the
            // FetchError -> ErrorCode collapse routes it to
            // `INTERNAL_ERROR` (closest closed-set fit; `StoreError`
            // does not have a direct closed-set arm).
            let _ = ctx.log.append(RowInput {
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
            });
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
) -> Option<(Vec<u8>, url::Url)> {
    const SOURCE: &str = "oa-publisher";
    let _permit = ctx.rate_limiter.acquire(SOURCE).await;
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
            }) {
                tracing::warn!(error = %e, "appending oa-publisher Fetch ok row failed");
            }
            Some((body.to_vec(), final_url))
        }
        Err(e) => {
            match &e {
                HttpError::RedirectDenied { host, .. } => {
                    tracing::info!(
                        oa_url = %url,
                        denied_host = %host,
                        "OA URL host outside oa-publisher allowlist; metadata-only fallback"
                    );
                }
                HttpError::NotAPdf { .. } => {
                    tracing::info!(
                        oa_url = %url,
                        "OA URL did not return a PDF magic byte; metadata-only fallback"
                    );
                }
                other => {
                    tracing::warn!(
                        oa_url = %url,
                        error = %other,
                        "OA PDF fetch failed; metadata-only fallback"
                    );
                }
            }
            let _ = ctx.log.append(RowInput {
                event: LogEvent::Fetch,
                result: LogResult::Err,
                capability: Capability::Oa,
                ref_: Some(doi.as_str()),
                source: Some(SOURCE),
                error_code: Some("NETWORK_ERROR"),
                size_bytes: None,
                license: None,
                store_path: None,
            });
            None
        }
    }
}

/// Subset of Crossref `message` fields populated into the on-disk metadata.
struct CrossrefFields {
    title: Option<String>,
    authors: Vec<String>,
    year: Option<i32>,
    venue: Option<String>,
    type_: Option<String>,
}

/// Defensively pull bibliographic fields out of a Crossref envelope's
/// `message` object. Every field is optional; malformed shapes degrade
/// to `None` rather than panicking.
fn extract_crossref_fields(msg: &Value) -> CrossrefFields {
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

/// Pull the preferred OA URL out of an Unpaywall `metadata_json`
/// envelope. Tries `best_oa_location.url_for_pdf` first (direct PDF
/// link), falling back to `best_oa_location.url`.
fn extract_best_oa_url_from_value(meta: Option<&Value>) -> Option<url::Url> {
    let meta = meta?;
    let loc = meta.get("best_oa_location")?;
    let candidate = loc
        .get("url_for_pdf")
        .and_then(|v| v.as_str())
        .or_else(|| loc.get("url").and_then(|v| v.as_str()))?;
    url::Url::parse(candidate).ok()
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
/// Returns `Err(FetchError::TooManyRefs)` when over the cap; otherwise
/// `Ok(Vec<(Ref, FetchPlan)>)` parallel to the input order.
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
    Ok(refs
        .iter()
        .map(|r| (r.clone(), build_fetch_plan(r, store_root)))
        .collect())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

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
    fn extract_best_oa_url_from_value_prefers_url_for_pdf() {
        let meta = serde_json::json!({
            "best_oa_location": {
                "url_for_pdf": "https://example.org/pdf",
                "url": "https://example.org/landing"
            }
        });
        let url = extract_best_oa_url_from_value(Some(&meta)).expect("Some(Url)");
        assert_eq!(url.as_str(), "https://example.org/pdf");
    }

    #[test]
    fn extract_best_oa_url_from_value_falls_back_to_url() {
        let meta = serde_json::json!({
            "best_oa_location": {
                "url": "https://example.org/landing"
            }
        });
        let url = extract_best_oa_url_from_value(Some(&meta)).expect("Some(Url)");
        assert_eq!(url.as_str(), "https://example.org/landing");
    }

    #[test]
    fn extract_best_oa_url_from_value_none_on_missing() {
        let meta = serde_json::json!({});
        assert!(extract_best_oa_url_from_value(Some(&meta)).is_none());
        assert!(extract_best_oa_url_from_value(None).is_none());
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
}
