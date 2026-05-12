//! Cross-source orchestrators that compose multiple [`Source`] impls into
//! a single user-facing operation.
//!
//! Phase 1 ships [`metadata_only`] only; the live PDF orchestrator lives
//! in `doiget-cli::commands::fetch` because it owns the on-disk store
//! (and `doiget-core` does not, by design). This module is the natural
//! home for orchestrators that the MCP server needs to call directly
//! without going through the CLI.
//!
//! [`Source`]: crate::source::Source

use serde_json::Value;

use crate::source::{FetchContext, FetchError, Source};
use crate::sources::arxiv::ArxivSource;
use crate::sources::crossref::CrossrefSource;
use crate::sources::unpaywall::UnpaywallSource;
use crate::{CapabilityProfile, Doi, Ref};

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
}
