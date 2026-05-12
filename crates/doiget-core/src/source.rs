//! Source abstraction. Each Tier 1/2/3 fetcher implements this trait.
//!
//! Binding spec: `docs/PUBLIC_API.md` §2 (trait surface),
//! `docs/ARCHITECTURE.md` §6 (per-fetch data flow), and
//! `docs/PROVENANCE_LOG.md` §3 (the `Fetch` row source impls emit).
//!
//! Phase 1 ships the trait + supporting types; concrete impls (Crossref,
//! Unpaywall, arXiv) land in follow-up PRs (see `docs/SOURCES.md` for the
//! source matrix and tiering).

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use thiserror::Error;

use crate::http::{HttpClient, HttpError};
use crate::provenance::{LogError, ProvenanceLog};
use crate::rate_limiter::RateLimiter;
use crate::{CapabilityProfile, Ref, RefParseError};

/// What a successful fetch returns to the caller.
///
/// Whether `pdf_bytes` is `None` depends on the source: metadata-only
/// sources (Phase 4) leave it unset; OA sources (Phase 1) return PDF bytes
/// when an OA URL was discovered.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct FetchResult {
    /// Source's name (matches `Source::name()`); set for the audit trail.
    pub source: String,
    /// OA license string (`"CC-BY-4.0"`, `"unknown"`, etc.).
    pub license: String,
    /// PDF bytes; `None` for metadata-only sources.
    pub pdf_bytes: Option<Bytes>,
    /// Final URL after redirect resolution; useful for the metadata
    /// `[doiget].url` field.
    pub final_url: Option<url::Url>,
    /// Source-side metadata payload as a serde_json value. The Source impl
    /// is responsible for the shape; the caller (Phase 1+ orchestrator)
    /// maps it into `Metadata` when one exists (Phase 1+).
    pub metadata_json: Option<serde_json::Value>,
}

/// Per-fetch context shared by all `Source` impls.
///
/// Held by the orchestrator (CLI / MCP server) and passed by reference into
/// each [`Source::fetch`]. Sources MUST NOT construct their own
/// [`HttpClient`] / [`RateLimiter`] / [`ProvenanceLog`] — they go through
/// this context for uniform politeness, redirect allowlisting, and audit
/// logging.
#[derive(Clone)]
pub struct FetchContext {
    /// Shared, allowlist-aware HTTP client. See [`HttpClient`].
    pub http: Arc<HttpClient>,
    /// Process-wide async rate limiter. See [`RateLimiter`].
    pub rate_limiter: Arc<RateLimiter>,
    /// Append-only, hash-chained provenance log. Source impls MUST emit
    /// one `LogEvent::Fetch` row per attempt via `log.append`. See
    /// [`ProvenanceLog`].
    pub log: Arc<ProvenanceLog>,
    /// 26-char ULID identifying this process invocation. Mirrors the
    /// `session_id` stamped into every provenance row by the writer; held
    /// here so source impls can include it in their own structured logs
    /// without re-reading the env.
    pub session_id: String,
}

impl std::fmt::Debug for FetchContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Avoid printing the full HTTP / rate-limiter / log internals; only
        // the session_id is human-meaningful for log breadcrumbs.
        f.debug_struct("FetchContext")
            .field("session_id", &self.session_id)
            .finish_non_exhaustive()
    }
}

/// Errors returned by [`Source::fetch`].
///
/// At the public CLI / MCP boundary, every variant collapses to an
/// [`crate::ErrorCode`] via the `From<FetchError>` impl below — mirroring
/// the [`RefParseError`] → [`crate::ErrorCode::InvalidRef`] collapse from
/// PR #55.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum FetchError {
    /// The source does not handle the given ref under the runtime
    /// capability profile (covers both `can_serve = false` outcomes and
    /// runtime denials raised inside `fetch`).
    #[error("source {source_key} cannot serve this ref")]
    NotEligible {
        /// The source key that declined.
        source_key: String,
    },
    /// Tier 1 sources reported no OA URL for this ref.
    #[error("Tier 1 sources reported no OA URL for this ref")]
    NoOaAvailable,
    /// Underlying HTTP / network failure. See [`HttpError`].
    #[error("network error: {0}")]
    Http(#[from] HttpError),
    /// Provenance log write failed. Per `docs/SECURITY.md` §1.8 this is a
    /// fail-closed signal; the surrounding fetch MUST be aborted.
    #[error("provenance log error: {0}")]
    Log(#[from] LogError),
    /// Ref re-parse / validation failed inside the source (e.g. when a
    /// source receives a borrowed string from upstream and re-validates).
    #[error("invalid ref: {0}")]
    InvalidRef(#[from] RefParseError),
    /// Source-side schema mismatch (unexpected JSON shape, missing
    /// required field). Surfaces to [`crate::ErrorCode::InternalError`]
    /// at the public boundary.
    #[error("source-side schema error: {hint}")]
    SourceSchema {
        /// Human-readable hint at the offending field/path; not parsed.
        hint: String,
    },
}

/// Map [`FetchError`] to the closed [`crate::ErrorCode`] set surfaced at
/// the public CLI / MCP boundary. Mirrors the
/// `From<RefParseError> for ErrorCode` collapse from PR #55.
impl From<FetchError> for crate::ErrorCode {
    fn from(e: FetchError) -> crate::ErrorCode {
        match e {
            FetchError::NotEligible { .. } => crate::ErrorCode::CapabilityDenied,
            FetchError::NoOaAvailable => crate::ErrorCode::NoOaAvailable,
            FetchError::Http(_) => crate::ErrorCode::NetworkError,
            FetchError::Log(_) => crate::ErrorCode::LogError,
            FetchError::InvalidRef(_) => crate::ErrorCode::InvalidRef,
            FetchError::SourceSchema { .. } => crate::ErrorCode::InternalError,
        }
    }
}

/// Map a [`FetchError`] reference to the structured [`crate::DenialContext`]
/// channel introduced by ADR-0023 §4.
///
/// `&FetchError` (rather than `FetchError`) so the orchestrator can
/// produce the structured side-channel without consuming the error it
/// still needs for `error.message` and the `From<FetchError> for
/// ErrorCode` collapse above. The `Http` arm delegates to the
/// `From<&HttpError> for Option<DenialContext>` impl in [`crate::http`].
impl From<&FetchError> for Option<crate::DenialContext> {
    fn from(e: &FetchError) -> Self {
        use crate::{DenialContext, DenialReason};
        match e {
            FetchError::NotEligible { source_key } => Some(DenialContext {
                reason: DenialReason::CapabilityNotGranted,
                source: Some(source_key.clone()),
                attempted: None,
                expected: Vec::new(),
                hop_index: None,
                cap: None,
                actual: None,
            }),
            // Delegate to the HttpError mapping (ADR-0023 §4 mapping table).
            FetchError::Http(http_err) => http_err.into(),
            // Non-denial variants map to None per ADR-0023 §4.
            FetchError::NoOaAvailable
            | FetchError::Log(_)
            | FetchError::InvalidRef(_)
            | FetchError::SourceSchema { .. } => None,
        }
    }
}

/// The trait implemented by every Tier 1 / 2 / 3 fetcher.
///
/// Binding signature: `docs/PUBLIC_API.md` §2 (NORMATIVE — the wire shape
/// of these three methods is semver-locked).
#[async_trait]
pub trait Source: Send + Sync {
    /// Stable name used in metadata (`[doiget].source`) and provenance
    /// rows. Conventional values: `"crossref"`, `"unpaywall"`, `"arxiv"`,
    /// `"openalex"`, `"semantic-scholar"`, `"doaj"`, `"tdm-elsevier"`,
    /// etc. (see `docs/SOURCES.md`).
    fn name(&self) -> &str;

    /// True if this source can plausibly serve the given ref under the
    /// runtime capability profile. Implementations MUST be fast and
    /// non-blocking; the orchestrator calls `can_serve` to decide whether
    /// to invoke `fetch` at all.
    fn can_serve(&self, profile: &CapabilityProfile, ref_: &Ref) -> bool;

    /// Perform the source-specific fetch.
    ///
    /// Implementations:
    ///   1. acquire `ctx.rate_limiter.acquire(self.name()).await`,
    ///   2. fetch via `ctx.http.fetch_bytes` / `ctx.http.fetch_pdf`,
    ///   3. emit one `LogEvent::Fetch` row via `ctx.log.append`,
    ///   4. return a [`FetchResult`].
    ///
    /// The trait does NOT enforce these steps; it documents the protocol
    /// so concrete impls produce uniform audit trails (per
    /// `docs/ARCHITECTURE.md` §6 and `docs/PROVENANCE_LOG.md` §3).
    async fn fetch(
        &self,
        ref_: &Ref,
        profile: &CapabilityProfile,
        ctx: &FetchContext,
    ) -> Result<FetchResult, FetchError>;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    use camino::Utf8PathBuf;
    use tempfile::TempDir;

    use crate::http::{tier_1_allowlist, HttpClient};
    use crate::provenance::ProvenanceLog;
    use crate::rate_limiter::RateLimiter;
    use crate::{CapabilityProfile, Doi, ErrorCode, RateLimits, Ref};

    /// Minimal `Source` impl exercised purely to pin the trait shape and
    /// verify dispatch through `Box<dyn Source>`. Concrete sources land in
    /// follow-up PRs (Crossref / Unpaywall / arXiv).
    struct MockSource;

    #[async_trait]
    impl Source for MockSource {
        fn name(&self) -> &str {
            "mock"
        }
        fn can_serve(&self, _: &CapabilityProfile, _: &Ref) -> bool {
            true
        }
        async fn fetch(
            &self,
            _: &Ref,
            _: &CapabilityProfile,
            _: &FetchContext,
        ) -> Result<FetchResult, FetchError> {
            Ok(FetchResult {
                source: "mock".into(),
                license: "unknown".into(),
                pdf_bytes: None,
                final_url: None,
                metadata_json: None,
            })
        }
    }

    /// Build a `FetchContext` backed by real (but inert) Round-A
    /// foundation modules: a `HttpClient` over the Tier-1 allowlist, a
    /// `RateLimiter` at hard-coded politeness, and a `ProvenanceLog` in
    /// a tempdir. Returns the dir as well so the caller keeps it alive
    /// for the duration of the test.
    fn build_test_context() -> (TempDir, FetchContext) {
        let td = TempDir::new().expect("tempdir");
        // Workspace lints ban `std::path::PathBuf` for log paths; convert
        // via camino's `Utf8PathBuf::try_from`.
        let log_dir =
            Utf8PathBuf::try_from(td.path().to_path_buf()).expect("temp dir path must be UTF-8");
        let log_path = log_dir.join("test.jsonl");

        let http = Arc::new(HttpClient::new(tier_1_allowlist()).expect("http client builds"));
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
            },
        )
    }

    #[tokio::test]
    async fn mock_source_compiles_as_trait_object() {
        // Trait-shape pin: a `Source` impl is dyn-safe and can be boxed.
        let s: Box<dyn Source> = Box::new(MockSource);
        assert_eq!(s.name(), "mock");
        let profile = CapabilityProfile::from_env().expect("Phase 0 stub");
        let r = Ref::Doi(Doi("10.1234/example".to_string()));
        assert!(s.can_serve(&profile, &r));

        let (_td, ctx) = build_test_context();
        let res = s.fetch(&r, &profile, &ctx).await.expect("fetch ok");
        assert_eq!(res.source, "mock");
    }

    #[tokio::test]
    async fn mock_source_fetch_returns_result() {
        // Direct dispatch (not through `dyn`) to exercise the async fn
        // body and assert the populated FetchResult fields.
        let s = MockSource;
        let profile = CapabilityProfile::from_env().expect("Phase 0 stub");
        let r = Ref::Doi(Doi("10.1234/example".to_string()));
        let (_td, ctx) = build_test_context();

        let res = s.fetch(&r, &profile, &ctx).await.expect("fetch ok");
        assert_eq!(res.source, "mock");
        assert_eq!(res.license, "unknown");
        assert!(res.pdf_bytes.is_none());
        assert!(res.final_url.is_none());
        assert!(res.metadata_json.is_none());
    }

    #[test]
    fn fetch_error_collapses_to_error_code() {
        // Mirrors `docs/PUBLIC_API.md` §4 / PR #55 boundary collapse.
        // Each variant must map to its documented code.
        let e: ErrorCode = FetchError::NotEligible {
            source_key: "mock".into(),
        }
        .into();
        assert_eq!(e, ErrorCode::CapabilityDenied);

        let e: ErrorCode = FetchError::NoOaAvailable.into();
        assert_eq!(e, ErrorCode::NoOaAvailable);

        let e: ErrorCode = FetchError::Http(HttpError::UnknownSource {
            source_key: "mock".into(),
        })
        .into();
        assert_eq!(e, ErrorCode::NetworkError);

        let e: ErrorCode = FetchError::Log(LogError::Io(std::io::Error::other("synthetic"))).into();
        assert_eq!(e, ErrorCode::LogError);

        let e: ErrorCode = FetchError::InvalidRef(RefParseError::Empty).into();
        assert_eq!(e, ErrorCode::InvalidRef);

        let e: ErrorCode = FetchError::SourceSchema {
            hint: "missing field 'license'".into(),
        }
        .into();
        assert_eq!(e, ErrorCode::InternalError);
    }

    #[test]
    fn fetch_context_debug_redacts_internals() {
        // Pin the Debug shape — only `session_id` is printed, the rest is
        // elided. Prevents accidental log leakage when a context is
        // included in a `tracing::debug!` event.
        let (_td, ctx) = build_test_context();
        let s = format!("{:?}", ctx);
        assert!(
            s.contains("session_id"),
            "session_id must be in Debug: {}",
            s
        );
        assert!(s.contains("01J0000000000000000000TEST"));
        assert!(
            !s.contains("HttpClient") && !s.contains("RateLimiter") && !s.contains("ProvenanceLog"),
            "FetchContext Debug must not dump foundation internals: {}",
            s,
        );
    }

    // ---------------------------------------------------------------
    // FetchError -> Option<DenialContext>  (ADR-0023 §4)
    // ---------------------------------------------------------------

    #[test]
    fn denial_from_not_eligible_carries_source_key() {
        use crate::{DenialContext, DenialReason};
        let e = FetchError::NotEligible {
            source_key: "tdm-elsevier".to_string(),
        };
        let dc: Option<DenialContext> = (&e).into();
        let dc = dc.expect("NotEligible -> Some(DenialContext)");
        assert_eq!(dc.reason, DenialReason::CapabilityNotGranted);
        assert_eq!(dc.source.as_deref(), Some("tdm-elsevier"));
        assert!(dc.attempted.is_none());
        assert!(dc.expected.is_empty());
    }

    #[test]
    fn denial_from_http_delegates_to_http_mapping() {
        use crate::http::HttpError;
        use crate::{DenialContext, DenialReason, PDF_MAX_BYTES};
        // The Http arm must delegate to the HttpError mapping rather than
        // reinventing it, so an OversizedBody surfaces with cap/actual
        // populated and the SizeCapExceeded reason — proving delegation
        // works without per-variant duplication.
        let e = FetchError::Http(HttpError::OversizedBody {
            actual: 209_715_200,
            cap: PDF_MAX_BYTES,
        });
        let dc: Option<DenialContext> = (&e).into();
        let dc = dc.expect("Http(OversizedBody) -> Some(DenialContext)");
        assert_eq!(dc.reason, DenialReason::SizeCapExceeded);
        assert_eq!(dc.cap, Some(PDF_MAX_BYTES));
        assert_eq!(dc.actual, Some(209_715_200));
    }

    #[test]
    fn denial_from_non_denial_variants_returns_none() {
        use crate::DenialContext;
        // Each of the four non-denial FetchError arms maps to None per
        // ADR-0023 §4.
        let e = FetchError::NoOaAvailable;
        let dc: Option<DenialContext> = (&e).into();
        assert!(dc.is_none(), "NoOaAvailable must not produce DenialContext");

        let e = FetchError::Log(LogError::Io(std::io::Error::other("synthetic")));
        let dc: Option<DenialContext> = (&e).into();
        assert!(dc.is_none(), "Log must not produce DenialContext");

        let e = FetchError::InvalidRef(RefParseError::Empty);
        let dc: Option<DenialContext> = (&e).into();
        assert!(dc.is_none(), "InvalidRef must not produce DenialContext");

        let e = FetchError::SourceSchema {
            hint: "missing field 'license'".into(),
        };
        let dc: Option<DenialContext> = (&e).into();
        assert!(dc.is_none(), "SourceSchema must not produce DenialContext");
    }
}
