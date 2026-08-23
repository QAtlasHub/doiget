//! OpenAlex source — DOI metadata enrichment (Phase 4 / Tier 2).
//!
//! Spec: `docs/SOURCES.md` §1 Tier 2 row + §4 "OpenAlex / Semantic
//! Scholar / DOAJ". OpenAlex is a free, no-auth metadata API. Polite
//! pool is opted into via a `mailto` query parameter; the contact
//! email is supplied through [`OpenalexSource::new`] (same channel
//! Crossref uses).
//!
//! ## Capability gate
//!
//! [`OpenalexSource::can_serve`] returns `true` only when
//! [`CapabilityProfile.metadata.openalex`](crate::CapabilityProfile)
//! is `true` AND the ref is a [`Ref::Doi`]. The metadata bool is set
//! by [`CapabilityProfile::from_env`] from the
//! `DOIGET_ENABLE_OPENALEX` environment variable (presence-checked),
//! and only when the `metadata` Cargo feature is compiled in
//! (`docs/CAPABILITY.md` §2).
//!
//! ## Metadata-only contract
//!
//! Per `docs/SOURCES.md` §4, this source is **metadata-only**.
//! [`OpenalexSource::fetch`] never returns PDF bytes
//! (`FetchResult.pdf_bytes` is always `None`). The citation graph
//! orchestrator (Slice 14) consumes the `referenced_works` array
//! from the response metadata to expand the graph; that array lists
//! OpenAlex Work IDs, not DOIs, so the orchestrator does its own ID
//! resolution.

use async_trait::async_trait;
use url::Url;

use crate::provenance::{Capability, LogEvent, LogResult, RowInput};
use crate::source::{FetchContext, FetchError, FetchResult, Source};
use crate::{CapabilityProfile, Ref};

/// Production OpenAlex REST API base URL.
///
/// Hard-coded per `docs/SOURCES.md` §1 Tier-2 row. Tests inject a
/// wiremock origin via [`OpenalexSource::with_base`], identical to
/// the pattern used by `CrossrefSource`.
const DEFAULT_BASE: &str = "https://api.openalex.org";

/// OpenAlex [`Source`] impl — DOI → enriched bibliographic metadata.
///
/// See module docs for the capability-gate and metadata-only contract.
#[derive(Clone, Debug)]
pub struct OpenalexSource {
    /// API base URL. Production constructor pins this to
    /// `https://api.openalex.org`; the [`with_base`](Self::with_base)
    /// test-only constructor lets wiremock substitute an
    /// `http://127.0.0.1:N` origin.
    base: Url,
    /// Polite-pool contact email per `docs/SOURCES.md` §6. OpenAlex
    /// accepts this as a `?mailto=<email>` query parameter; doiget
    /// uses the query-parameter route so callers reading raw URLs in
    /// the provenance log can see the polite-pool opt-in directly.
    contact_email: String,
}

impl OpenalexSource {
    /// Production constructor: hard-codes `https://api.openalex.org`
    /// as the base URL.
    #[must_use]
    pub fn new(contact_email: String) -> Self {
        Self {
            #[allow(clippy::expect_used)]
            base: Url::parse(DEFAULT_BASE).expect("hard-coded base URL is valid"),
            contact_email,
        }
    }

    /// Construct with an arbitrary base URL.
    ///
    /// The orchestrator uses this to honor the `DOIGET_OPENALEX_BASE`
    /// env var (Slice 11+ wiring), which lets integration tests point
    /// the source at a wiremock origin without compile-time gates.
    /// Production callers use [`OpenalexSource::new`].
    pub fn with_base(base: Url, contact_email: String) -> Self {
        Self {
            base,
            contact_email,
        }
    }

    /// Build the `/works/doi:{doi}?mailto=<contact>` URL.
    ///
    /// OpenAlex's `/works/{id}` endpoint does **not** accept a bare DOI
    /// in the path — `GET /works/10.1103/PhysRevLett.102.190601` returns
    /// HTTP 404. It accepts an OpenAlex Work ID (`W…`), the namespaced
    /// `doi:<doi>` form, or a full `https://doi.org/<doi>` URL. We use
    /// the `doi:` prefix: it is unambiguous, needs no percent-encoding,
    /// and (unlike the `https://doi.org/` form) does not confuse
    /// `Url::join`'s scheme detection. The `mailto` query parameter opts
    /// into the polite pool per `docs/SOURCES.md` §6; when
    /// `contact_email` is empty the query parameter is omitted.
    fn request_url(&self, doi: &crate::Doi) -> Result<Url, FetchError> {
        let path = format!("/works/doi:{}", doi.as_str());
        let mut url = self
            .base
            .join(&path)
            .map_err(|e| FetchError::SourceSchema {
                hint: format!("openalex URL construction failed: {e}"),
            })?;
        if !self.contact_email.is_empty() {
            url.query_pairs_mut()
                .append_pair("mailto", &self.contact_email);
        }
        Ok(url)
    }
}

#[async_trait]
impl Source for OpenalexSource {
    fn name(&self) -> &str {
        "openalex"
    }

    fn can_serve(&self, profile: &CapabilityProfile, ref_: &Ref) -> bool {
        // Gated by both the runtime capability flag AND the ref kind.
        // arXiv ids are not OpenAlex Work IDs and OpenAlex's
        // `/works/<id>` endpoint expects either a DOI or an OpenAlex
        // Work ID; we only accept DOI here.
        profile.metadata.openalex && matches!(ref_, Ref::Doi(_))
    }

    async fn fetch(
        &self,
        ref_: &Ref,
        profile: &CapabilityProfile,
        ctx: &FetchContext,
    ) -> Result<FetchResult, FetchError> {
        let doi = match ref_ {
            Ref::Doi(d) => d,
            Ref::Arxiv(_) => {
                return Err(FetchError::NotEligible {
                    source_key: "openalex".into(),
                });
            }
        };

        // Defense-in-depth capability gate — the orchestrator should
        // have called `can_serve` first, but the source enforces too.
        if !profile.metadata.openalex {
            return Err(FetchError::NotEligible {
                source_key: "openalex".into(),
            });
        }

        // Step 1: rate limiter (politeness — `docs/SOURCES.md` §6).
        let _permit = ctx.rate_limiter.acquire(self.name()).await;

        // Step 2: HTTP fetch. Body is JSON; OpenAlex Work records are
        // tens of KB even for highly-cited papers, well under the
        // `PDF_MAX_BYTES` cap.
        let url = self.request_url(doi)?;
        let (body, final_url) = ctx.http.fetch_bytes(self.name(), url).await?;

        // Step 3: parse the response. OpenAlex returns the Work
        // record directly at the top level (no envelope, unlike
        // Crossref).
        let work: serde_json::Value =
            serde_json::from_slice(&body).map_err(|e| FetchError::SourceSchema {
                hint: format!("openalex returned non-JSON: {e}"),
            })?;

        // Defensive shape check — every real Work record has an
        // `id` field. An error payload has an `error` field instead
        // (and no `id`). Use missing `id` as the "not a Work record"
        // signal.
        if work.get("id").is_none() {
            return Err(FetchError::SourceSchema {
                hint: format!(
                    "openalex response missing `id` field — likely an error \
                     payload (got: {})",
                    truncate_for_hint(&body)
                ),
            });
        }

        // Step 4: provenance row. Tier 2 sources emit under
        // `Capability::Metadata` per `docs/PROVENANCE_LOG.md` §3.
        // ADR-0021 §1 canonical-digest: promote the ref under the
        // "openalex" resolver profile.
        let canonical = ref_.promote(self.name(), None).digest_hex();
        ctx.log.append(RowInput {
            event: LogEvent::Fetch,
            result: LogResult::Ok,
            capability: Capability::Metadata,
            ref_: Some(doi.as_str()),
            source: Some(self.name()),
            error_code: None,
            size_bytes: Some(body.len() as u64),
            license: None,
            store_path: None,
            canonical_digest: Some(&canonical),
        })?;

        Ok(FetchResult {
            source: self.name().to_string(),
            // OpenAlex Work records carry a `best_oa_location.license`
            // field but a missing-or-null value is the common case
            // for non-OA works. Surface it via `metadata_json` and let
            // the orchestrator decide; report a neutral marker here.
            license: "unknown".into(),
            // Metadata-only contract (docs/SOURCES.md §4).
            pdf_bytes: None,
            final_url: Some(final_url),
            metadata_json: Some(work),
        })
    }
}

/// Truncate a response body to a short prefix for inclusion in error
/// hints. Avoids dumping a multi-KB payload into a single log line
/// when the response is malformed; 200 chars is enough to identify the
/// shape (HTML 404 page vs. JSON error envelope vs. truncated work).
fn truncate_for_hint(body: &[u8]) -> String {
    const MAX: usize = 200;
    let s = String::from_utf8_lossy(body);
    if s.len() <= MAX {
        s.into_owned()
    } else {
        format!("{}…", &s[..MAX])
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    use std::sync::Arc;

    use camino::Utf8PathBuf;
    use tempfile::TempDir;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::http::HttpClient;
    use crate::provenance::ProvenanceLog;
    use crate::rate_limiter::RateLimiter;
    use crate::{ArxivId, CapabilityProfile, Doi, MetadataAccess, RateLimits, Ref};

    /// Hand-crafted (not a snapshot) OpenAlex Work record. Kept small
    /// and synthetic to avoid third-party redistribution concerns.
    const SAMPLE_WORK: &str = r#"{
        "id": "https://openalex.org/W2741809807",
        "doi": "https://doi.org/10.1234/example",
        "display_name": "Example Work Title",
        "publication_year": 2024,
        "referenced_works": [
            "https://openalex.org/W2000000001",
            "https://openalex.org/W2000000002"
        ]
    }"#;

    fn build_test_context(wiremock_host: &str) -> (TempDir, FetchContext) {
        let td = TempDir::new().expect("tempdir");
        let log_dir =
            Utf8PathBuf::try_from(td.path().to_path_buf()).expect("temp dir path must be UTF-8");
        let log_path = log_dir.join("test.jsonl");

        let http = Arc::new(HttpClient::new_for_tests_allow_http(
            "openalex",
            wiremock_host,
        ));
        let rate_limiter = Arc::new(RateLimiter::new(RateLimits::HARD_CODED));
        let session_id = "01J0000000000000000000TEST".to_string();
        let log = Arc::new(
            ProvenanceLog::open(log_path, session_id.clone()).expect("provenance log opens"),
        );
        let ctx = FetchContext {
            http,
            rate_limiter,
            log,
            session_id,
            cache_root: None,
        };
        (td, ctx)
    }

    fn profile_with_openalex_enabled() -> CapabilityProfile {
        // Build a clean profile, then flip the openalex flag. We avoid
        // touching the real env vars so the test runs single-threaded
        // without `serial_test`.
        let mut p = CapabilityProfile::from_env().expect("clean env never errors");
        p.metadata = MetadataAccess {
            openalex: true,
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

    #[tokio::test]
    async fn fetch_doi_returns_work_metadata() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/works/doi:10.1234/example"))
            .and(query_param("mailto", "doiget@localhost"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_WORK))
            .mount(&server)
            .await;

        let (_td, ctx) = build_test_context(&server.uri());
        let src = OpenalexSource::with_base(
            Url::parse(&server.uri()).expect("wiremock URI parses"),
            "doiget@localhost".to_string(),
        );
        let profile = profile_with_openalex_enabled();
        let ref_ = Ref::Doi(Doi::parse("10.1234/example").expect("DOI parses"));

        let result = src.fetch(&ref_, &profile, &ctx).await.expect("fetch ok");
        assert_eq!(result.source, "openalex");
        assert!(result.pdf_bytes.is_none(), "metadata-only contract");
        let meta = result.metadata_json.expect("metadata_json present");
        assert_eq!(meta["display_name"], "Example Work Title");
        assert_eq!(
            meta["referenced_works"][0],
            "https://openalex.org/W2000000001"
        );
    }

    #[tokio::test]
    async fn fetch_arxiv_id_is_not_eligible() {
        let (_td, ctx) = build_test_context("http://127.0.0.1:1");
        let src = OpenalexSource::with_base(
            Url::parse("http://127.0.0.1:1").expect("URI parses"),
            "doiget@localhost".to_string(),
        );
        let profile = profile_with_openalex_enabled();
        let ref_ = Ref::Arxiv(ArxivId::parse("2401.12345").expect("arXiv id parses"));

        let err = src
            .fetch(&ref_, &profile, &ctx)
            .await
            .expect_err("arXiv ref must be rejected");
        assert!(matches!(err, FetchError::NotEligible { .. }));
    }

    #[tokio::test]
    async fn fetch_without_capability_flag_is_not_eligible() {
        let (_td, ctx) = build_test_context("http://127.0.0.1:1");
        let src = OpenalexSource::with_base(
            Url::parse("http://127.0.0.1:1").expect("URI parses"),
            "doiget@localhost".to_string(),
        );
        // Profile with metadata.openalex == false (default).
        let profile = CapabilityProfile::from_env().expect("clean env never errors");
        let ref_ = Ref::Doi(Doi::parse("10.1234/example").expect("DOI parses"));

        assert!(
            !src.can_serve(&profile, &ref_),
            "can_serve must be false without DOIGET_ENABLE_OPENALEX"
        );
        let err = src
            .fetch(&ref_, &profile, &ctx)
            .await
            .expect_err("fetch must reject when capability is denied");
        assert!(matches!(err, FetchError::NotEligible { .. }));
    }

    #[tokio::test]
    async fn fetch_malformed_response_returns_source_schema_error() {
        let server = MockServer::start().await;
        // Response has no `id` field — defensive shape check trips.
        Mock::given(method("GET"))
            .and(path("/works/doi:10.1234/example"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"error":"not found"}"#))
            .mount(&server)
            .await;

        let (_td, ctx) = build_test_context(&server.uri());
        let src = OpenalexSource::with_base(
            Url::parse(&server.uri()).expect("wiremock URI parses"),
            "doiget@localhost".to_string(),
        );
        let profile = profile_with_openalex_enabled();
        let ref_ = Ref::Doi(Doi::parse("10.1234/example").expect("DOI parses"));

        let err = src
            .fetch(&ref_, &profile, &ctx)
            .await
            .expect_err("missing `id` must surface as SourceSchema");
        assert!(matches!(err, FetchError::SourceSchema { .. }));
    }
}
