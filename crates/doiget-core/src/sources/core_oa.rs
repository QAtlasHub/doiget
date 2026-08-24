//! CORE source — cross-repository OA aggregation (Tier 2, #417).
//!
//! Spec: `docs/SOURCES.md` §1 Tier 2 row + §4. CORE aggregates OA full
//! text across repositories worldwide and is the broadest single OA index
//! outside Unpaywall, so it sits **last** in the optional chain: the
//! fallback before giving up.
//!
//! The module is `core_oa` rather than `core` because `core` is a Rust
//! built-in crate name, and shadowing it inside `sources::` would make
//! every `core::` path in this module ambiguous.
//!
//! ## Capability gate
//!
//! `CoreSource::can_serve` returns `true` only when
//! [`CapabilityProfile.metadata.core`](crate::CapabilityProfile) is `true`
//! AND the ref is a `Ref::Doi`. The bool is set by
//! `CapabilityProfile::from_env` from `DOIGET_ENABLE_CORE`, which is
//! **off by default** (ADR-0040) — unset, this source is inert.
//!
//! ## The API key is optional, and absence is not an error
//!
//! CORE works with no key at a low rate limit; a **free** key raises it.
//! So the key is read from `DOIGET_CORE_API_KEY` when the user set one and
//! simply omitted otherwise — an absent key degrades to the key-less rate
//! limit rather than failing, which is what #417 requires. doiget bundles
//! no key, and none is needed to build.
//!
//! The key is sent as a bearer header through
//! [`HttpClient::fetch_bytes_with_headers`](crate::http::HttpClient::fetch_bytes_with_headers),
//! whose contract is that header values reach the wire only and are never
//! logged.
//!
//! ## A bad key must not look like a missing paper
//!
//! Two failures that are easy to conflate:
//!
//! - CORE holds nothing for this DOI — `FetchError::NotFound`
//! - CORE rejected our credentials — `FetchError::Http`, carrying the
//!   401/403
//!
//! They are different error *types*, not two spellings of one hint, so a
//! caller cannot accidentally read a misconfigured key as an absent
//! record. When a key *was* sent and the answer is 401/403, the source
//! additionally logs a warning naming the env var — that is the one case
//! where the user has something to fix.
//!
//! ## Metadata-only contract
//!
//! Per `docs/SOURCES.md` §4 this source never returns PDF bytes. CORE
//! exposes `downloadUrl` on its works, but those point at repository
//! hosts, reached through the `oa-publisher` key rather than this one.
//!
//! API: public REST v3.
//! Terms: <https://core.ac.uk/terms>

use async_trait::async_trait;
use url::Url;

use crate::provenance::{Capability, LogEvent, LogResult, RowInput};
use crate::source::{FetchContext, FetchError, FetchResult, Source};
use crate::{CapabilityProfile, Ref};

/// Production CORE API base.
const DEFAULT_BASE: &str = "https://api.core.ac.uk";

/// Env var holding the user's own free CORE key. Never bundled.
const API_KEY_ENV: &str = "DOIGET_CORE_API_KEY";

/// CORE [`Source`] impl — DOI to aggregated OA work record.
#[derive(Clone, Debug)]
pub struct CoreSource {
    /// API base URL. Production pins `https://api.core.ac.uk`;
    /// [`with_base`](Self::with_base) lets wiremock substitute an
    /// `http://127.0.0.1:N` origin.
    base: Url,
}

impl CoreSource {
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

    /// Build `/v3/search/works?q=doi:"<doi>"&limit=1`.
    ///
    /// The DOI is quoted so the query engine treats it as a phrase rather
    /// than tokenising on `.` and `/`, and the whole value is
    /// percent-encoded by `query_pairs_mut`, so no query syntax can be
    /// injected from the suffix.
    fn request_url(&self, doi: &crate::Doi) -> Result<Url, FetchError> {
        let mut url = self
            .base
            .join("/v3/search/works")
            .map_err(|e| FetchError::SourceSchema {
                hint: format!("core URL construction failed: {e}"),
            })?;
        url.query_pairs_mut()
            .append_pair("q", &format!("doi:\"{}\"", doi.as_str()))
            .append_pair("limit", "1");
        Ok(url)
    }
}

impl Default for CoreSource {
    fn default() -> Self {
        Self::new()
    }
}

/// Read the optional user-supplied CORE key.
///
/// Returns `None` for unset **and** for whitespace-only, so an
/// accidentally blank `DOIGET_CORE_API_KEY=` behaves as "no key" instead
/// of sending an empty bearer token that CORE would answer with a 401 —
/// which would then look like a *bad* key rather than no key.
fn api_key_from_env() -> Option<String> {
    std::env::var(API_KEY_ENV)
        .ok()
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
}

#[async_trait]
impl Source for CoreSource {
    fn name(&self) -> &str {
        "core"
    }

    fn can_serve(&self, profile: &CapabilityProfile, ref_: &Ref) -> bool {
        profile.metadata.core && matches!(ref_, Ref::Doi(_))
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
                    source_key: "core".into(),
                });
            }
        };

        if !profile.metadata.core {
            return Err(FetchError::NotEligible {
                source_key: "core".into(),
            });
        }

        let _permit = ctx.rate_limiter.acquire(self.name()).await;

        let url = self.request_url(doi)?;
        let key = api_key_from_env();
        let bearer = key.as_ref().map(|k| format!("Bearer {k}"));
        let headers: Vec<(&str, &str)> = match bearer.as_deref() {
            Some(v) => vec![("authorization", v)],
            None => Vec::new(),
        };

        let (body, final_url) = match ctx
            .http
            .fetch_bytes_with_headers(self.name(), url, &headers)
            .await
        {
            Ok(ok) => ok,
            Err(e) => {
                // A rejected key is the one failure here the user can act
                // on, so say so — but only when a key was actually sent,
                // since a key-less 401 means something else entirely.
                if key.is_some() && matches!(status_of(&e), Some(401 | 403)) {
                    tracing::warn!(
                        env = API_KEY_ENV,
                        "core rejected the configured API key; it is wrong or \
                         revoked (unset the variable to fall back to the \
                         key-less rate limit)"
                    );
                }
                return Err(e.into());
            }
        };

        let envelope: serde_json::Value =
            serde_json::from_slice(&body).map_err(|e| FetchError::SourceSchema {
                hint: format!("core returned non-JSON: {e}"),
            })?;

        let results = envelope
            .get("results")
            .and_then(|r| r.as_array())
            .ok_or_else(|| FetchError::SourceSchema {
                hint: format!(
                    "core response missing `results` array (got: {})",
                    truncate_for_hint(&body)
                ),
            })?;
        // Deliberately NotFound, not SourceSchema: "CORE holds nothing for
        // this DOI" is a clean miss, and must stay type-distinct from the
        // credential failure handled above.
        let work = results.first().ok_or_else(|| FetchError::NotFound {
            hint: format!("core has no work for {}", doi.as_str()),
        })?;

        let license = license_of(work);
        let canonical = ref_.promote(self.name(), None).digest_hex();
        ctx.log.append(RowInput {
            event: LogEvent::Fetch,
            result: LogResult::Ok,
            capability: Capability::Metadata,
            ref_: Some(doi.as_str()),
            source: Some(self.name()),
            error_code: None,
            size_bytes: Some(body.len() as u64),
            license: Some(license.as_str()),
            store_path: None,
            canonical_digest: Some(&canonical),
        })?;

        Ok(FetchResult {
            source: self.name().to_string(),
            license,
            pdf_bytes: None,
            final_url: Some(final_url),
            metadata_json: Some(work.clone()),
        })
    }
}

/// HTTP status carried by an [`crate::http::HttpError`], when it has one.
fn status_of(e: &crate::http::HttpError) -> Option<u16> {
    match e {
        crate::http::HttpError::HttpStatus { status, .. } => Some(*status),
        _ => None,
    }
}

/// Licence from the CORE work, else `"unknown"`.
///
/// Free text from the aggregated repository rather than an SPDX id, so it
/// is passed through verbatim — normalising would mean guessing, and a
/// wrong licence is worse than an absent one.
fn license_of(work: &serde_json::Value) -> String {
    work.get("license")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("unknown")
        .to_string()
}

/// The OA download location CORE reports for a work, if any (#445).
///
/// CORE's `downloadUrl` points at a repository host reached through the
/// `oa-publisher` key, not this one — see the metadata-only contract in the
/// module docs. This surfaces it so the OA chain can try it; the fetch is
/// still performed by the `oa-publisher` leg, with its allowlist and its
/// ADR-0023 denial context.
#[must_use]
pub fn open_access_pdf_url(record: &serde_json::Value) -> Option<&str> {
    record
        .get("downloadUrl")
        .and_then(serde_json::Value::as_str)
        .filter(|u| !u.is_empty())
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
    use wiremock::matchers::{header_exists, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::http::HttpClient;
    use crate::provenance::ProvenanceLog;
    use crate::rate_limiter::RateLimiter;
    use crate::{CapabilityProfile, Doi, MetadataAccess, RateLimits, Ref};

    const SAMPLE_HIT: &str = r#"{
        "totalHits": 1,
        "results": [
            {
                "id": 12345,
                "title": "An Aggregated Open Access Work",
                "doi": "10.1234/example",
                "license": "https://creativecommons.org/licenses/by/4.0/",
                "downloadUrl": "https://core.ac.uk/download/12345.pdf"
            }
        ]
    }"#;

    const SAMPLE_EMPTY: &str = r#"{"totalHits": 0, "results": []}"#;

    /// RAII env guard — these tests mutate `DOIGET_CORE_API_KEY`, which is
    /// process-global, so they are serialised.
    struct KeyGuard(Option<String>);
    impl KeyGuard {
        fn set(v: Option<&str>) -> Self {
            let prev = std::env::var(API_KEY_ENV).ok();
            match v {
                Some(k) => std::env::set_var(API_KEY_ENV, k),
                None => std::env::remove_var(API_KEY_ENV),
            }
            Self(prev)
        }
    }
    impl Drop for KeyGuard {
        fn drop(&mut self) {
            match &self.0 {
                Some(v) => std::env::set_var(API_KEY_ENV, v),
                None => std::env::remove_var(API_KEY_ENV),
            }
        }
    }

    fn build_test_context(wiremock_host: &str) -> (TempDir, FetchContext) {
        let td = TempDir::new().expect("tempdir");
        let log_dir =
            Utf8PathBuf::try_from(td.path().to_path_buf()).expect("temp dir path must be UTF-8");
        let http = Arc::new(HttpClient::new_for_tests_allow_http("core", wiremock_host));
        let session_id = "01J0000000000000000000TEST".to_string();
        let log = Arc::new(
            ProvenanceLog::open(log_dir.join("test.jsonl"), session_id.clone())
                .expect("provenance log opens"),
        );
        let ctx = FetchContext {
            http,
            rate_limiter: Arc::new(RateLimiter::new(RateLimits::HARD_CODED)),
            log,
            session_id,
            cache_root: None,
        };
        (td, ctx)
    }

    fn profile(core: bool) -> CapabilityProfile {
        let mut p = CapabilityProfile::from_env().expect("clean env never errors");
        p.metadata = MetadataAccess {
            openalex: false,
            semantic_scholar: false,
            doaj: false,
            datacite: false,
            hal: false,
            openaire: false,
            core,
            europe_pmc: false,
        };
        p
    }

    #[test]
    fn request_url_quotes_the_doi_as_a_phrase() {
        let src = CoreSource::new();
        let doi = Doi::parse("10.1234/example").expect("valid doi");
        let url = src.request_url(&doi).expect("url builds");
        assert_eq!(url.path(), "/v3/search/works");
        let q = url
            .query_pairs()
            .find(|(k, _)| k == "q")
            .expect("q param")
            .1
            .into_owned();
        assert_eq!(q, "doi:\"10.1234/example\"");
    }

    /// #417: an absent key must degrade to the key-less rate limit, NOT
    /// fail, and must not send an empty `authorization` header.
    #[tokio::test]
    #[serial_test::serial]
    async fn works_without_a_key_and_sends_no_auth_header() {
        let _g = KeyGuard::set(None);
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v3/search/works"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_HIT))
            .mount(&server)
            .await;

        let (_td, ctx) = build_test_context(&server.address().to_string());
        let src = CoreSource::with_base(Url::parse(&server.uri()).expect("base"));
        let ref_ = Ref::Doi(Doi::parse("10.1234/example").expect("doi"));

        let got = src
            .fetch(&ref_, &profile(true), &ctx)
            .await
            .expect("key-less fetch must succeed");
        assert_eq!(got.source, "core");
        assert_eq!(got.license, "https://creativecommons.org/licenses/by/4.0/");
        assert!(got.pdf_bytes.is_none(), "metadata-only contract");

        let reqs = server.received_requests().await.expect("recorded");
        assert!(
            reqs[0].headers.get("authorization").is_none(),
            "no key configured means no authorization header"
        );
    }

    /// A configured key must actually reach the wire as a bearer header.
    #[tokio::test]
    #[serial_test::serial]
    async fn sends_the_configured_key_as_a_bearer_header() {
        let _g = KeyGuard::set(Some("secret-key-value"));
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v3/search/works"))
            .and(header_exists("authorization"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_HIT))
            .mount(&server)
            .await;

        let (_td, ctx) = build_test_context(&server.address().to_string());
        let src = CoreSource::with_base(Url::parse(&server.uri()).expect("base"));
        let ref_ = Ref::Doi(Doi::parse("10.1234/example").expect("doi"));
        src.fetch(&ref_, &profile(true), &ctx)
            .await
            .expect("keyed fetch must succeed");

        let reqs = server.received_requests().await.expect("recorded");
        let auth = reqs[0]
            .headers
            .get("authorization")
            .expect("authorization header")
            .to_str()
            .expect("ascii");
        assert_eq!(auth, "Bearer secret-key-value");
    }

    /// A blank `DOIGET_CORE_API_KEY=` is "no key", not an empty key.
    /// Sending an empty bearer would earn a 401 that looks like a *bad* key.
    #[test]
    #[serial_test::serial]
    fn blank_key_is_treated_as_absent() {
        {
            let _g = KeyGuard::set(Some("   "));
            assert_eq!(api_key_from_env(), None);
        }
        {
            let _g = KeyGuard::set(Some("k"));
            assert_eq!(api_key_from_env(), Some("k".to_string()));
        }
    }

    /// #417: a rejected key must be reported distinctly from "not found".
    /// These are different error TYPES, so a caller cannot conflate them.
    #[tokio::test]
    #[serial_test::serial]
    async fn rejected_key_and_missing_work_are_different_error_types() {
        let ref_ = Ref::Doi(Doi::parse("10.1234/example").expect("doi"));

        // 401 with a key configured -> Http, carrying the status.
        let _g = KeyGuard::set(Some("bad-key"));
        let unauthorized = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v3/search/works"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&unauthorized)
            .await;
        let (_td1, ctx1) = build_test_context(&unauthorized.address().to_string());
        let src1 = CoreSource::with_base(Url::parse(&unauthorized.uri()).expect("base"));
        let auth_err = src1
            .fetch(&ref_, &profile(true), &ctx1)
            .await
            .expect_err("401 must be an error");
        assert!(
            matches!(auth_err, FetchError::Http(_)),
            "a rejected key must surface as Http, got {auth_err:?}"
        );

        // Empty results -> NotFound.
        let empty = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v3/search/works"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_EMPTY))
            .mount(&empty)
            .await;
        let (_td2, ctx2) = build_test_context(&empty.address().to_string());
        let src2 = CoreSource::with_base(Url::parse(&empty.uri()).expect("base"));
        let miss = src2
            .fetch(&ref_, &profile(true), &ctx2)
            .await
            .expect_err("no results must be an error");
        assert!(
            matches!(miss, FetchError::NotFound { .. }),
            "a clean miss must surface as NotFound, got {miss:?}"
        );
    }

    /// The regression #413 requires of every new source.
    #[tokio::test]
    #[serial_test::serial]
    async fn is_inert_when_the_runtime_flag_is_unset() {
        let _g = KeyGuard::set(None);
        let server = MockServer::start().await;
        let (_td, ctx) = build_test_context(&server.address().to_string());
        let src = CoreSource::with_base(Url::parse(&server.uri()).expect("base"));
        let ref_ = Ref::Doi(Doi::parse("10.1234/example").expect("doi"));

        assert!(!src.can_serve(&profile(false), &ref_));
        let err = src
            .fetch(&ref_, &profile(false), &ctx)
            .await
            .expect_err("must refuse");
        assert!(matches!(err, FetchError::NotEligible { .. }), "got {err:?}");
        assert!(
            server
                .received_requests()
                .await
                .expect("recorded")
                .is_empty(),
            "an inert source must make NO request"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn arxiv_refs_are_not_eligible() {
        let server = MockServer::start().await;
        let (_td, ctx) = build_test_context(&server.address().to_string());
        let src = CoreSource::with_base(Url::parse(&server.uri()).expect("base"));
        let ref_ = Ref::Arxiv(crate::ArxivId::parse("2401.12345").expect("arxiv id"));
        assert!(!src.can_serve(&profile(true), &ref_));
        assert!(matches!(
            src.fetch(&ref_, &profile(true), &ctx).await,
            Err(FetchError::NotEligible { .. })
        ));
    }

    #[test]
    fn license_falls_back_to_unknown() {
        assert_eq!(license_of(&serde_json::json!({})), "unknown");
        assert_eq!(license_of(&serde_json::json!({"license": "  "})), "unknown");
        assert_eq!(
            license_of(&serde_json::json!({"license": "cc-by"})),
            "cc-by"
        );
    }
}
