//! DOAJ source — DOI metadata via search (Phase 4 / Tier 2).
//!
//! Spec: `docs/SOURCES.md` §1 Tier 2 row + §4. DOAJ (Directory of Open
//! Access Journals) is a journal-centric registry. It has no direct
//! `/article-by-doi/<doi>` endpoint, so doiget queries the article
//! search API with a `doi:<value>` filter and takes the first
//! result.
//!
//! ## Capability gate
//!
//! [`DoajSource::can_serve`] returns `true` only when
//! [`CapabilityProfile.metadata.doaj`](crate::CapabilityProfile)
//! is `true` AND the ref is a [`Ref::Doi`]. The metadata bool is set
//! by [`CapabilityProfile::from_env`] from `DOIGET_ENABLE_DOAJ`.
//!
//! ## Metadata-only contract
//!
//! Per `docs/SOURCES.md` §4 this source never returns PDF bytes
//! (`FetchResult.pdf_bytes` is always `None`). DOAJ does include
//! `bibjson.link[]` entries with `fulltext` URLs, but following those
//! is out of scope here — they would be routed through the existing
//! `oa-publisher` source key, not this one.

use async_trait::async_trait;
use url::Url;

use crate::provenance::{Capability, LogEvent, LogResult, RowInput};
use crate::source::{FetchContext, FetchError, FetchResult, Source};
use crate::{CapabilityProfile, Ref};

/// Production DOAJ API base URL.
const DEFAULT_BASE: &str = "https://doaj.org";

/// DOAJ [`Source`] impl — DOI → first matching DOAJ article record.
#[derive(Clone, Debug)]
pub struct DoajSource {
    /// API base URL. Production pins `https://doaj.org`;
    /// [`with_base`](Self::with_base) lets wiremock substitute an
    /// `http://127.0.0.1:N` origin.
    base: Url,
}

impl DoajSource {
    /// Production constructor.
    #[must_use]
    pub fn new() -> Self {
        Self {
            #[allow(clippy::expect_used)]
            base: Url::parse(DEFAULT_BASE).expect("hard-coded base URL is valid"),
        }
    }

    /// Test-only constructor accepting an arbitrary base URL.
    pub fn with_base(base: Url) -> Self {
        Self { base }
    }

    /// Build the `/api/search/articles/doi:<doi>?pageSize=1` URL.
    ///
    /// DOAJ's article search endpoint accepts Lucene-style queries.
    /// `doi:<value>` matches against the `bibjson.identifier[].id`
    /// field where `type == "doi"`. `pageSize=1` keeps the response
    /// tiny — we always take the first match.
    fn request_url(&self, doi: &crate::Doi) -> Result<Url, FetchError> {
        // The query value must be URL-encoded — the `:` separator
        // after `doi` is part of the Lucene syntax (kept literal),
        // but `/` in the DOI suffix must be percent-encoded for the
        // path component.
        let path = format!(
            "/api/search/articles/{}",
            percent_encode_path_segment(&format!("doi:{}", doi.as_str()))
        );
        let mut url = self
            .base
            .join(&path)
            .map_err(|e| FetchError::SourceSchema {
                hint: format!("doaj URL construction failed: {e}"),
            })?;
        url.query_pairs_mut().append_pair("pageSize", "1");
        Ok(url)
    }
}

impl Default for DoajSource {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Source for DoajSource {
    fn name(&self) -> &str {
        "doaj"
    }

    fn can_serve(&self, profile: &CapabilityProfile, ref_: &Ref) -> bool {
        profile.metadata.doaj && matches!(ref_, Ref::Doi(_))
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
                    source_key: "doaj".into(),
                });
            }
        };

        if !profile.metadata.doaj {
            return Err(FetchError::NotEligible {
                source_key: "doaj".into(),
            });
        }

        let _permit = ctx.rate_limiter.acquire(self.name()).await;

        let url = self.request_url(doi)?;
        let (body, final_url) = ctx.http.fetch_bytes(self.name(), url).await?;

        let envelope: serde_json::Value =
            serde_json::from_slice(&body).map_err(|e| FetchError::SourceSchema {
                hint: format!("doaj returned non-JSON: {e}"),
            })?;

        // DOAJ search envelope: { total: N, page: 1, pageSize: 1,
        // results: [...] }. When the DOI isn't in DOAJ, `total == 0`
        // and `results` is empty — surface as SourceSchema so the
        // orchestrator can fall through to the next source.
        let results = envelope
            .get("results")
            .and_then(|r| r.as_array())
            .ok_or_else(|| FetchError::SourceSchema {
                hint: format!(
                    "doaj response missing `results` array (got: {})",
                    truncate_for_hint(&body)
                ),
            })?;
        let first = results.first().ok_or_else(|| FetchError::SourceSchema {
            hint: "doaj search returned 0 results for this DOI".to_string(),
        })?;

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
            metadata_json: Some(first.clone()),
        })
    }
}

/// Percent-encode a path segment, preserving the unreserved set per
/// RFC 3986 plus `:` (Lucene query syntax separator). `/` and other
/// reserved characters are percent-encoded.
fn percent_encode_path_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for b in segment.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b':') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
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
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::http::HttpClient;
    use crate::provenance::ProvenanceLog;
    use crate::rate_limiter::RateLimiter;
    use crate::{CapabilityProfile, Doi, MetadataAccess, RateLimits, Ref};

    const SAMPLE_ENVELOPE_HIT: &str = r#"{
        "total": 1,
        "page": 1,
        "pageSize": 1,
        "results": [
            {
                "id": "abc1234567890",
                "bibjson": {
                    "title": "Example DOAJ Article",
                    "year": "2024",
                    "identifier": [
                        {"type": "doi", "id": "10.1234/example"}
                    ]
                }
            }
        ]
    }"#;

    const SAMPLE_ENVELOPE_EMPTY: &str = r#"{
        "total": 0,
        "page": 1,
        "pageSize": 1,
        "results": []
    }"#;

    fn build_test_context(wiremock_host: &str) -> (TempDir, FetchContext) {
        let td = TempDir::new().expect("tempdir");
        let log_dir =
            Utf8PathBuf::try_from(td.path().to_path_buf()).expect("temp dir path must be UTF-8");
        let log_path = log_dir.join("test.jsonl");

        let http = Arc::new(HttpClient::new_for_tests_allow_http("doaj", wiremock_host));
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

    fn profile_with_doaj_enabled() -> CapabilityProfile {
        let mut p = CapabilityProfile::for_tests();
        p.metadata = MetadataAccess {
            openalex: false,
            semantic_scholar: false,
            doaj: true,
            datacite: false,
            hal: false,
            openaire: false,
            core: false,
            europe_pmc: false,
        };
        p
    }

    #[tokio::test]
    async fn fetch_doi_returns_first_match() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            // DOAJ path with percent-encoded DOI suffix. `:` is kept
            // literal; `/` in the DOI suffix becomes `%2F`.
            .and(path("/api/search/articles/doi:10.1234%2Fexample"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_ENVELOPE_HIT))
            .mount(&server)
            .await;

        let (_td, ctx) = build_test_context(&server.uri());
        let src = DoajSource::with_base(Url::parse(&server.uri()).expect("wiremock URI parses"));
        let profile = profile_with_doaj_enabled();
        let ref_ = Ref::Doi(Doi::parse("10.1234/example").expect("DOI parses"));

        let result = src.fetch(&ref_, &profile, &ctx).await.expect("fetch ok");
        assert_eq!(result.source, "doaj");
        assert!(result.pdf_bytes.is_none(), "metadata-only contract");
        let meta = result.metadata_json.expect("metadata_json present");
        assert_eq!(meta["bibjson"]["title"], "Example DOAJ Article");
    }

    #[tokio::test]
    async fn fetch_empty_results_returns_source_schema() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/search/articles/doi:10.1234%2Fexample"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_ENVELOPE_EMPTY))
            .mount(&server)
            .await;

        let (_td, ctx) = build_test_context(&server.uri());
        let src = DoajSource::with_base(Url::parse(&server.uri()).expect("wiremock URI parses"));
        let profile = profile_with_doaj_enabled();
        let ref_ = Ref::Doi(Doi::parse("10.1234/example").expect("DOI parses"));

        let err = src
            .fetch(&ref_, &profile, &ctx)
            .await
            .expect_err("empty results must surface as SourceSchema");
        assert!(matches!(err, FetchError::SourceSchema { .. }));
    }

    #[tokio::test]
    async fn fetch_without_capability_flag_is_not_eligible() {
        let (_td, ctx) = build_test_context("http://127.0.0.1:1");
        let src = DoajSource::with_base(Url::parse("http://127.0.0.1:1").expect("URI parses"));
        let profile = CapabilityProfile::for_tests();
        let ref_ = Ref::Doi(Doi::parse("10.1234/example").expect("DOI parses"));

        assert!(
            !src.can_serve(&profile, &ref_),
            "can_serve must be false without DOIGET_ENABLE_DOAJ"
        );
        let err = src
            .fetch(&ref_, &profile, &ctx)
            .await
            .expect_err("fetch must reject when capability is denied");
        assert!(matches!(err, FetchError::NotEligible { .. }));
    }
}
