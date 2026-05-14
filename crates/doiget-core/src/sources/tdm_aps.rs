//! APS Harvest TDM source — DOI metadata via the
//! `/v2/article/<DOI>` endpoint (Phase 5b / Tier 3).
//!
//! Spec: `docs/SOURCES.md` §1 Tier 3 row + §4 "TDM sources (Phase 5)",
//! `docs/CAPABILITY.md` §2, ADR-0002 (per-publisher Cargo features),
//! ADR-0019 (eight safeguards).
//!
//! Whole module gated by `#[cfg(feature = "tdm-aps")]` so default
//! release binaries never include the host pattern or the env-var
//! read path (ADR-0002).
//!
//! ## Three-gate activation
//!
//! Per `docs/CAPABILITY.md` §2 a fetch only succeeds when ALL THREE
//! gates pass:
//!
//! 1. The binary was built with `--features tdm-aps`.
//! 2. The user set `DOIGET_KEY_APS=<api-key>`.
//! 3. The user set `DOIGET_AGREE_TDM_APS=1`.
//!
//! Gates 2 + 3 are checked at startup by
//! [`CapabilityProfile::from_env`] and surface as
//! `profile.tdm_aps = Some(TdmGrant)`. This source mirrors that
//! check in [`can_serve`](TdmApsSource::can_serve) and again in
//! [`fetch`](TdmApsSource::fetch) (defensive). APS expects the key
//! in the `X-API-Key` header (not a URL parameter, unlike Springer);
//! `TdmGrant` does not currently store the key, so the source
//! re-reads `DOIGET_KEY_APS` at fetch time.
//!
//! ## Metadata-only contract (Phase 5b)
//!
//! `FetchResult.pdf_bytes` is always `None`. APS Harvest exposes
//! full-text article-manifest URLs in the response, but Phase 5b
//! deliberately stays metadata-only — fetching those payloads
//! requires the eight ADR-0019 safeguards wired through the
//! orchestrator, which lands later in Phase 5.

#![cfg(feature = "tdm-aps")]

use async_trait::async_trait;
use url::Url;

use crate::provenance::{Capability, LogEvent, LogResult, RowInput};
use crate::source::{FetchContext, FetchError, FetchResult, Source};
use crate::{CapabilityProfile, Ref};

/// Production APS Harvest API base URL.
const DEFAULT_BASE: &str = "https://harvest.aps.org";

/// Env var holding the per-tenant API key. Presence is one of the
/// three activation gates (`docs/CAPABILITY.md` §2); the value is
/// read at fetch time and would be sent as the `X-API-Key` header
/// once `HttpClient` grows a per-source header hook (TODO in
/// `fetch`).
const KEY_ENV_VAR: &str = "DOIGET_KEY_APS";

/// APS Harvest [`Source`] impl — DOI → `/v2/article/<DOI>` JSON
/// record.
#[derive(Clone, Debug)]
pub struct TdmApsSource {
    /// API base URL. Production pins `https://harvest.aps.org`;
    /// [`with_base`](Self::with_base) lets wiremock substitute an
    /// `http://127.0.0.1:N` origin.
    base: Url,
}

impl TdmApsSource {
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

    /// Build the `/v2/article/<doi>` URL.
    ///
    /// APS encodes the DOI directly in the path; the `/` separator
    /// in the DOI suffix must be percent-encoded.
    fn request_url(&self, doi: &crate::Doi) -> Result<Url, FetchError> {
        let path = format!(
            "/v2/article/{}",
            percent_encode_path_segment(doi.as_str())
        );
        self.base
            .join(&path)
            .map_err(|e| FetchError::SourceSchema {
                hint: format!("tdm-aps URL construction failed: {e}"),
            })
    }
}

impl Default for TdmApsSource {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Source for TdmApsSource {
    fn name(&self) -> &str {
        "tdm-aps"
    }

    fn can_serve(&self, profile: &CapabilityProfile, ref_: &Ref) -> bool {
        profile.tdm_aps.is_some() && matches!(ref_, Ref::Doi(_))
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
                    source_key: "tdm-aps".into(),
                });
            }
        };

        // Defensive gate (1/3): runtime grant must be populated.
        if profile.tdm_aps.is_none() {
            return Err(FetchError::NotEligible {
                source_key: "tdm-aps".into(),
            });
        }

        // Defensive gate (2/3): re-read the key env var. A missing
        // key here means env changed mid-process — fail-close as
        // NotEligible so the orchestrator can fall through.
        //
        // NOTE: Phase 5b relies on the shared `FetchContext.http`
        // for the GET, which does not yet surface a per-source
        // header hook. The `X-API-Key` header is therefore NOT
        // attached on the wire in this slice; the call would 401 in
        // production until the HttpClient is extended (the same
        // hook Slice 19 / Elsevier needs). The wiremock test below
        // exercises the path with header matching disabled. See
        // `docs/SOURCES.md` §4 "TDM sources (Phase 5)" follow-up.
        let api_key = std::env::var(KEY_ENV_VAR).map_err(|_| FetchError::NotEligible {
            source_key: "tdm-aps".into(),
        })?;
        if api_key.is_empty() {
            return Err(FetchError::NotEligible {
                source_key: "tdm-aps".into(),
            });
        }

        let _permit = ctx.rate_limiter.acquire(self.name()).await;

        let url = self.request_url(doi)?;
        let (body, final_url) = ctx.http.fetch_bytes(self.name(), url).await?;

        let envelope: serde_json::Value =
            serde_json::from_slice(&body).map_err(|e| FetchError::SourceSchema {
                hint: format!("tdm-aps returned non-JSON: {e}"),
            })?;

        // APS Harvest article envelope: { id, doi, title, ..., links: [...] }.
        // Shape check: must be a JSON object with at least a `doi`
        // or `id` field.
        if !envelope.is_object() {
            return Err(FetchError::SourceSchema {
                hint: format!(
                    "tdm-aps response is not a JSON object (got: {})",
                    truncate_for_hint(&body)
                ),
            });
        }
        if envelope.get("doi").is_none() && envelope.get("id").is_none() {
            return Err(FetchError::SourceSchema {
                hint: "tdm-aps response missing both `doi` and `id` fields".to_string(),
            });
        }

        let canonical = ref_.promote(self.name(), None).digest_hex();
        ctx.log.append(RowInput {
            event: LogEvent::Fetch,
            result: LogResult::Ok,
            capability: Capability::TdmAps,
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
            metadata_json: Some(envelope),
        })
    }
}

/// Percent-encode a path segment, preserving the RFC 3986 unreserved
/// set. `/` and other reserved characters are percent-encoded.
fn percent_encode_path_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for b in segment.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
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
    use crate::{CapabilityProfile, Doi, RateLimits, Ref, TdmGrant};

    const SAMPLE_ARTICLE_HIT: &str = r#"{
        "id": "PhysRevX.10.011001",
        "doi": "10.1103/PhysRevX.10.011001",
        "title": "Example APS Harvest Article",
        "journal": "Physical Review X"
    }"#;

    const SAMPLE_BAD_SHAPE: &str = r#"[1, 2, 3]"#;

    fn build_test_context(wiremock_host: &str) -> (TempDir, FetchContext) {
        let td = TempDir::new().expect("tempdir");
        let log_dir =
            Utf8PathBuf::try_from(td.path().to_path_buf()).expect("temp dir path must be UTF-8");
        let log_path = log_dir.join("test.jsonl");

        let http = Arc::new(HttpClient::new_for_tests_allow_http(
            "tdm-aps",
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

    fn profile_with_aps_grant() -> CapabilityProfile {
        let mut p = CapabilityProfile::from_env().expect("clean env never errors");
        p.tdm_aps = Some(TdmGrant {
            agree_env_var: "DOIGET_AGREE_TDM_APS".to_string(),
            ..Default::default()
        });
        p
    }

    const TEST_KEY: &str = "aps-test-key-xyz";

    #[tokio::test]
    #[serial_test::serial]
    async fn fetch_doi_returns_article_object() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            // APS path with percent-encoded DOI (`/` → `%2F`).
            .and(path("/v2/article/10.1103%2FPhysRevX.10.011001"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_ARTICLE_HIT))
            .mount(&server)
            .await;

        let (_td, ctx) = build_test_context(&server.uri());
        let src = TdmApsSource::with_base(Url::parse(&server.uri()).expect("wiremock URI parses"));
        let profile = profile_with_aps_grant();
        let ref_ = Ref::Doi(Doi::parse("10.1103/PhysRevX.10.011001").expect("DOI parses"));

        std::env::set_var(KEY_ENV_VAR, TEST_KEY);
        let result = src.fetch(&ref_, &profile, &ctx).await;
        std::env::remove_var(KEY_ENV_VAR);
        let result = result.expect("fetch ok");

        assert_eq!(result.source, "tdm-aps");
        assert!(result.pdf_bytes.is_none(), "metadata-only contract");
        let meta = result.metadata_json.expect("metadata_json present");
        assert_eq!(meta["title"], "Example APS Harvest Article");
        assert_eq!(meta["doi"], "10.1103/PhysRevX.10.011001");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn fetch_without_grant_is_not_eligible() {
        let (_td, ctx) = build_test_context("http://127.0.0.1:1");
        let src = TdmApsSource::with_base(Url::parse("http://127.0.0.1:1").expect("parses"));
        let profile = CapabilityProfile::from_env().expect("clean env never errors");
        let ref_ = Ref::Doi(Doi::parse("10.1103/PhysRevX.10.011001").expect("DOI parses"));

        assert!(
            !src.can_serve(&profile, &ref_),
            "can_serve must be false without TdmGrant"
        );
        let err = src
            .fetch(&ref_, &profile, &ctx)
            .await
            .expect_err("fetch must reject when grant is absent");
        assert!(matches!(err, FetchError::NotEligible { .. }));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn fetch_non_object_returns_source_schema() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/article/10.1103%2FPhysRevX.10.011001"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_BAD_SHAPE))
            .mount(&server)
            .await;

        let (_td, ctx) = build_test_context(&server.uri());
        let src = TdmApsSource::with_base(Url::parse(&server.uri()).expect("wiremock URI parses"));
        let profile = profile_with_aps_grant();
        let ref_ = Ref::Doi(Doi::parse("10.1103/PhysRevX.10.011001").expect("DOI parses"));

        std::env::set_var(KEY_ENV_VAR, TEST_KEY);
        let result = src.fetch(&ref_, &profile, &ctx).await;
        std::env::remove_var(KEY_ENV_VAR);

        let err = result.expect_err("non-object response must surface as SourceSchema");
        assert!(matches!(err, FetchError::SourceSchema { .. }));
    }
}
