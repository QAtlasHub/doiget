//! Springer Nature OA TDM source — DOI metadata via the
//! `/openaccess/json` endpoint (Phase 5a / Tier 3).
//!
//! Spec: `docs/SOURCES.md` §1 Tier 3 row + §4 "TDM sources (Phase 5)",
//! `docs/CAPABILITY.md` §2, ADR-0002 (per-publisher Cargo features),
//! ADR-0019 (eight safeguards: author opt-in, no detection evasion,
//! per-tenant key, no PDF caching by default, allowlist, etc.).
//!
//! Whole module gated by `#[cfg(feature = "tdm-springer")]` so default
//! release binaries never include the host pattern or the env-var read
//! path (ADR-0002).
//!
//! ## Three-gate activation
//!
//! Per `docs/CAPABILITY.md` §2 a fetch only succeeds when ALL THREE
//! gates pass:
//!
//! 1. The binary was built with `--features tdm-springer`.
//! 2. The user set `DOIGET_KEY_SPRINGER=<api-key>`.
//! 3. The user set `DOIGET_AGREE_TDM_SPRINGER=1`.
//!
//! Gates 2 + 3 are checked at startup by
//! [`CapabilityProfile::from_env`] and surface as
//! `profile.tdm_springer = Some(TdmGrant)`. This source mirrors that
//! check in [`can_serve`](TdmSpringerSource::can_serve) and again in
//! [`fetch`](TdmSpringerSource::fetch) (defensive — the orchestrator
//! is *supposed* to gate on `can_serve` first). The key value itself
//! is not currently stored in `TdmGrant` (see the
//! [`crate::TdmGrant`] doc-comment); this source re-reads
//! `DOIGET_KEY_SPRINGER` at fetch time.
//!
//! ## Metadata-only contract (Phase 5a)
//!
//! `FetchResult.pdf_bytes` is always `None`. Springer's TDM endpoint
//! does expose `openaccess` PDF links in the returned record, but
//! Phase 5a deliberately stays metadata-only — fetching those PDFs
//! requires the eight ADR-0019 safeguards to be wired through the
//! orchestrator, which lands later in Phase 5.

#![cfg(feature = "tdm-springer")]

use async_trait::async_trait;
use url::Url;

use crate::provenance::{Capability, LogEvent, LogResult, RowInput};
use crate::source::{FetchContext, FetchError, FetchResult, Source};
use crate::{CapabilityProfile, Ref};

/// Production Springer Nature TDM API base URL.
const DEFAULT_BASE: &str = "https://api.springernature.com";

/// Env var holding the per-tenant API key. The presence of this var
/// is one of the three activation gates (`docs/CAPABILITY.md` §2);
/// the value is read at fetch time and appended as `?api_key=...`.
const KEY_ENV_VAR: &str = "DOIGET_KEY_SPRINGER";

/// Springer Nature OA TDM [`Source`] impl — DOI → first matching
/// `records[]` entry from `/openaccess/json`.
#[derive(Clone, Debug)]
pub struct TdmSpringerSource {
    /// API base URL. Production pins `https://api.springernature.com`;
    /// [`with_base`](Self::with_base) lets wiremock substitute an
    /// `http://127.0.0.1:N` origin.
    base: Url,
}

impl TdmSpringerSource {
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

    /// Build the `/openaccess/json?q=doi:<doi>&api_key=<key>` URL.
    ///
    /// Springer's TDM endpoint takes a Lucene-style `q=` filter and
    /// the API key as a URL parameter (not a header). `q` and
    /// `api_key` are both URL-encoded via `query_pairs_mut`.
    fn request_url(&self, doi: &crate::Doi, api_key: &str) -> Result<Url, FetchError> {
        let mut url = self
            .base
            .join("/openaccess/json")
            .map_err(|e| FetchError::SourceSchema {
                hint: format!("tdm-springer URL construction failed: {e}"),
            })?;
        url.query_pairs_mut()
            .append_pair("q", &format!("doi:{}", doi.as_str()))
            .append_pair("api_key", api_key);
        Ok(url)
    }
}

impl Default for TdmSpringerSource {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Source for TdmSpringerSource {
    fn name(&self) -> &str {
        "tdm-springer"
    }

    fn can_serve(&self, profile: &CapabilityProfile, ref_: &Ref) -> bool {
        profile.tdm_springer.is_some() && matches!(ref_, Ref::Doi(_))
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
                    source_key: "tdm-springer".into(),
                });
            }
        };

        // Defensive gate (1/3): runtime grant must be populated. The
        // orchestrator is supposed to call `can_serve` first, but we
        // re-check here so a misrouted call still fail-closes per
        // ADR-0019.
        if profile.tdm_springer.is_none() {
            return Err(FetchError::NotEligible {
                source_key: "tdm-springer".into(),
            });
        }

        // Defensive gate (2/3): the api key env var must still be
        // present at fetch time. `TdmGrant` does not yet hold the
        // key value (see the `TdmGrant` doc-comment), so we re-read
        // here. A missing key at this point indicates the env
        // changed mid-process — treat it as NotEligible so the
        // orchestrator falls through to the next source rather than
        // surfacing a confusing schema error.
        let api_key = std::env::var(KEY_ENV_VAR).map_err(|_| FetchError::NotEligible {
            source_key: "tdm-springer".into(),
        })?;
        if api_key.is_empty() {
            return Err(FetchError::NotEligible {
                source_key: "tdm-springer".into(),
            });
        }

        let _permit = ctx.rate_limiter.acquire(self.name()).await;

        let url = self.request_url(doi, &api_key)?;
        let (body, final_url) = ctx.http.fetch_bytes(self.name(), url).await?;

        let envelope: serde_json::Value =
            serde_json::from_slice(&body).map_err(|e| FetchError::SourceSchema {
                hint: format!("tdm-springer returned non-JSON: {e}"),
            })?;

        // Springer TDM envelope: { apiMessage, query, ..., records: [...] }.
        // When the DOI isn't covered, `records` is empty — surface as
        // SourceSchema so the orchestrator falls through.
        let records = envelope
            .get("records")
            .and_then(|r| r.as_array())
            .ok_or_else(|| FetchError::SourceSchema {
                hint: format!(
                    "tdm-springer response missing `records` array (got: {})",
                    truncate_for_hint(&body)
                ),
            })?;
        let first = records.first().ok_or_else(|| FetchError::SourceSchema {
            hint: "tdm-springer returned 0 records for this DOI".to_string(),
        })?;

        let canonical = ref_.promote(self.name(), None).digest_hex();
        ctx.log.append(RowInput {
            event: LogEvent::Fetch,
            result: LogResult::Ok,
            capability: Capability::TdmSpringer,
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
    use crate::{CapabilityProfile, Doi, RateLimits, Ref, TdmGrant};

    const SAMPLE_ENVELOPE_HIT: &str = r#"{
        "apiMessage": "ok",
        "query": "doi:10.1234/example",
        "records": [
            {
                "identifier": "doi:10.1234/example",
                "title": "Example Springer OA Article",
                "publicationName": "Example Journal",
                "openaccess": "true"
            }
        ]
    }"#;

    const SAMPLE_ENVELOPE_EMPTY: &str = r#"{
        "apiMessage": "ok",
        "query": "doi:10.1234/example",
        "records": []
    }"#;

    fn build_test_context(wiremock_host: &str) -> (TempDir, FetchContext) {
        let td = TempDir::new().expect("tempdir");
        let log_dir =
            Utf8PathBuf::try_from(td.path().to_path_buf()).expect("temp dir path must be UTF-8");
        let log_path = log_dir.join("test.jsonl");

        let http = Arc::new(HttpClient::new_for_tests_allow_http(
            "tdm-springer",
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

    fn profile_with_springer_grant() -> CapabilityProfile {
        let mut p = CapabilityProfile::from_env().expect("clean env never errors");
        p.tdm_springer = Some(TdmGrant {
            agree_env_var: "DOIGET_AGREE_TDM_SPRINGER".to_string(),
            ..Default::default()
        });
        p
    }

    /// Sentinel test key used in the happy-path wiremock matcher. The
    /// real env var is set/unset around the test body so it does not
    /// bleed into sibling tests.
    const TEST_KEY: &str = "test-key-xyz";

    #[tokio::test]
    #[serial_test::serial]
    async fn fetch_doi_returns_first_record_and_passes_key_in_query() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/openaccess/json"))
            .and(query_param("q", "doi:10.1234/example"))
            .and(query_param("api_key", TEST_KEY))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_ENVELOPE_HIT))
            .mount(&server)
            .await;

        let (_td, ctx) = build_test_context(&server.uri());
        let src =
            TdmSpringerSource::with_base(Url::parse(&server.uri()).expect("wiremock URI parses"));
        let profile = profile_with_springer_grant();
        let ref_ = Ref::Doi(Doi::parse("10.1234/example").expect("DOI parses"));

        std::env::set_var(KEY_ENV_VAR, TEST_KEY);
        let result = src.fetch(&ref_, &profile, &ctx).await;
        std::env::remove_var(KEY_ENV_VAR);
        let result = result.expect("fetch ok");

        assert_eq!(result.source, "tdm-springer");
        assert!(result.pdf_bytes.is_none(), "metadata-only contract");
        let meta = result.metadata_json.expect("metadata_json present");
        assert_eq!(meta["title"], "Example Springer OA Article");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn fetch_without_grant_is_not_eligible() {
        let (_td, ctx) = build_test_context("http://127.0.0.1:1");
        let src = TdmSpringerSource::with_base(Url::parse("http://127.0.0.1:1").expect("parses"));
        let profile = CapabilityProfile::from_env().expect("clean env never errors");
        let ref_ = Ref::Doi(Doi::parse("10.1234/example").expect("DOI parses"));

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
    async fn fetch_empty_records_returns_source_schema() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/openaccess/json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_ENVELOPE_EMPTY))
            .mount(&server)
            .await;

        let (_td, ctx) = build_test_context(&server.uri());
        let src =
            TdmSpringerSource::with_base(Url::parse(&server.uri()).expect("wiremock URI parses"));
        let profile = profile_with_springer_grant();
        let ref_ = Ref::Doi(Doi::parse("10.1234/example").expect("DOI parses"));

        std::env::set_var(KEY_ENV_VAR, TEST_KEY);
        let result = src.fetch(&ref_, &profile, &ctx).await;
        std::env::remove_var(KEY_ENV_VAR);

        let err = result.expect_err("empty records must surface as SourceSchema");
        assert!(matches!(err, FetchError::SourceSchema { .. }));
    }
}
