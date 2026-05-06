//! arXiv source — arXiv id → PDF (and minimal metadata fields).
//!
//! Spec: `docs/SOURCES.md` §4 arXiv. No auth; the API has a 3-second-per-request
//! rate guideline that doiget's 5/sec global + 200ms per-source backoff
//! comfortably respects (no extra source-specific tuning needed).
//!
//! # Fetch flow
//!
//! 1. `can_serve` returns `true` only for `Ref::Arxiv(_)`; `Ref::Doi(_)` is
//!    rejected up front.
//! 2. `fetch` acquires a permit from the shared `RateLimiter`, builds the
//!    PDF URL `https://arxiv.org/pdf/<id>.pdf`, and dispatches via
//!    [`crate::http::HttpClient::fetch_pdf`] which enforces the magic-byte
//!    (`%PDF-`) check per `docs/SECURITY.md` §1.2 — non-PDF response is a
//!    hard error.
//! 3. One `LogEvent::Fetch` row is appended via `ctx.log.append`; per
//!    `docs/PROVENANCE_LOG.md` §3 a write failure is fail-closed and aborts
//!    the surrounding fetch (the `?` on `append` propagates as
//!    `FetchError::Log`).
//!
//! # Metadata (deferred)
//!
//! Phase 1 returns the PDF bytes only. The export.arxiv.org Atom feed
//! (`https://export.arxiv.org/api/query?id_list=<id>`) is documented in the
//! arXiv API guide but XML parsing is deferred to a follow-up PR — see the
//! `metadata_json` field of [`FetchResult`] which is set to `None` here
//! (TODO Phase 1+).

use async_trait::async_trait;
use bytes::Bytes;
use url::Url;

use crate::provenance::{Capability, LogEvent, LogResult, RowInput};
use crate::source::{FetchContext, FetchError, FetchResult, Source};
use crate::{ArxivId, CapabilityProfile, Ref};

/// Default base for the PDF endpoint. arXiv serves PDFs at
/// `https://arxiv.org/pdf/<id>` (the trailing `.pdf` is optional but
/// most reliable to include). PDFs may redirect to `cdn.arxiv.org` —
/// the per-source allowlist in `crate::http::tier_1_allowlist()` covers
/// this via the `*.arxiv.org` glob.
const PDF_BASE: &str = "https://arxiv.org";

/// arXiv [`Source`] impl. Phase 1 returns the PDF bytes and skips metadata
/// (the export.arxiv.org Atom feed is documented but XML parsing is
/// deferred to a follow-up PR — TODO Phase 1+).
#[derive(Clone, Debug)]
pub struct ArxivSource {
    base: Url,
}

impl ArxivSource {
    /// Production constructor. Uses the public arxiv.org PDF endpoint.
    pub fn new() -> Self {
        // The hard-coded `PDF_BASE` is a `'static` string literal known
        // at compile time to be a valid absolute URL. The `expect` here
        // can only fire if the constant itself regresses, which is
        // exercised at every test run via `ArxivSource::new()`.
        #[allow(clippy::expect_used)]
        let base = Url::parse(PDF_BASE).expect("hard-coded base URL is valid");
        Self { base }
    }

    /// Construct with an arbitrary base URL.
    ///
    /// The orchestrator (`doiget-cli::commands::fetch`) uses this to honor
    /// the `DOIGET_ARXIV_BASE` env var, which lets integration tests point
    /// the source at a wiremock origin without resorting to compile-time
    /// gates. Production callers use [`ArxivSource::new`].
    pub fn with_base(base: Url) -> Self {
        Self { base }
    }

    /// Build the PDF URL for a given arXiv id. arXiv accepts both
    /// `/pdf/<id>` and `/pdf/<id>.pdf`; we use the trailing-`.pdf` form to
    /// make the URL self-describing.
    ///
    /// Old-style ids (`cond-mat/9501001`) contain a `/` in the id itself;
    /// the resulting path `/pdf/cond-mat/9501001.pdf` is the form arXiv
    /// expects. Because the base URL has no path beyond `/`, `Url::join`
    /// resolves the absolute reference `/pdf/<id>.pdf` to exactly that
    /// path for both new-style (`2401.12345`) and old-style
    /// (`cond-mat/9501001`) ids. The `arxiv_fetch_old_style_id_*` test
    /// pins this behavior.
    fn pdf_url(&self, id: &ArxivId) -> Result<Url, FetchError> {
        let path = format!("/pdf/{}.pdf", id.as_str());
        self.base.join(&path).map_err(|e| FetchError::SourceSchema {
            hint: format!("arxiv URL construction failed: {e}"),
        })
    }
}

impl Default for ArxivSource {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Source for ArxivSource {
    fn name(&self) -> &str {
        "arxiv"
    }

    fn can_serve(&self, _profile: &CapabilityProfile, ref_: &Ref) -> bool {
        matches!(ref_, Ref::Arxiv(_))
    }

    async fn fetch(
        &self,
        ref_: &Ref,
        _profile: &CapabilityProfile,
        ctx: &FetchContext,
    ) -> Result<FetchResult, FetchError> {
        // Eligibility gate. The orchestrator is expected to call
        // `can_serve` first, but a runtime check here gives a clean error
        // path if it does not.
        let id = match ref_ {
            Ref::Arxiv(a) => a,
            Ref::Doi(_) => {
                return Err(FetchError::NotEligible {
                    source_key: "arxiv".into(),
                });
            }
        };

        // Hold the rate-limiter permit for the duration of the HTTP fetch.
        // Drop happens at end of scope after the log append below.
        let _permit = ctx.rate_limiter.acquire(self.name()).await;

        let url = self.pdf_url(id)?;

        // `fetch_pdf` enforces the magic-byte check (`%PDF-`) per
        // `docs/SECURITY.md` §1.2 — non-PDF response surfaces as
        // `HttpError::NotAPdf`, which `From` converts to `FetchError::Http`.
        let (body, final_url): (Bytes, Url) = ctx.http.fetch_pdf(self.name(), url).await?;

        // One `event=fetch` row per attempt, per `docs/ARCHITECTURE.md` §6
        // and `docs/PROVENANCE_LOG.md` §3. Per `docs/SECURITY.md` §1.8 a
        // log write failure is fail-closed — the `?` aborts the fetch.
        ctx.log.append(RowInput {
            event: LogEvent::Fetch,
            result: LogResult::Ok,
            capability: Capability::Oa,
            ref_: Some(id.as_str()),
            source: Some(self.name()),
            error_code: None,
            size_bytes: Some(body.len() as u64),
            // arXiv does not expose a per-item license string; the
            // platform-wide license declaration lives at
            // <https://info.arxiv.org/help/license/>. Phase 1 records
            // `"arxiv-default"` so the value is informative without
            // claiming a specific Creative Commons license.
            license: Some("arxiv-default"),
            store_path: None,
        })?;

        Ok(FetchResult {
            source: self.name().to_string(),
            license: "arxiv-default".into(),
            pdf_bytes: Some(body),
            final_url: Some(final_url),
            // TODO Phase 1+: parse export.arxiv.org Atom feed
            // (`https://export.arxiv.org/api/query?id_list=<id>`) and
            // populate this with title / authors / abstract / categories.
            metadata_json: None,
        })
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

    use crate::http::{HttpClient, HttpError};
    use crate::provenance::{LogRow, ProvenanceLog};
    use crate::rate_limiter::RateLimiter;
    use crate::source::FetchContext;
    use crate::{ArxivId, CapabilityProfile, Doi, RateLimits, Ref};

    const TEST_SESSION_ID: &str = "01J0000000000000000000TEST";

    /// Build a complete `FetchContext` against a wiremock host for use in
    /// the source-level tests below.
    fn build_test_context(wiremock_host: &str) -> (TempDir, FetchContext) {
        let td = TempDir::new().expect("tempdir");
        let log_dir =
            Utf8PathBuf::try_from(td.path().to_path_buf()).expect("temp dir path must be UTF-8");
        let log_path = log_dir.join("test.jsonl");

        let http = Arc::new(HttpClient::new_for_tests_allow_http("arxiv", wiremock_host));
        let rate_limiter = Arc::new(RateLimiter::new(RateLimits::HARD_CODED));
        let session_id = TEST_SESSION_ID.to_string();
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
            },
        )
    }

    fn read_rows(path: &camino::Utf8Path) -> Vec<LogRow> {
        let raw = std::fs::read_to_string(path).expect("read log");
        raw.lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str::<LogRow>(l).expect("valid LogRow"))
            .collect()
    }

    fn profile() -> CapabilityProfile {
        CapabilityProfile::from_env().expect("Phase 0 stub profile")
    }

    // -----------------------------------------------------------------
    // can_serve
    // -----------------------------------------------------------------

    #[test]
    fn arxiv_can_serve_returns_true_for_arxiv() {
        let s = ArxivSource::new();
        let id = ArxivId::parse("2401.12345").expect("valid id");
        let r = Ref::Arxiv(id);
        assert!(s.can_serve(&profile(), &r));
    }

    #[test]
    fn arxiv_can_serve_returns_false_for_doi() {
        let s = ArxivSource::new();
        let r = Ref::Doi(Doi("10.1234/example".to_string()));
        assert!(!s.can_serve(&profile(), &r));
    }

    // -----------------------------------------------------------------
    // fetch — happy paths
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn arxiv_fetch_new_style_id_returns_pdf_bytes() {
        let server = MockServer::start().await;
        let body = b"%PDF-1.7\n%fixture\n".to_vec();
        Mock::given(method("GET"))
            .and(path("/pdf/2401.12345.pdf"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
            .mount(&server)
            .await;

        let host = server
            .uri()
            .parse::<Url>()
            .unwrap()
            .host_str()
            .unwrap()
            .to_string();
        let (_td, ctx) = build_test_context(&host);
        let s = ArxivSource::with_base(server.uri().parse().unwrap());

        let id = ArxivId::parse("2401.12345").unwrap();
        let r = Ref::Arxiv(id);
        let res = s.fetch(&r, &profile(), &ctx).await.expect("fetch ok");

        assert_eq!(res.source, "arxiv");
        assert_eq!(res.license, "arxiv-default");
        let bytes = res.pdf_bytes.expect("pdf bytes set");
        assert!(
            bytes.starts_with(b"%PDF-"),
            "expected PDF magic prefix, got {:?}",
            &bytes[..bytes.len().min(8)]
        );
        assert_eq!(&bytes[..], &body[..]);
    }

    #[tokio::test]
    async fn arxiv_fetch_old_style_id_returns_pdf_bytes() {
        // Old-style id contains `/` (`cond-mat/9501001`); the URL must
        // become `/pdf/cond-mat/9501001.pdf`. This pins the URL-builder
        // behavior across both id shapes.
        let server = MockServer::start().await;
        let body = b"%PDF-1.4\n%old-style fixture\n".to_vec();
        Mock::given(method("GET"))
            .and(path("/pdf/cond-mat/9501001.pdf"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
            .mount(&server)
            .await;

        let host = server
            .uri()
            .parse::<Url>()
            .unwrap()
            .host_str()
            .unwrap()
            .to_string();
        let (_td, ctx) = build_test_context(&host);
        let s = ArxivSource::with_base(server.uri().parse().unwrap());

        let id = ArxivId::parse("cond-mat/9501001").expect("old-style id");
        let r = Ref::Arxiv(id);
        let res = s.fetch(&r, &profile(), &ctx).await.expect("fetch ok");

        let bytes = res.pdf_bytes.expect("pdf bytes set");
        assert!(bytes.starts_with(b"%PDF-"));
        assert_eq!(&bytes[..], &body[..]);
    }

    // -----------------------------------------------------------------
    // fetch — error paths
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn arxiv_fetch_with_doi_ref_errors_not_eligible() {
        let server = MockServer::start().await;
        let host = server
            .uri()
            .parse::<Url>()
            .unwrap()
            .host_str()
            .unwrap()
            .to_string();
        let (_td, ctx) = build_test_context(&host);
        let s = ArxivSource::with_base(server.uri().parse().unwrap());

        let r = Ref::Doi(Doi("10.1234/example".to_string()));
        let err = s
            .fetch(&r, &profile(), &ctx)
            .await
            .expect_err("doi ref must not be eligible");
        match err {
            FetchError::NotEligible { source_key } => {
                assert_eq!(source_key, "arxiv");
            }
            other => panic!("expected NotEligible, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn arxiv_fetch_writes_log_row_with_arxiv_default_license() {
        let server = MockServer::start().await;
        let body = b"%PDF-1.7\n%log-row fixture\n".to_vec();
        Mock::given(method("GET"))
            .and(path("/pdf/2401.12345.pdf"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
            .mount(&server)
            .await;
        let host = server
            .uri()
            .parse::<Url>()
            .unwrap()
            .host_str()
            .unwrap()
            .to_string();
        let (_td, ctx) = build_test_context(&host);
        // Capture the log path before the fetch call for later read-back.
        let log_path = ctx.log.path().to_path_buf();
        let s = ArxivSource::with_base(server.uri().parse().unwrap());

        let id = ArxivId::parse("2401.12345").unwrap();
        let r = Ref::Arxiv(id);
        let _ = s.fetch(&r, &profile(), &ctx).await.expect("fetch ok");

        let rows = read_rows(&log_path);
        assert_eq!(rows.len(), 1, "exactly one fetch row expected");
        let row = &rows[0];
        assert_eq!(row.source.as_deref(), Some("arxiv"));
        assert_eq!(row.ref_.as_deref(), Some("2401.12345"));
        assert_eq!(row.license.as_deref(), Some("arxiv-default"));
        assert_eq!(row.size_bytes, Some(body.len() as u64));
        assert!(row.error_code.is_none());
    }

    #[tokio::test]
    async fn arxiv_non_pdf_body_rejected() {
        // Wiremock returns 200 with a non-PDF body. The magic-byte check
        // inside `HttpClient::fetch_pdf` rejects it as `HttpError::NotAPdf`,
        // surfacing as `FetchError::Http`.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/pdf/2401.12345.pdf"))
            .respond_with(
                ResponseTemplate::new(200).set_body_bytes(b"<html>not a pdf</html>".to_vec()),
            )
            .mount(&server)
            .await;
        let host = server
            .uri()
            .parse::<Url>()
            .unwrap()
            .host_str()
            .unwrap()
            .to_string();
        let (_td, ctx) = build_test_context(&host);
        let s = ArxivSource::with_base(server.uri().parse().unwrap());

        let id = ArxivId::parse("2401.12345").unwrap();
        let r = Ref::Arxiv(id);
        let err = s
            .fetch(&r, &profile(), &ctx)
            .await
            .expect_err("non-pdf body must be rejected");
        match err {
            FetchError::Http(HttpError::NotAPdf { got }) => {
                assert_eq!(&got, b"<html");
            }
            other => panic!("expected FetchError::Http(NotAPdf), got {:?}", other),
        }
    }

    #[tokio::test]
    async fn arxiv_404_maps_to_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/pdf/2401.99999.pdf"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let host = server
            .uri()
            .parse::<Url>()
            .unwrap()
            .host_str()
            .unwrap()
            .to_string();
        let (_td, ctx) = build_test_context(&host);
        let s = ArxivSource::with_base(server.uri().parse().unwrap());

        let id = ArxivId::parse("2401.99999").unwrap();
        let r = Ref::Arxiv(id);
        let err = s
            .fetch(&r, &profile(), &ctx)
            .await
            .expect_err("404 must surface");
        match err {
            FetchError::Http(HttpError::HttpStatus { status, .. }) => {
                assert_eq!(status, 404);
            }
            other => panic!("expected FetchError::Http(HttpStatus), got {:?}", other),
        }
    }
}
