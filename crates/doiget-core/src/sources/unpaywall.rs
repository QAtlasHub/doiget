//! Unpaywall source — OA URL discovery + license metadata for a given DOI.
//!
//! Spec: docs/SOURCES.md §4 Unpaywall. Free public API; the polite pool
//! requires `email=<contact>` in the URL query. The `email` is set per
//! `[network] unpaywall_email` in `config.toml` (Phase 1: caller-injected
//! via `UnpaywallSource::new`).

use async_trait::async_trait;
use serde::Deserialize;
use url::Url;

use crate::provenance::{Capability, LogEvent, LogResult, RowInput};
use crate::source::{FetchContext, FetchError, FetchResult, Source};
use crate::{CapabilityProfile, Ref};

const DEFAULT_BASE: &str = "https://api.unpaywall.org/v2";

/// Unpaywall Source impl.
#[derive(Clone, Debug)]
pub struct UnpaywallSource {
    base: Url,
    contact_email: String,
}

impl UnpaywallSource {
    /// Production constructor. The `contact_email` is REQUIRED for the polite
    /// pool — Unpaywall returns 403 without it.
    pub fn new(contact_email: String) -> Self {
        // `DEFAULT_BASE` is a compile-time const string with a valid HTTPS
        // URL syntax; `Url::parse` on it cannot fail at runtime. The local
        // `allow` is the documented exception to the workspace `expect_used`
        // lint (see `rate_limiter.rs::acquire`).
        #[allow(clippy::expect_used)]
        let base = Url::parse(DEFAULT_BASE).expect("hard-coded base URL is valid");
        Self {
            base,
            contact_email,
        }
    }

    /// Construct with an arbitrary base URL.
    ///
    /// The orchestrator (`doiget-cli::commands::fetch`) uses this to honor
    /// the `DOIGET_UNPAYWALL_BASE` env var, which lets integration tests
    /// point the source at a wiremock origin without compile-time gates.
    /// Production callers use [`UnpaywallSource::new`].
    pub fn with_base(base: Url, contact_email: String) -> Self {
        Self {
            base,
            contact_email,
        }
    }

    fn request_url(&self, doi: &crate::Doi) -> Result<Url, FetchError> {
        // The path layout is `/v2/<DOI>`. Unpaywall accepts the bare DOI
        // (no `doi:` scheme); `Doi::as_str()` already strips it.
        let mut url = self.base.clone();
        // `path_segments_mut` properly URL-encodes each segment, including the
        // forward slash inside the DOI suffix.
        url.path_segments_mut()
            .map_err(|()| FetchError::SourceSchema {
                hint: "unpaywall base URL is cannot-be-a-base".into(),
            })?
            .push(doi.as_str()); // single-push so the `/` in the DOI is encoded
        url.query_pairs_mut()
            .append_pair("email", &self.contact_email);
        Ok(url)
    }
}

#[async_trait]
impl Source for UnpaywallSource {
    fn name(&self) -> &str {
        "unpaywall"
    }

    fn can_serve(&self, _profile: &CapabilityProfile, ref_: &Ref) -> bool {
        matches!(ref_, Ref::Doi(_))
    }

    async fn fetch(
        &self,
        ref_: &Ref,
        _profile: &CapabilityProfile,
        ctx: &FetchContext,
    ) -> Result<FetchResult, FetchError> {
        let doi = match ref_ {
            Ref::Doi(d) => d,
            Ref::Arxiv(_) => {
                return Err(FetchError::NotEligible {
                    source_key: "unpaywall".into(),
                });
            }
        };

        let _permit = ctx.rate_limiter.acquire(self.name()).await;

        let url = self.request_url(doi)?;
        let (body, final_url) = ctx.http.fetch_bytes(self.name(), url).await?;

        let work: UnpaywallWork =
            serde_json::from_slice(&body).map_err(|e| FetchError::SourceSchema {
                hint: format!("unpaywall returned non-JSON: {e}"),
            })?;

        // Resolve a license string from `best_oa_location.license`, falling back
        // to "unknown" if absent. Spec: docs/STORE.md §2 — `license` is always
        // a string (use "unknown" when not provided).
        let license = work
            .best_oa_location
            .as_ref()
            .and_then(|loc| loc.license.clone())
            .unwrap_or_else(|| "unknown".to_string());

        // ADR-0021 §1 canonical-digest under the "unpaywall" resolver
        // profile. Distinct from a Crossref attempt for the same DOI.
        let canonical = ref_.promote(self.name(), None).digest_hex();
        ctx.log.append(RowInput {
            event: LogEvent::Fetch,
            result: LogResult::Ok,
            capability: Capability::Oa,
            ref_: Some(doi.as_str()),
            source: Some(self.name()),
            error_code: None,
            size_bytes: Some(body.len() as u64),
            license: Some(&license),
            store_path: None,
            canonical_digest: Some(&canonical),
        })?;

        // Note: this source returns metadata only; the actual PDF fetch
        // from the discovered OA URL is the orchestrator's job
        // (`crate::orchestrator::try_fetch_oa_pdf`, called from
        // `fetch_paper_doi`). That leg runs the OA URL through the
        // `oa-publisher` per-publisher allowlist BOTH as a pre-fetch host
        // check (issue #145; `docs/REDIRECT_ALLOWLIST.md` §1 — applied to
        // the metadata-discovered URL before the fetch is issued) and on
        // every redirect hop via the per-source redirect closure in
        // `crate::http`. See ARCHITECTURE.md §6.
        Ok(FetchResult {
            source: self.name().to_string(),
            license,
            pdf_bytes: None,
            final_url: Some(final_url),
            metadata_json: Some(serde_json::to_value(&work).unwrap_or(serde_json::Value::Null)),
        })
    }
}

/// Subset of the Unpaywall work record. We deserialize loosely — extra fields
/// are ignored (no `deny_unknown_fields`) so future API additions don't break.
#[derive(Debug, Deserialize, serde::Serialize)]
struct UnpaywallWork {
    doi: String,
    is_oa: bool,
    /// Unpaywall's OA classification: `gold` / `green` / `hybrid` /
    /// `bronze` / `closed`. Surfaced to the caller as `oa_status` for OA
    /// transparency (#281 item 4) so an agent can tell a paywalled
    /// (`closed`) work from an openly-available one. Captured into
    /// `metadata_json` (the orchestrator reads it from there).
    #[serde(default)]
    oa_status: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    best_oa_location: Option<UnpaywallOaLocation>,
    #[serde(default)]
    oa_locations: Vec<UnpaywallOaLocation>,
}

#[derive(Debug, Deserialize, serde::Serialize, Clone)]
struct UnpaywallOaLocation {
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    url_for_pdf: Option<String>,
    #[serde(default)]
    license: Option<String>,
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
    use crate::provenance::{LogRow, ProvenanceLog};
    use crate::rate_limiter::RateLimiter;
    use crate::source::FetchContext;
    use crate::{ArxivId, CapabilityProfile, Doi, RateLimits};

    const TEST_EMAIL: &str = "alice@example.org";
    const TEST_DOI: &str = "10.1234/example";
    /// Percent-encoded form of `TEST_DOI` as it appears on the wire after
    /// `path_segments_mut().push(...)`. Wiremock's `path` matcher operates on
    /// the request's encoded path portion, so we match against this form.
    const TEST_DOI_ENCODED: &str = "10.1234%2Fexample";

    /// Build a `FetchContext` whose `HttpClient` allows plain-HTTP initial
    /// legs against the wiremock origin. The redirect closure is unchanged
    /// (HTTPS-only + allowlist) — only the *initial* connection is relaxed.
    fn build_test_context(host: &str) -> (TempDir, FetchContext) {
        let td = TempDir::new().expect("tempdir");
        let log_dir =
            Utf8PathBuf::try_from(td.path().to_path_buf()).expect("temp dir path must be UTF-8");
        let log_path = log_dir.join("test.jsonl");

        let http = Arc::new(HttpClient::new_for_tests_allow_http("unpaywall", host));
        let rate_limiter = Arc::new(RateLimiter::new(RateLimits::HARD_CODED));
        let session_id = "01J0000000000000000000TEST".to_string();
        let log = Arc::new(
            ProvenanceLog::open(log_path, session_id.clone()).expect("provenance log opens"),
        );

        (
            td,
            FetchContext {
                http,
                rate_limiter,
                log,
                session_id,
                cache_root: None,
            },
        )
    }

    fn host_of(uri: &str) -> String {
        uri.parse::<Url>()
            .expect("valid uri")
            .host_str()
            .expect("has host")
            .to_string()
    }

    fn base_of(server_uri: &str) -> Url {
        // The wiremock server roots at `/`; Unpaywall lives at `/v2/<DOI>`.
        // Including the `/v2` segment in the base lets `request_url`'s
        // single-push DOI segment land at the correct path.
        format!("{}/v2", server_uri).parse().expect("valid base")
    }

    fn ok_response_body() -> serde_json::Value {
        serde_json::json!({
            "doi": TEST_DOI,
            "is_oa": true,
            "title": "Example",
            "best_oa_location": {
                "url": "https://example.org/free.pdf",
                "license": "cc-by"
            }
        })
    }

    #[test]
    fn unpaywall_can_serve_returns_true_for_doi() {
        let s = UnpaywallSource::new(TEST_EMAIL.to_string());
        let profile = CapabilityProfile::for_tests();
        let r = Ref::Doi(Doi(TEST_DOI.to_string()));
        assert!(s.can_serve(&profile, &r));
    }

    #[test]
    fn unpaywall_can_serve_returns_false_for_arxiv() {
        let s = UnpaywallSource::new(TEST_EMAIL.to_string());
        let profile = CapabilityProfile::for_tests();
        let r = Ref::Arxiv(ArxivId("2401.12345".to_string()));
        assert!(!s.can_serve(&profile, &r));
    }

    #[tokio::test]
    async fn unpaywall_fetch_returns_oa_metadata() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/v2/{}", TEST_DOI_ENCODED)))
            .and(query_param("email", TEST_EMAIL))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_response_body()))
            .mount(&server)
            .await;

        let host = host_of(&server.uri());
        let (_td, ctx) = build_test_context(&host);
        let s = UnpaywallSource::with_base(base_of(&server.uri()), TEST_EMAIL.to_string());
        let profile = CapabilityProfile::for_tests();
        let r = Ref::Doi(Doi(TEST_DOI.to_string()));

        let res = s.fetch(&r, &profile, &ctx).await.expect("fetch ok");
        assert_eq!(res.source, "unpaywall");
        assert!(res.final_url.is_some());
        let meta = res.metadata_json.expect("metadata present");
        let parsed: UnpaywallWork = serde_json::from_value(meta).expect("metadata round-trips");
        assert!(parsed.is_oa);
        assert_eq!(parsed.doi, TEST_DOI);
    }

    #[tokio::test]
    async fn unpaywall_extracts_license_from_best_oa_location() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/v2/{}", TEST_DOI_ENCODED)))
            .and(query_param("email", TEST_EMAIL))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_response_body()))
            .mount(&server)
            .await;

        let host = host_of(&server.uri());
        let (_td, ctx) = build_test_context(&host);
        let s = UnpaywallSource::with_base(base_of(&server.uri()), TEST_EMAIL.to_string());
        let profile = CapabilityProfile::for_tests();
        let r = Ref::Doi(Doi(TEST_DOI.to_string()));

        let res = s.fetch(&r, &profile, &ctx).await.expect("fetch ok");
        assert_eq!(res.license, "cc-by");
    }

    #[tokio::test]
    async fn unpaywall_surfaces_oa_status_in_metadata() {
        // OA transparency (#281 item 4): the work's `oa_status` must round
        // -trip into `metadata_json` so the orchestrator can surface it.
        let body = serde_json::json!({
            "doi": TEST_DOI,
            "is_oa": true,
            "oa_status": "gold",
            "best_oa_location": { "url": "https://example.org/free.pdf", "license": "cc-by" }
        });
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/v2/{}", TEST_DOI_ENCODED)))
            .and(query_param("email", TEST_EMAIL))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let host = host_of(&server.uri());
        let (_td, ctx) = build_test_context(&host);
        let s = UnpaywallSource::with_base(base_of(&server.uri()), TEST_EMAIL.to_string());
        let profile = CapabilityProfile::for_tests();
        let r = Ref::Doi(Doi(TEST_DOI.to_string()));

        let res = s.fetch(&r, &profile, &ctx).await.expect("fetch ok");
        let meta = res.metadata_json.expect("metadata present");
        assert_eq!(meta.get("oa_status").and_then(|v| v.as_str()), Some("gold"));
    }

    #[tokio::test]
    async fn unpaywall_falls_back_to_unknown_license() {
        let body = serde_json::json!({
            "doi": TEST_DOI,
            "is_oa": true,
            "best_oa_location": {
                "url": "https://example.org/free.pdf",
                "license": serde_json::Value::Null
            }
        });
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/v2/{}", TEST_DOI_ENCODED)))
            .and(query_param("email", TEST_EMAIL))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let host = host_of(&server.uri());
        let (_td, ctx) = build_test_context(&host);
        let s = UnpaywallSource::with_base(base_of(&server.uri()), TEST_EMAIL.to_string());
        let profile = CapabilityProfile::for_tests();
        let r = Ref::Doi(Doi(TEST_DOI.to_string()));

        let res = s.fetch(&r, &profile, &ctx).await.expect("fetch ok");
        assert_eq!(res.license, "unknown");
    }

    #[tokio::test]
    async fn unpaywall_with_arxiv_ref_errors_not_eligible() {
        // No mock: should never reach the network because the ref-kind
        // gate fires first.
        let (_td, ctx) = build_test_context("127.0.0.1");
        let s = UnpaywallSource::new(TEST_EMAIL.to_string());
        let profile = CapabilityProfile::for_tests();
        let r = Ref::Arxiv(ArxivId("2401.12345".to_string()));

        let err = s
            .fetch(&r, &profile, &ctx)
            .await
            .expect_err("arxiv must be ineligible");
        match err {
            FetchError::NotEligible { source_key } => {
                assert_eq!(source_key, "unpaywall");
            }
            other => panic!("expected NotEligible, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn unpaywall_writes_log_row_with_license() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/v2/{}", TEST_DOI_ENCODED)))
            .and(query_param("email", TEST_EMAIL))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_response_body()))
            .mount(&server)
            .await;

        let host = host_of(&server.uri());
        let (td, ctx) = build_test_context(&host);
        let s = UnpaywallSource::with_base(base_of(&server.uri()), TEST_EMAIL.to_string());
        let profile = CapabilityProfile::for_tests();
        let r = Ref::Doi(Doi(TEST_DOI.to_string()));

        let _res = s.fetch(&r, &profile, &ctx).await.expect("fetch ok");

        // Read every JSON-Lines row from the log file and assert the Fetch
        // row is present with license="cc-by".
        let log_path = Utf8PathBuf::try_from(td.path().to_path_buf())
            .expect("temp path utf-8")
            .join("test.jsonl");
        let raw = std::fs::read_to_string(&log_path).expect("read log");
        let rows: Vec<LogRow> = raw
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str::<LogRow>(l).expect("valid LogRow"))
            .collect();

        let fetch_rows: Vec<&LogRow> = rows.iter().filter(|r| r.event == LogEvent::Fetch).collect();
        assert_eq!(
            fetch_rows.len(),
            1,
            "expected one Fetch row, got {:?}",
            rows
        );
        let row = fetch_rows[0];
        assert_eq!(row.result, LogResult::Ok);
        assert_eq!(row.license.as_deref(), Some("cc-by"));
        assert_eq!(row.source.as_deref(), Some("unpaywall"));
        assert_eq!(row.ref_.as_deref(), Some(TEST_DOI));
    }

    #[test]
    fn unpaywall_email_is_in_query_string() {
        // White-box: invoke `request_url` directly to assert the email is
        // serialized into the query. The wiremock-driven tests above assert
        // the same property via the `query_param("email", ...)` matcher,
        // but this test pins the contract without booting an HTTP server.
        // `query_pairs_mut().append_pair` percent-encodes the `@`, so we
        // verify against the decoded form via `query_pairs()`.
        let s = UnpaywallSource::new(TEST_EMAIL.to_string());
        let doi = Doi(TEST_DOI.to_string());
        let url = s.request_url(&doi).expect("url builds");
        let pair = url
            .query_pairs()
            .find(|(k, _)| k == "email")
            .expect("email pair present");
        assert_eq!(pair.1, TEST_EMAIL, "decoded email must match: {:?}", pair);
    }

    #[tokio::test]
    async fn unpaywall_404_maps_to_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/v2/{}", TEST_DOI_ENCODED)))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let host = host_of(&server.uri());
        let (_td, ctx) = build_test_context(&host);
        let s = UnpaywallSource::with_base(base_of(&server.uri()), TEST_EMAIL.to_string());
        let profile = CapabilityProfile::for_tests();
        let r = Ref::Doi(Doi(TEST_DOI.to_string()));

        let err = s
            .fetch(&r, &profile, &ctx)
            .await
            .expect_err("404 must error");
        match err {
            FetchError::Http(_) => {}
            other => panic!("expected FetchError::Http, got {:?}", other),
        }
    }
}
