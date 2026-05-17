//! Semantic Scholar source — DOI metadata enrichment (Phase 4 / Tier 2).
//!
//! Spec: `docs/SOURCES.md` §1 Tier 2 row + §4. Semantic Scholar's Graph
//! API accepts unauthenticated polite-pool calls; an optional
//! `x-api-key` header raises rate limits. doiget reads the key from
//! `DOIGET_S2_API_KEY` at the orchestrator boundary; when present it is
//! sent as the `x-api-key` request header (issue #156 ① — previously
//! the field was carried but never sent, silently throttling keyed
//! users to the anonymous tier); when absent the request is sent
//! unauthenticated.
//!
//! ## Capability gate
//!
//! [`S2Source::can_serve`] returns `true` only when
//! [`CapabilityProfile.metadata.semantic_scholar`](crate::CapabilityProfile)
//! is `true` AND the ref is a [`Ref::Doi`]. The metadata bool is set
//! by [`CapabilityProfile::from_env`] from `DOIGET_ENABLE_S2`.
//!
//! ## Metadata-only contract
//!
//! Per `docs/SOURCES.md` §4 this source never returns PDF bytes
//! (`FetchResult.pdf_bytes` is always `None`). The `references` array
//! in the response is consumed by the citation graph orchestrator
//! (Slice 14); the IDs there are S2 paper IDs, not DOIs.

use async_trait::async_trait;
use url::Url;

use crate::provenance::{Capability, LogEvent, LogResult, RowInput};
use crate::source::{FetchContext, FetchError, FetchResult, Source};
use crate::{CapabilityProfile, Ref};

/// Production Semantic Scholar Graph API base URL.
const DEFAULT_BASE: &str = "https://api.semanticscholar.org";

/// Default `fields` query parameter — keep the response small but
/// include `references` so the citation graph orchestrator can walk
/// outgoing citations without a follow-up request.
const DEFAULT_FIELDS: &str = "title,year,citationCount,references";

/// Semantic Scholar [`Source`] impl — DOI → Graph-API paper record.
#[derive(Clone, Debug)]
pub struct S2Source {
    /// API base URL. Production constructor pins
    /// `https://api.semanticscholar.org`; [`with_base`](Self::with_base)
    /// lets wiremock substitute an `http://127.0.0.1:N` origin.
    base: Url,
    /// Optional API key (S2 raises rate limits when present). Absent
    /// means the request is sent unauthenticated, which the API
    /// explicitly allows under the public rate-limit ceiling.
    ///
    /// When present it is sent as the `x-api-key` request header via
    /// [`HttpClient::fetch_bytes_with_headers`] (issue #156 ①),
    /// mirroring how the APS TDM source attaches `X-API-Key`. The value
    /// is sent on the wire only — it is never logged, recorded in
    /// provenance, or echoed back in error strings.
    api_key: Option<String>,
}

impl S2Source {
    /// Production constructor. `api_key` is `Option` because S2's
    /// Graph API is usable without one (subject to the public
    /// rate-limit ceiling).
    #[must_use]
    pub fn new(api_key: Option<String>) -> Self {
        Self {
            #[allow(clippy::expect_used)]
            base: Url::parse(DEFAULT_BASE).expect("hard-coded base URL is valid"),
            api_key,
        }
    }

    /// Test-only constructor accepting an arbitrary base URL.
    pub fn with_base(base: Url, api_key: Option<String>) -> Self {
        Self { base, api_key }
    }

    /// Build the `/graph/v1/paper/DOI:<doi>?fields=...` URL. S2's
    /// `/paper/{id}` endpoint accepts DOI-prefixed IDs in the form
    /// `DOI:10.1234/foo`.
    fn request_url(&self, doi: &crate::Doi) -> Result<Url, FetchError> {
        let path = format!("/graph/v1/paper/DOI:{}", doi.as_str());
        let mut url = self
            .base
            .join(&path)
            .map_err(|e| FetchError::SourceSchema {
                hint: format!("s2 URL construction failed: {e}"),
            })?;
        url.query_pairs_mut().append_pair("fields", DEFAULT_FIELDS);
        Ok(url)
    }
}

#[async_trait]
impl Source for S2Source {
    fn name(&self) -> &str {
        "semantic_scholar"
    }

    fn can_serve(&self, profile: &CapabilityProfile, ref_: &Ref) -> bool {
        profile.metadata.semantic_scholar && matches!(ref_, Ref::Doi(_))
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
                    source_key: "semantic_scholar".into(),
                });
            }
        };

        if !profile.metadata.semantic_scholar {
            return Err(FetchError::NotEligible {
                source_key: "semantic_scholar".into(),
            });
        }

        let _permit = ctx.rate_limiter.acquire(self.name()).await;

        let url = self.request_url(doi)?;
        // Issue #156 ①: when an API key is configured, send it as the
        // `x-api-key` header so S2 applies the keyed rate-limit tier
        // instead of the anonymous ceiling. Mirrors the APS TDM
        // source's `X-API-Key` wiring. An empty string is treated as
        // "no key" (it cannot authenticate and would only add a
        // useless header). The header value is sent on the wire only —
        // never logged or echoed.
        let api_key = self.api_key.as_deref().filter(|k| !k.is_empty());
        let (body, final_url) = match api_key {
            Some(key) => {
                ctx.http
                    .fetch_bytes_with_headers(self.name(), url, &[("x-api-key", key)])
                    .await?
            }
            None => ctx.http.fetch_bytes(self.name(), url).await?,
        };

        let paper: serde_json::Value =
            serde_json::from_slice(&body).map_err(|e| FetchError::SourceSchema {
                hint: format!("s2 returned non-JSON: {e}"),
            })?;
        if paper.get("paperId").is_none() {
            return Err(FetchError::SourceSchema {
                hint: format!(
                    "s2 response missing `paperId` field — likely an error \
                     payload (got: {})",
                    truncate_for_hint(&body)
                ),
            });
        }

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
            license: "unknown".into(),
            pdf_bytes: None,
            final_url: Some(final_url),
            metadata_json: Some(paper),
        })
    }
}

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
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::http::HttpClient;
    use crate::provenance::ProvenanceLog;
    use crate::rate_limiter::RateLimiter;
    use crate::{CapabilityProfile, Doi, MetadataAccess, RateLimits, Ref};

    const SAMPLE_PAPER: &str = r#"{
        "paperId": "0123456789abcdef",
        "title": "Example S2 Paper",
        "year": 2024,
        "citationCount": 42,
        "references": [
            {"paperId": "abc111", "title": "Cited Paper A"},
            {"paperId": "abc222", "title": "Cited Paper B"}
        ]
    }"#;

    fn build_test_context(wiremock_host: &str) -> (TempDir, FetchContext) {
        let td = TempDir::new().expect("tempdir");
        let log_dir =
            Utf8PathBuf::try_from(td.path().to_path_buf()).expect("temp dir path must be UTF-8");
        let log_path = log_dir.join("test.jsonl");

        let http = Arc::new(HttpClient::new_for_tests_allow_http(
            "semantic_scholar",
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
        };
        (td, ctx)
    }

    fn profile_with_s2_enabled() -> CapabilityProfile {
        let mut p = CapabilityProfile::from_env().expect("clean env never errors");
        p.metadata = MetadataAccess {
            openalex: false,
            semantic_scholar: true,
            doaj: false,
        };
        p
    }

    #[tokio::test]
    async fn fetch_doi_returns_paper_metadata() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/graph/v1/paper/DOI:10.1234/example"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_PAPER))
            .mount(&server)
            .await;

        let (_td, ctx) = build_test_context(&server.uri());
        let src = S2Source::with_base(
            Url::parse(&server.uri()).expect("wiremock URI parses"),
            None,
        );
        let profile = profile_with_s2_enabled();
        let ref_ = Ref::Doi(Doi::parse("10.1234/example").expect("DOI parses"));

        let result = src.fetch(&ref_, &profile, &ctx).await.expect("fetch ok");
        assert_eq!(result.source, "semantic_scholar");
        assert!(result.pdf_bytes.is_none(), "metadata-only contract");
        let meta = result.metadata_json.expect("metadata_json present");
        assert_eq!(meta["title"], "Example S2 Paper");
        assert_eq!(meta["references"][0]["paperId"], "abc111");
    }

    #[tokio::test]
    async fn fetch_sends_x_api_key_header_when_key_present() {
        // Issue #156 ①: a configured key must reach the wire as the
        // `x-api-key` header so S2 applies the keyed rate-limit tier.
        const S2_KEY: &str = "s2-test-key-abc";
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/graph/v1/paper/DOI:10.1234/example"))
            .and(header("x-api-key", S2_KEY))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_PAPER))
            .mount(&server)
            .await;

        let (_td, ctx) = build_test_context(&server.uri());
        let src = S2Source::with_base(
            Url::parse(&server.uri()).expect("wiremock URI parses"),
            Some(S2_KEY.to_string()),
        );
        let profile = profile_with_s2_enabled();
        let ref_ = Ref::Doi(Doi::parse("10.1234/example").expect("DOI parses"));

        let result = src.fetch(&ref_, &profile, &ctx).await.expect("fetch ok");
        assert_eq!(result.source, "semantic_scholar");
        let meta = result.metadata_json.expect("metadata_json present");
        assert_eq!(meta["title"], "Example S2 Paper");
    }

    #[tokio::test]
    async fn fetch_without_capability_flag_is_not_eligible() {
        let (_td, ctx) = build_test_context("http://127.0.0.1:1");
        let src = S2Source::with_base(Url::parse("http://127.0.0.1:1").expect("URI parses"), None);
        let profile = CapabilityProfile::from_env().expect("clean env never errors");
        let ref_ = Ref::Doi(Doi::parse("10.1234/example").expect("DOI parses"));

        assert!(
            !src.can_serve(&profile, &ref_),
            "can_serve must be false without DOIGET_ENABLE_S2"
        );
        let err = src
            .fetch(&ref_, &profile, &ctx)
            .await
            .expect_err("fetch must reject when capability is denied");
        assert!(matches!(err, FetchError::NotEligible { .. }));
    }
}
