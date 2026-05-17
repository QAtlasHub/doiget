//! Elsevier ScienceDirect TDM source — DOI metadata via the
//! `/content/article/doi/<DOI>` endpoint (Phase 5c / Tier 3).
//!
//! Spec: `docs/SOURCES.md` §1 Tier 3 row + §4 "TDM sources (Phase 5)",
//! `docs/CAPABILITY.md` §2, ADR-0002 (per-publisher Cargo features),
//! ADR-0019 (eight safeguards).
//!
//! Whole module gated by `#[cfg(feature = "tdm-elsevier")]` so
//! default release binaries never include the host pattern or the
//! env-var read path (ADR-0002).
//!
//! ## Three-gate activation
//!
//! Per `docs/CAPABILITY.md` §2 a fetch only succeeds when ALL THREE
//! gates pass:
//!
//! 1. The binary was built with `--features tdm-elsevier`.
//! 2. The user set `DOIGET_KEY_ELSEVIER=<api-key>`.
//! 3. The user set `DOIGET_AGREE_TDM_ELSEVIER=1`.
//!
//! Gates 2 + 3 are checked at startup by
//! [`CapabilityProfile::from_env`] and surface as
//! `profile.tdm_elsevier = Some(TdmGrant)`. This source mirrors that
//! check in [`can_serve`](TdmElsevierSource::can_serve) and again in
//! [`fetch`](TdmElsevierSource::fetch) (defensive). Elsevier expects
//! the key in the `X-ELS-APIKey` header. The key is read once at
//! startup and carried in [`TdmGrant::api_key`](crate::TdmGrant)
//! (issue #153); the source consumes it from the grant and never
//! re-reads `DOIGET_KEY_ELSEVIER` at fetch time.
//!
//! ## Metadata-only contract (Phase 5c)
//!
//! `FetchResult.pdf_bytes` is always `None`. Elsevier's full-text
//! retrieval endpoint can stream PDF bytes, but Phase 5c
//! deliberately stays metadata-only — fetching PDFs requires the
//! eight ADR-0019 safeguards wired through the orchestrator, which
//! lands later in Phase 5.

#![cfg(feature = "tdm-elsevier")]

use async_trait::async_trait;
use secrecy::ExposeSecret;
use url::Url;

use crate::provenance::{Capability, LogEvent, LogResult, RowInput};
use crate::source::{FetchContext, FetchError, FetchResult, Source};
use crate::{CapabilityProfile, Ref};

/// Production Elsevier ScienceDirect API base URL.
const DEFAULT_BASE: &str = "https://api.elsevier.com";

/// Elsevier ScienceDirect [`Source`] impl — DOI →
/// `/content/article/doi/<DOI>?httpAccept=application/json` JSON
/// record.
#[derive(Clone, Debug)]
pub struct TdmElsevierSource {
    /// API base URL. Production pins `https://api.elsevier.com`;
    /// [`with_base`](Self::with_base) lets wiremock substitute an
    /// `http://127.0.0.1:N` origin.
    base: Url,
}

impl TdmElsevierSource {
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

    /// Build the `/content/article/doi/<doi>?httpAccept=application/json`
    /// URL.
    ///
    /// Elsevier encodes the DOI directly in the path; the `/`
    /// separator in the DOI suffix must be percent-encoded. The
    /// `httpAccept` query parameter is the documented way to ask
    /// for the JSON variant of the full-text-retrieval response (a
    /// belt-and-braces companion to the `Accept` header).
    fn request_url(&self, doi: &crate::Doi) -> Result<Url, FetchError> {
        let path = format!(
            "/content/article/doi/{}",
            percent_encode_path_segment(doi.as_str())
        );
        let mut url = self
            .base
            .join(&path)
            .map_err(|e| FetchError::SourceSchema {
                hint: format!("tdm-elsevier URL construction failed: {e}"),
            })?;
        url.query_pairs_mut()
            .append_pair("httpAccept", "application/json");
        Ok(url)
    }
}

impl Default for TdmElsevierSource {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Source for TdmElsevierSource {
    fn name(&self) -> &str {
        "tdm-elsevier"
    }

    fn can_serve(&self, profile: &CapabilityProfile, ref_: &Ref) -> bool {
        profile.tdm_elsevier.is_some() && matches!(ref_, Ref::Doi(_))
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
                    source_key: "tdm-elsevier".into(),
                });
            }
        };

        // Defensive gate (1/3 + 2/3): the runtime grant must be
        // populated and now carries the key validated at startup
        // (issue #153). `CapabilityProfile` is immutable for the
        // process lifetime (`docs/CAPABILITY.md` §6), so the startup
        // grant is the single source of truth — no env re-read.
        let grant = profile
            .tdm_elsevier
            .as_ref()
            .ok_or_else(|| FetchError::NotEligible {
                source_key: "tdm-elsevier".into(),
            })?;
        let api_key = grant.api_key.expose_secret();
        if api_key.is_empty() {
            return Err(FetchError::NotEligible {
                source_key: "tdm-elsevier".into(),
            });
        }

        let _permit = ctx.rate_limiter.acquire(self.name()).await;

        let url = self.request_url(doi)?;
        // Elsevier authenticates via the `X-ELS-APIKey` request
        // header (Slice 20 wiring). The header value is sent on
        // the wire only; it is never logged or echoed back.
        let (body, final_url) = ctx
            .http
            .fetch_bytes_with_headers(self.name(), url, &[("X-ELS-APIKey", api_key)])
            .await?;

        let envelope: serde_json::Value =
            serde_json::from_slice(&body).map_err(|e| FetchError::SourceSchema {
                hint: format!("tdm-elsevier returned non-JSON: {e}"),
            })?;

        // Elsevier full-text-retrieval JSON envelope is shaped as:
        //   { "full-text-retrieval-response": { "coredata": {...}, ... } }
        // Surface a SourceSchema error if the top-level wrapper is
        // missing — the orchestrator falls through to the next
        // source.
        let response = envelope
            .get("full-text-retrieval-response")
            .ok_or_else(|| FetchError::SourceSchema {
                hint: format!(
                    "tdm-elsevier response missing `full-text-retrieval-response` (got: {})",
                    truncate_for_hint(&body)
                ),
            })?;
        if response.get("coredata").is_none() {
            return Err(FetchError::SourceSchema {
                hint: "tdm-elsevier response missing `coredata`".to_string(),
            });
        }

        let canonical = ref_.promote(self.name(), None).digest_hex();
        ctx.log.append(RowInput {
            event: LogEvent::Fetch,
            result: LogResult::Ok,
            capability: Capability::TdmElsevier,
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
            metadata_json: Some(response.clone()),
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
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::http::HttpClient;
    use crate::provenance::ProvenanceLog;
    use crate::rate_limiter::RateLimiter;
    use crate::{CapabilityProfile, Doi, RateLimits, Ref, TdmGrant};

    const SAMPLE_ARTICLE_HIT: &str = r#"{
        "full-text-retrieval-response": {
            "coredata": {
                "prism:doi": "10.1016/j.example.2024.001",
                "dc:title": "Example Elsevier Article",
                "prism:publicationName": "Example Journal"
            }
        }
    }"#;

    const SAMPLE_MISSING_WRAPPER: &str = r#"{"error": "not authorized"}"#;

    fn build_test_context(wiremock_host: &str) -> (TempDir, FetchContext) {
        let td = TempDir::new().expect("tempdir");
        let log_dir =
            Utf8PathBuf::try_from(td.path().to_path_buf()).expect("temp dir path must be UTF-8");
        let log_path = log_dir.join("test.jsonl");

        let http = Arc::new(HttpClient::new_for_tests_allow_http(
            "tdm-elsevier",
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

    const TEST_KEY: &str = "els-test-key-xyz";

    fn profile_with_elsevier_grant() -> CapabilityProfile {
        let mut p = CapabilityProfile::from_env().expect("clean env never errors");
        p.tdm_elsevier = Some(TdmGrant {
            // Issue #153: key flows through the grant, not the env var.
            api_key: secrecy::SecretString::from(TEST_KEY.to_string()),
            agree_env_var: "DOIGET_AGREE_TDM_ELSEVIER".to_string(),
            ..Default::default()
        });
        p
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn fetch_doi_returns_full_text_retrieval_response() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            // Elsevier path with percent-encoded DOI (`/` → `%2F`).
            .and(path("/content/article/doi/10.1016%2Fj.example.2024.001"))
            .and(query_param("httpAccept", "application/json"))
            // Slice 20: X-ELS-APIKey header MUST be present on the wire.
            .and(header("x-els-apikey", TEST_KEY))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_ARTICLE_HIT))
            .mount(&server)
            .await;

        let (_td, ctx) = build_test_context(&server.uri());
        let src =
            TdmElsevierSource::with_base(Url::parse(&server.uri()).expect("wiremock URI parses"));
        let profile = profile_with_elsevier_grant();
        let ref_ = Ref::Doi(Doi::parse("10.1016/j.example.2024.001").expect("DOI parses"));

        let result = src.fetch(&ref_, &profile, &ctx).await.expect("fetch ok");

        assert_eq!(result.source, "tdm-elsevier");
        assert!(result.pdf_bytes.is_none(), "metadata-only contract");
        let meta = result.metadata_json.expect("metadata_json present");
        assert_eq!(meta["coredata"]["dc:title"], "Example Elsevier Article");
        assert_eq!(meta["coredata"]["prism:doi"], "10.1016/j.example.2024.001");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn fetch_without_grant_is_not_eligible() {
        let (_td, ctx) = build_test_context("http://127.0.0.1:1");
        let src = TdmElsevierSource::with_base(Url::parse("http://127.0.0.1:1").expect("parses"));
        let profile = CapabilityProfile::from_env().expect("clean env never errors");
        let ref_ = Ref::Doi(Doi::parse("10.1016/j.example.2024.001").expect("DOI parses"));

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
    async fn fetch_missing_wrapper_returns_source_schema() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/content/article/doi/10.1016%2Fj.example.2024.001"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_MISSING_WRAPPER))
            .mount(&server)
            .await;

        let (_td, ctx) = build_test_context(&server.uri());
        let src =
            TdmElsevierSource::with_base(Url::parse(&server.uri()).expect("wiremock URI parses"));
        let profile = profile_with_elsevier_grant();
        let ref_ = Ref::Doi(Doi::parse("10.1016/j.example.2024.001").expect("DOI parses"));

        let result = src.fetch(&ref_, &profile, &ctx).await;

        let err = result.expect_err("missing wrapper must surface as SourceSchema");
        assert!(matches!(err, FetchError::SourceSchema { .. }));
    }
}
