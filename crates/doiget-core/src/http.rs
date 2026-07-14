// allow: outbound-network
//! Centralized HTTP client wrapper. All `Source` impls fetch through here.
//!
//! Security defaults per `docs/SECURITY.md`:
//!   - rustls TLS only (no openssl, no native-tls — enforced by `deny.toml`)
//!   - HTTPS-only redirect policy (file://, data://, http:// rejected)
//!   - Per-source redirect host allowlist (`docs/REDIRECT_ALLOWLIST.md`)
//!   - Body size cap ([`crate::PDF_MAX_BYTES`] = 100 MB)
//!   - Per-request timeouts (connect 10s, read 60s, total 300s)
//!   - PDF magic-byte check on the first 5 bytes (`%PDF-`)
//!   - User-Agent: `doiget/<version> (+https://github.com/QAtlasHub/doiget)`
//!
//! See `docs/SECURITY.md` §1.2-1.3 / §1.10 and `docs/REDIRECT_ALLOWLIST.md`.
//!
//! # Architectural note: per-source `reqwest::Client`
//!
//! `reqwest::redirect::Policy::custom` receives only an `Attempt` value, which
//! exposes the next URL and previous URL chain but **not** the original
//! request's headers. That makes the "tag the request with `X-Doiget-Source`
//! and inspect it from inside the redirect closure" approach infeasible on
//! `reqwest 0.13.x`. Instead, [`HttpClient`] holds one
//! [`reqwest::Client`] per source — each client's redirect closure captures
//! that source's [`SourceAllowlist`] so cross-source confusion is impossible
//! by construction.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Once;
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use futures_util::StreamExt;
use reqwest::redirect::Policy;
use reqwest::{Client, ClientBuilder, Url};
use thiserror::Error;

use crate::{PDF_MAX_BYTES, VERSION};

/// PDF magic-byte prefix per the PDF 1.7 specification (ISO 32000-1 §7.5.2).
/// `b"%PDF-"`.
const PDF_MAGIC: [u8; 5] = [0x25, 0x50, 0x44, 0x46, 0x2D];

/// Hard cap on redirect chain length. Matches `reqwest`'s default of 10.
/// Re-asserted here so the value is reviewed alongside the other security
/// defaults in this module rather than inheriting silently from upstream.
const MAX_REDIRECTS: usize = 10;

/// Connect timeout per `docs/SECURITY.md` §1.2 (Slowloris row).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Read (idle-between-bytes) timeout per `docs/SECURITY.md` §1.2.
const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// Total per-request timeout per `docs/SECURITY.md` §1.2.
const TOTAL_TIMEOUT: Duration = Duration::from_secs(300);

/// Max retry attempts AFTER the first try, for transient failures only
/// (connect/timeout/mid-stream network errors and the transient HTTP
/// status set). 3 retries → up to 4 total attempts. See issue #117.
const MAX_FETCH_RETRIES: u32 = 3;

/// Base delay for the exponential backoff (`base * 2^attempt`, jittered).
const RETRY_BASE_DELAY: Duration = Duration::from_millis(500);

/// Hard ceiling on any single backoff / `Retry-After` sleep. Keeps the
/// worst-case retry chain comfortably inside [`TOTAL_TIMEOUT`].
const RETRY_MAX_DELAY: Duration = Duration::from_secs(30);

/// HTTP status codes worth retrying: request timeout, rate-limited, and
/// the transient 5xx family. A plain 500 is included because upstreams
/// (Crossref/Unpaywall) intermittently 500 under load. 4xx other than
/// 408/429 are caller/permanent and never retried.
fn is_transient_status(code: u16) -> bool {
    matches!(code, 408 | 429 | 500 | 502 | 503 | 504)
}

/// A `reqwest::Error` is transient iff it is a connect or timeout
/// failure or a mid-body transfer error. Redirect-policy aborts
/// (allowlist denial), builder errors, and decode errors are NOT
/// transient — retrying them cannot help and would mask a real denial.
fn reqwest_is_transient(e: &reqwest::Error) -> bool {
    (e.is_timeout() || e.is_connect() || e.is_body()) && !e.is_redirect()
}

/// Parse a `Retry-After` header expressed as integer seconds (the
/// HTTP-date form is accepted by the RFC but rare for these APIs and
/// deliberately ignored for the MVP — we fall back to exponential
/// backoff in that case). Capped at [`RETRY_MAX_DELAY`].
fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let secs: u64 = headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(Duration::from_secs(secs).min(RETRY_MAX_DELAY))
}

/// Exponential backoff with decorrelated jitter. `RETRY_BASE_DELAY *
/// 2^attempt`, capped at [`RETRY_MAX_DELAY`], plus 0..base jitter so a
/// fleet of clients does not thunder back in lockstep. Jitter is derived
/// from the wall-clock subsec nanos rather than pulling in an RNG
/// dependency — adequate decorrelation for backoff, not a security
/// primitive.
fn backoff_delay(attempt: u32) -> Duration {
    let factor = 1u64 << attempt.min(20);
    let base_ms = RETRY_BASE_DELAY.as_millis() as u64;
    let capped_ms = base_ms
        .saturating_mul(factor)
        .min(RETRY_MAX_DELAY.as_millis() as u64);
    let jitter_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| (d.subsec_nanos() as u64) % base_ms.max(1))
        .unwrap_or(0);
    Duration::from_millis(capped_ms.saturating_add(jitter_ms))
}

// ---------------------------------------------------------------------------
// SourceAllowlist
// ---------------------------------------------------------------------------

/// Per-source allowlist entry. Matches the schema in
/// `docs/REDIRECT_ALLOWLIST.md` §2.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SourceAllowlist {
    /// Source key. MUST match a `source` value in `docs/SOURCES.md` §1
    /// (e.g. `crossref`, `unpaywall`, `arxiv`).
    pub source: String,
    /// Each pattern is either a literal FQDN or a `*.<suffix>` glob (matches
    /// the suffix and any subdomain — see `docs/REDIRECT_ALLOWLIST.md` §2.2
    /// matching rule).
    pub redirect_hosts: Vec<String>,
}

impl SourceAllowlist {
    /// Construct a new allowlist entry.
    pub fn new(source: impl Into<String>, redirect_hosts: Vec<String>) -> Self {
        Self {
            source: source.into(),
            redirect_hosts,
        }
    }

    /// Returns `true` if `host` matches any pattern in this allowlist.
    ///
    /// Matching is byte-level on the lowercased ASCII form of the host.
    /// Callers MUST lowercase upstream; this method also lowercases as a
    /// defense-in-depth measure but treats the result as ASCII (Punycode
    /// is the caller's responsibility per `docs/REDIRECT_ALLOWLIST.md`
    /// §2.2 rule 4).
    pub fn matches(&self, host: &str) -> bool {
        let host_lc = host.to_ascii_lowercase();
        self.redirect_hosts
            .iter()
            .any(|pat| host_matches_pattern(&host_lc, pat))
    }
}

/// Returns `true` if `host` (already lowercased) matches `pattern` per
/// `docs/REDIRECT_ALLOWLIST.md` §2.2.
fn host_matches_pattern(host: &str, pattern: &str) -> bool {
    let pat_lc = pattern.to_ascii_lowercase();
    if let Some(suffix) = pat_lc.strip_prefix("*.") {
        // Suffix-glob: matches `<suffix>` exactly OR `*.<suffix>`.
        host == suffix || host.ends_with(&format!(".{}", suffix))
    } else {
        // Exact-FQDN: byte-identical (after lowercasing both sides).
        host == pat_lc
    }
}

/// Hard-coded Phase 1 allowlist for Tier 1 sources. Sourced from
/// `docs/REDIRECT_ALLOWLIST.md` §3.
///
/// Marked `Phase 1; revisit during real fetches` in the spec — entries
/// flagged `(unverified)` (e.g. arXiv subdomain redirect behavior) MUST be
/// confirmed or removed before Phase 1 is closed; see §3.3 of the spec.
pub fn tier_1_allowlist() -> Vec<SourceAllowlist> {
    vec![
        // §3.1 crossref
        SourceAllowlist::new(
            "crossref",
            vec!["api.crossref.org".to_string(), "*.crossref.org".to_string()],
        ),
        // §3.2 unpaywall
        SourceAllowlist::new("unpaywall", vec!["api.unpaywall.org".to_string()]),
        // §3.3 arxiv
        SourceAllowlist::new(
            "arxiv",
            vec![
                "arxiv.org".to_string(),
                "export.arxiv.org".to_string(),
                "*.arxiv.org".to_string(),
            ],
        ),
    ]
}

/// Hard-coded Phase 4 allowlist for Tier 2 metadata sources (OpenAlex,
/// Semantic Scholar, DOAJ). Sourced from `docs/SOURCES.md` §1 (the Tier 2
/// table) and `docs/REDIRECT_ALLOWLIST.md` §3 (same redirect-allowlist
/// policy as Tier 1, distinct source keys).
///
/// Returned hosts:
///
/// - `"openalex"` → `api.openalex.org` (production OpenAlex REST API).
/// - `"semantic_scholar"` → `api.semanticscholar.org` (S2 Graph API base).
/// - `"doaj"` → `doaj.org` + `*.doaj.org` (DOAJ public API; wildcard
///   covers `api.doaj.org` and any v4+ subdomain split).
///
/// Per `docs/SOURCES.md` §4 "OpenAlex / Semantic Scholar / DOAJ", these
/// sources are **metadata-only**: their `Source::fetch` impls MUST
/// return `pdf_bytes: None`. The redirect closure in [`HttpClient`]
/// uses this list to deny redirects to off-list hosts under each Tier
/// 2 source key — identical mechanism to Tier 1, but the per-tool
/// capability gate (`profile.metadata.openalex` etc.) is layered on
/// top so the network surface remains capability-aware.
pub fn tier_2_allowlist() -> Vec<SourceAllowlist> {
    vec![
        SourceAllowlist::new("openalex", vec!["api.openalex.org".to_string()]),
        SourceAllowlist::new(
            "semantic_scholar",
            vec!["api.semanticscholar.org".to_string()],
        ),
        SourceAllowlist::new(
            "doaj",
            vec!["doaj.org".to_string(), "*.doaj.org".to_string()],
        ),
    ]
}

/// Always-compiled allowlist for the **discovery search** call path
/// (ADR-0031).
///
/// Registers `api.openalex.org` under the `"openalex"` source key so the
/// Tier-1 `discovery::paper_search` (`GET /works?search=`) can reach the
/// endpoint in the **default `oa-only` binary** — unlike
/// [`tier_2_allowlist`], which the CLI only wires in under
/// `#[cfg(feature = "citation")]`.
///
/// Discovery search is classified as Tier 1 OA metadata (read-only, never
/// paywalled, never a PDF — same risk class as Crossref/Unpaywall), so its
/// transport allowlist must exist regardless of the `metadata`/`citation`
/// features (ADR-0031 D1/D2). The CLI's `build_http_client` extends the
/// production allowlist with this **unconditionally**; in `citation`
/// builds [`tier_2_allowlist`] re-registers the identical
/// `"openalex" → api.openalex.org` entry, which is a harmless idempotent
/// `HashMap` overwrite in [`HttpClient::new`].
pub fn discovery_allowlist() -> Vec<SourceAllowlist> {
    vec![SourceAllowlist::new(
        "openalex",
        vec!["api.openalex.org".to_string()],
    )]
}

/// Always-compiled allowlist for the **full-text extraction** call path
/// (ADR-0032).
///
/// Registers `ar5iv.labs.arxiv.org` under a dedicated `"ar5iv"` source key
/// so [`crate::paper_text::paper_text`] (`GET /html/<arxiv-id>`) can reach
/// the ar5iv LaTeXML-XHTML renderer in the **default `oa-only` binary** —
/// the same always-on posture as [`discovery_allowlist`].
///
/// The host is an arXiv subdomain (`*.arxiv.org` already matches it under
/// the [`tier_1_allowlist`] `"arxiv"` key), so this adds no new
/// registrable domain to the network surface — it only registers the host
/// under a **distinct source key** so the provenance trail records that
/// extracted text came from the ar5iv HTML renderer, not the arXiv
/// PDF/Atom API (ADR-0032 D3). Full-text extraction is classified Tier-1
/// OA metadata (read-only, OA, never a PDF reinterpretation), so its
/// transport allowlist must exist regardless of any feature gate
/// (ADR-0032 D2). The CLI's `build_http_client` extends the production
/// allowlist with this **unconditionally**.
pub fn fulltext_allowlist() -> Vec<SourceAllowlist> {
    vec![SourceAllowlist::new(
        "ar5iv",
        vec!["ar5iv.labs.arxiv.org".to_string()],
    )]
}

/// Hard-coded Phase 5a allowlist for the Springer Nature OA TDM
/// source. Compile-gated by the `tdm-springer` Cargo feature so
/// default release binaries never include the host pattern (per
/// ADR-0002 and `docs/SOURCES.md` §3).
///
/// Returned entry:
/// - `"tdm-springer"` → `api.springernature.com` (production base) +
///   `*.springernature.com` (covers load-balancing subdomains; the
///   redirect closure denies anything outside the wildcard).
///
/// Per `docs/SOURCES.md` §4 "TDM sources (Phase 5)", a fetch under
/// this source key requires ALL THREE gates: Cargo feature compiled
/// in, `DOIGET_KEY_SPRINGER` env var present, and
/// `DOIGET_AGREE_TDM_SPRINGER=1`. The `CapabilityProfile` gate
/// enforces the env-var pair; this allowlist is the transport gate.
#[cfg(feature = "tdm-springer")]
pub fn tier_3_springer_allowlist() -> Vec<SourceAllowlist> {
    vec![SourceAllowlist::new(
        "tdm-springer",
        vec![
            "api.springernature.com".to_string(),
            "*.springernature.com".to_string(),
        ],
    )]
}

/// Hard-coded Phase 5b allowlist for the APS Harvest TDM source.
/// Compile-gated by the `tdm-aps` Cargo feature so default release
/// binaries never include the host pattern (per ADR-0002 and
/// `docs/SOURCES.md` §3).
///
/// Returned entry:
/// - `"tdm-aps"` → `harvest.aps.org` (production base) +
///   `*.aps.org` (covers load-balancing subdomains; the redirect
///   closure denies anything outside the wildcard).
///
/// Three-gate activation: Cargo feature compiled in,
/// `DOIGET_KEY_APS` env var present, and `DOIGET_AGREE_TDM_APS=1`.
/// The `CapabilityProfile` gate enforces the env-var pair; this
/// allowlist is the transport gate.
#[cfg(feature = "tdm-aps")]
pub fn tier_3_aps_allowlist() -> Vec<SourceAllowlist> {
    vec![SourceAllowlist::new(
        "tdm-aps",
        vec!["harvest.aps.org".to_string(), "*.aps.org".to_string()],
    )]
}

/// Hard-coded Phase 5c allowlist for the Elsevier ScienceDirect TDM
/// source. Compile-gated by the `tdm-elsevier` Cargo feature so
/// default release binaries never include the host pattern (per
/// ADR-0002 and `docs/SOURCES.md` §3).
///
/// Returned entry:
/// - `"tdm-elsevier"` → `api.elsevier.com` (production base) +
///   `*.elsevier.com` (covers load-balancing subdomains; the
///   redirect closure denies anything outside the wildcard).
///
/// Three-gate activation: Cargo feature compiled in,
/// `DOIGET_KEY_ELSEVIER` env var present, and
/// `DOIGET_AGREE_TDM_ELSEVIER=1`. The `CapabilityProfile` gate
/// enforces the env-var pair; this allowlist is the transport gate.
#[cfg(feature = "tdm-elsevier")]
pub fn tier_3_elsevier_allowlist() -> Vec<SourceAllowlist> {
    vec![SourceAllowlist::new(
        "tdm-elsevier",
        vec!["api.elsevier.com".to_string(), "*.elsevier.com".to_string()],
    )]
}

/// Hard-coded Phase 1 allowlist for the synthetic `"oa-publisher"` source —
/// the publisher / preprint / repository hosts to which Unpaywall's
/// `best_oa_location.url` (or `url_for_pdf`) typically resolves.
///
/// **Status: informed-best-effort.** Per `docs/REDIRECT_ALLOWLIST.md` §3,
/// every entry below is a documented OA-publisher host pulled from the
/// public DOI / OA discovery surface as of this function's authoring; they
/// are **not** a substitute for empirical validation. Entries marked
/// `(unverified)` MUST be confirmed by a real fetch or removed before
/// Phase 1 is closed.
///
/// The orchestrator (`doiget-cli::commands::fetch::fetch_doi`) calls
/// [`HttpClient::fetch_pdf`] under the `"oa-publisher"` source key when
/// Unpaywall returns an OA URL. If the OA host is not in this list, the
/// PDF leg is denied (`HttpError::RedirectDenied`) and the orchestrator
/// falls back to metadata-only success (the `informed-best-effort`
/// posture from the spec section above).
pub fn oa_publisher_allowlist() -> Vec<SourceAllowlist> {
    vec![SourceAllowlist::new(
        "oa-publisher",
        vec![
            // Springer Nature OA imprints. Springer / SpringerOpen / Nature
            // OA URLs all resolve under one of these registrable suffixes.
            // (unverified) — confirm by replaying real Unpaywall responses.
            "*.springer.com".to_string(),
            "*.springeropen.com".to_string(),
            "*.springernature.com".to_string(),
            "*.nature.com".to_string(),
            // Wiley OA. (unverified)
            "*.wiley.com".to_string(),
            // Elsevier OA route only — the TDM gated path is a separate
            // source (`tdm-elsevier`, Phase 5c) and is not covered here.
            // (unverified)
            "*.elsevier.com".to_string(),
            "*.sciencedirect.com".to_string(),
            // Frontiers. (unverified)
            "*.frontiersin.org".to_string(),
            // MDPI. (unverified)
            "*.mdpi.com".to_string(),
            // PLOS. (unverified)
            "*.plos.org".to_string(),
            // Preprint servers — biorxiv / medrxiv. (unverified)
            "*.biorxiv.org".to_string(),
            "*.medrxiv.org".to_string(),
            // Europe PMC + NIH PMC. (unverified)
            "europepmc.org".to_string(),
            "*.europepmc.org".to_string(),
            "*.nih.gov".to_string(),
            "*.ncbi.nlm.nih.gov".to_string(),
            // Physics-society / diamond-OA hosts. UNLIKE the entries
            // above, these are EMPIRICALLY VERIFIED: a real `doiget batch`
            // over 30 OpenAlex-OA finite-temperature-MPS DOIs observed
            // Unpaywall `best_oa_location` resolving to these hosts and
            // being denied (#193, REDIRECT_ALLOWLIST.md §3.4, ADR-0027).
            // APS — journals.aps.org / link.aps.org (green & gold OA;
            // society host; `*.aps.org` is also trusted under the separate
            // `tdm-aps` Tier-3 source key WHEN that feature is compiled
            // in — `tier_3_aps_allowlist` is `#[cfg(feature = "tdm-aps")]`
            // and absent from default release builds).
            "*.aps.org".to_string(),
            // SciPost — diamond OA, community-run physics publisher.
            "scipost.org".to_string(),
            "*.scipost.org".to_string(),
            // IOP Publishing — iopscience.iop.org (New J. Phys. etc.).
            "*.iop.org".to_string(),
            // arXiv — already on the `arxiv` tier-1 allowlist, but the
            // Unpaywall-driven path uses the `oa-publisher` source key,
            // so we mirror the host list here too. See REDIRECT_ALLOWLIST.md
            // §3.3 for the underlying entries.
            "arxiv.org".to_string(),
            "*.arxiv.org".to_string(),
        ],
    )]
}

// ---------------------------------------------------------------------------
// HttpError
// ---------------------------------------------------------------------------

/// Errors that can arise during HTTP fetches.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum HttpError {
    /// Transport / DNS / TLS failure or other `reqwest`-level error. Note
    /// that `reqwest` surfaces a redirect-policy abort (via `Attempt::error`)
    /// as a `reqwest::Error` carrying the source error — callers seeing
    /// `Network` for what they believed was a redirect violation should
    /// inspect the inner error chain.
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    /// Redirect target host did not match any pattern in the source's
    /// `redirect_hosts`. See `docs/REDIRECT_ALLOWLIST.md` §2.2.
    ///
    /// Field naming: `source_key` rather than `source` because `thiserror`
    /// auto-treats a field literally named `source` as a `#[source]` error
    /// chain link (which would require the field to implement `std::error::Error`).
    ///
    /// `expected_hosts` carries a snapshot of the source's allowlist
    /// patterns at the time of the denial — populated for the structured
    /// `denial_context.expected` channel introduced by ADR-0023 §4
    /// (NORMATIVE mapping table). Cloning the patterns into the error
    /// keeps the `From<&HttpError> for Option<DenialContext>` impl from
    /// having to re-look-up the allowlist by `source_key`. May be empty
    /// when the rejection happened before any allowlist was matched
    /// (e.g. URL had no host component at all).
    #[error("redirect target {host} not in allowlist for source {source_key}")]
    RedirectDenied {
        /// Source key whose allowlist rejected the redirect.
        source_key: String,
        /// The lowercased host that was rejected.
        host: String,
        /// Snapshot of the source's `redirect_hosts` at denial time.
        /// Surfaces as `denial_context.expected` (ADR-0023 §4).
        expected_hosts: Vec<String>,
    },
    /// Redirect target had a scheme other than `https`. See
    /// `docs/SECURITY.md` §1.3.
    #[error("redirect to non-HTTPS scheme: {scheme}")]
    InsecureRedirect {
        /// The disallowed scheme (e.g. `http`, `file`, `data`).
        scheme: String,
    },
    /// Body would exceed [`PDF_MAX_BYTES`] either by a `Content-Length`
    /// hint or by accumulated streamed bytes. See `docs/SECURITY.md` §1.2.
    #[error("body too large: {actual} bytes (cap = {cap})")]
    OversizedBody {
        /// Observed size (header value or accumulated bytes).
        actual: u64,
        /// Hard upper bound (always [`PDF_MAX_BYTES`]).
        cap: u64,
    },
    /// PDF magic-byte mismatch — the body does not start with `%PDF-`.
    /// We deliberately do NOT use `Content-Type` (publishers misbehave —
    /// the magic byte is the trustworthy signal per `docs/SECURITY.md`
    /// §1.2 "Magic-byte mismatch" row).
    #[error("PDF magic-byte mismatch: got {got:?}")]
    NotAPdf {
        /// First five bytes of the response body (zero-padded if shorter).
        got: [u8; 5],
    },
    /// Server returned a non-2xx status.
    #[error("HTTP {status} from {url}")]
    HttpStatus {
        /// HTTP status code.
        status: u16,
        /// The URL that produced the status.
        url: String,
    },
    /// No allowlist entry exists for this source. The caller asked
    /// [`HttpClient`] to fetch on behalf of a source that wasn't passed to
    /// [`HttpClient::new`].
    ///
    /// See note on `RedirectDenied` for why the field is `source_key`.
    #[error("no allowlist registered for source {source_key}")]
    UnknownSource {
        /// The unregistered source key.
        source_key: String,
    },
    /// A header name or value passed to
    /// [`HttpClient::fetch_bytes_with_headers`] was not a valid HTTP
    /// header. The header parser only accepts the visible-ASCII subset
    /// per RFC 7230 §3.2; control characters and non-ASCII bytes are
    /// rejected before the request is even built. Surfaces as
    /// `ErrorCode::InternalError` at the public boundary (callers
    /// supplying bad headers are responsible for fixing the call site;
    /// not a denial in the ADR-0023 sense).
    #[error("invalid HTTP header `{name}`: {reason}")]
    InvalidHeader {
        /// The header name as supplied by the caller.
        name: String,
        /// `"name"` or `"value"` — which side failed parsing.
        reason: String,
    },
}

// ---------------------------------------------------------------------------
// HttpError -> Option<DenialContext>  (ADR-0023 §4 mapping table)
// ---------------------------------------------------------------------------

/// Map an [`HttpError`] reference to the structured [`crate::DenialContext`]
/// channel introduced by ADR-0023.
///
/// Returns `Some(_)` for the four denial classes named in ADR-0023 §4
/// (`RedirectDenied`, `OversizedBody`, `NotAPdf`, `InsecureRedirect`) and
/// `None` for every other variant — `Network`, `HttpStatus`,
/// `UnknownSource` are not denials in the ADR-0023 sense (they are
/// transport / upstream / programming-error signals, not allowlist or
/// cap rejections).
///
/// The `&HttpError` borrow form is used (rather than `HttpError`) so the
/// caller — typically the orchestrator that already needs the original
/// error for `error.message` and the `From<HttpError> for ErrorCode`
/// collapse — does not have to clone the error to produce the optional
/// structured side-channel.
impl From<&HttpError> for Option<crate::DenialContext> {
    fn from(e: &HttpError) -> Self {
        use crate::{DenialContext, DenialReason};
        match e {
            HttpError::RedirectDenied {
                source_key,
                host,
                expected_hosts,
            } => Some(DenialContext {
                reason: DenialReason::RedirectNotInAllowlist,
                source: Some(source_key.clone()),
                attempted: Some(host.clone()),
                expected: Some(expected_hosts.clone()),
                hop_index: None,
                cap: None,
                actual: None,
            }),
            HttpError::OversizedBody { actual, cap } => Some(DenialContext {
                reason: DenialReason::SizeCapExceeded,
                source: None,
                attempted: None,
                // The size-cap reason has no allowlist channel; use
                // `None` to signal "field not populated by producer"
                // rather than `Some(vec![])` (which would mean "explicit
                // empty allowlist"). See `DenialContext::expected` docs.
                expected: None,
                hop_index: None,
                cap: Some(*cap),
                actual: Some(*actual),
            }),
            HttpError::NotAPdf { got } => Some(DenialContext {
                reason: DenialReason::ContentTypeMismatch,
                source: None,
                // ADR-0023 §4 mapping table: hex-encode the first 5 bytes
                // for the `attempted` field. `format!("{:02x}...")` is
                // chosen over `hex::encode` to avoid pulling the
                // additional dep into this conversion path; the result is
                // bit-identical (lowercase, zero-padded).
                attempted: Some(format!(
                    "{:02x}{:02x}{:02x}{:02x}{:02x}",
                    got[0], got[1], got[2], got[3], got[4]
                )),
                expected: Some(vec!["%PDF-".to_string()]),
                hop_index: None,
                cap: None,
                actual: None,
            }),
            HttpError::InsecureRedirect { scheme } => Some(DenialContext {
                reason: DenialReason::InsecureScheme,
                source: None,
                attempted: Some(format!("{}:...", scheme)),
                expected: Some(vec!["https".to_string()]),
                hop_index: None,
                cap: None,
                actual: None,
            }),
            // `reqwest` wraps a custom error returned by the redirect
            // policy closure (`attempt.error(HttpError::RedirectDenied{..})`
            // / `attempt.error(HttpError::InsecureRedirect{..})`) inside a
            // `reqwest::Error`, which surfaces here as `HttpError::Network`.
            // Without source-chain walking, production redirect denials —
            // the most operationally important denial class — would never
            // produce a `DenialContext`, defeating the whole point of
            // ADR-0023.
            //
            // Walk the `std::error::Error::source()` chain on the inner
            // `reqwest::Error` and downcast each link to `&HttpError`. If
            // a wrapped `HttpError` is found, recurse via this same `From`
            // impl. Otherwise the network error is a "real" transport /
            // DNS / TLS failure with no denial semantics — return `None`.
            //
            // `std::error::Error::source(e)` is fully-qualified to
            // disambiguate against the inherent (and unrelated)
            // `reqwest::Error::source()`.
            HttpError::Network(e) => {
                let mut source: Option<&(dyn std::error::Error + 'static)> =
                    std::error::Error::source(e);
                while let Some(s) = source {
                    if let Some(http_err) = s.downcast_ref::<HttpError>() {
                        return Option::<crate::DenialContext>::from(http_err);
                    }
                    source = s.source();
                }
                None
            }
            // The remaining variants are not "denials" in the ADR-0023
            // sense — HttpStatus/UnknownSource are upstream / programming-
            // error signals; InvalidHeader is a caller-bug signal.
            HttpError::HttpStatus { .. }
            | HttpError::UnknownSource { .. }
            | HttpError::InvalidHeader { .. } => None,
        }
    }
}

// ---------------------------------------------------------------------------
// HttpClient
// ---------------------------------------------------------------------------

/// Workspace-wide HTTP client with the security defaults applied.
///
/// Internally holds one `reqwest::Client` per source. Construct via
/// [`HttpClient::new`] with the full set of allowlists the calling process
/// will need.
#[derive(Clone, Debug)]
pub struct HttpClient {
    /// One [`reqwest::Client`] per source. Each client carries a redirect
    /// policy that captures only that source's allowlist. `Arc` so cloning
    /// is cheap.
    clients: Arc<HashMap<String, Client>>,
    /// The exact [`SourceAllowlist`] each per-source client was built from,
    /// keyed by source. The redirect closure inside each `reqwest::Client`
    /// captures its allowlist *by move*, so it cannot be read back from the
    /// client itself. This map keeps the identical `SourceAllowlist`
    /// available to callers that must perform a *pre-fetch* host check on a
    /// metadata-discovered URL (issue #145 / `docs/REDIRECT_ALLOWLIST.md`
    /// §1: the allowlist is consulted "on the OA URL discovered through
    /// metadata sources before the actual PDF fetch is issued", not only on
    /// redirect hops). Storing the same value here — rather than re-deriving
    /// it from [`oa_publisher_allowlist`] at the call site — guarantees the
    /// pre-check and the redirect closure can never drift, and that the
    /// check works under the test constructors too (which register a
    /// wiremock host as the allowlist).
    allowlists: Arc<HashMap<String, SourceAllowlist>>,
}

impl HttpClient {
    /// Build a client with rustls + redirect-allowlist + size cap +
    /// timeouts.
    ///
    /// `allowlists` MUST cover every source whose URL might be passed in;
    /// fetches against unregistered sources return
    /// [`HttpError::UnknownSource`].
    ///
    /// # Errors
    ///
    /// Returns the underlying `reqwest::Error` if `ClientBuilder::build`
    /// fails (typically a TLS-backend init failure).
    pub fn new(allowlists: Vec<SourceAllowlist>) -> Result<Self, reqwest::Error> {
        let ua = format!("doiget/{} (+https://github.com/QAtlasHub/doiget)", VERSION);
        Self::new_with_user_agent(allowlists, &ua)
    }

    /// Build a client with a custom `User-Agent` header.
    ///
    /// Used by `doiget batch --user-agent` to override the default UA for
    /// hosts that classify the default string as a bot.
    pub fn new_with_user_agent(
        allowlists: Vec<SourceAllowlist>,
        user_agent: &str,
    ) -> Result<Self, reqwest::Error> {
        let mut clients = HashMap::with_capacity(allowlists.len());
        let mut allowlist_map = HashMap::with_capacity(allowlists.len());
        for entry in allowlists {
            let source = entry.source.clone();
            allowlist_map.insert(source.clone(), entry.clone());
            let client = build_client(entry, user_agent)?;
            clients.insert(source, client);
        }
        Ok(Self {
            clients: Arc::new(clients),
            allowlists: Arc::new(allowlist_map),
        })
    }

    /// The [`SourceAllowlist`] this client was built with for `source`, or
    /// `None` if `source` was not registered.
    ///
    /// This is the *identical* value captured by the per-source redirect
    /// closure (see [`HttpClient`]'s `allowlists` field doc). It exists so
    /// the orchestrator can apply the `docs/REDIRECT_ALLOWLIST.md` §1
    /// pre-fetch host check on a metadata-discovered OA URL — the URL that
    /// is fetched *without* necessarily passing through a redirect hop —
    /// using the same source of truth the redirect closure uses, so the two
    /// can never disagree. Callers MUST use this for the `"oa-publisher"`
    /// leg only; the initial template-constructed URL is exempt per
    /// `docs/REDIRECT_ALLOWLIST.md` §6.
    pub fn source_allowlist(&self, source: &str) -> Option<&SourceAllowlist> {
        self.allowlists.get(source)
    }

    /// Fetch a URL, treating it as a JSON or text body. Caps at
    /// [`PDF_MAX_BYTES`].
    ///
    /// Returns the response body bytes plus the effective final URL after
    /// redirects (post-allowlist verification — every hop has already been
    /// validated by the time this returns).
    ///
    /// # Errors
    ///
    /// Any [`HttpError`] variant.
    pub async fn fetch_bytes(&self, source: &str, url: Url) -> Result<(Bytes, Url), HttpError> {
        self.fetch_inner(source, url, &[], false).await
    }

    /// Like [`Self::fetch_bytes`] but attaches additional request
    /// headers to the outgoing GET. The headers are validated up-front
    /// against the visible-ASCII subset (RFC 7230 §3.2); any failure
    /// returns [`HttpError::InvalidHeader`] before the request is sent.
    ///
    /// Used by Tier-3 TDM sources that authenticate via a header
    /// (APS Harvest `X-API-Key`, Elsevier ScienceDirect `X-ELS-APIKey`).
    /// Header values appear on the wire only — they are never logged.
    ///
    /// # Errors
    ///
    /// Any [`HttpError`] variant including [`HttpError::InvalidHeader`].
    pub async fn fetch_bytes_with_headers(
        &self,
        source: &str,
        url: Url,
        headers: &[(&str, &str)],
    ) -> Result<(Bytes, Url), HttpError> {
        self.fetch_inner(source, url, headers, false).await
    }

    /// Fetch a URL expected to be a PDF. Same as [`Self::fetch_bytes`] plus
    /// the magic-byte check on the first 5 bytes
    /// (`%PDF-` = `[0x25, 0x50, 0x44, 0x46, 0x2D]`). Mismatch returns
    /// [`HttpError::NotAPdf`].
    ///
    /// # Errors
    ///
    /// Any [`HttpError`] variant including [`HttpError::NotAPdf`].
    pub async fn fetch_pdf(&self, source: &str, url: Url) -> Result<(Bytes, Url), HttpError> {
        self.fetch_inner(source, url, &[], true).await
    }

    async fn fetch_inner(
        &self,
        source: &str,
        url: Url,
        headers: &[(&str, &str)],
        check_pdf_magic: bool,
    ) -> Result<(Bytes, Url), HttpError> {
        // Normalise legacy `http://` URLs returned by OpenAlex /
        // Unpaywall metadata before send. See `upgrade_http_to_https`
        // for the rationale (TLS posture preserved per ADR-0020) and
        // the loopback carve-out.
        let url = upgrade_http_to_https(url);

        let client = self
            .clients
            .get(source)
            .ok_or_else(|| HttpError::UnknownSource {
                source_key: source.to_string(),
            })?;

        // Parse headers up-front so an invalid name/value fails BEFORE
        // we touch the network. `HeaderName::from_bytes` / `HeaderValue::from_str`
        // accept the visible-ASCII subset only (RFC 7230 §3.2).
        let mut header_map = reqwest::header::HeaderMap::with_capacity(headers.len());
        for (name, value) in headers {
            let hn = reqwest::header::HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
                HttpError::InvalidHeader {
                    name: (*name).to_string(),
                    reason: "name".to_string(),
                }
            })?;
            let hv = reqwest::header::HeaderValue::from_str(value).map_err(|_| {
                HttpError::InvalidHeader {
                    name: (*name).to_string(),
                    reason: "value".to_string(),
                }
            })?;
            header_map.insert(hn, hv);
        }

        // Bounded retry loop (issue #117). Only transient classes are
        // retried — connect/timeout/mid-stream network errors and the
        // transient HTTP status set. Allowlist denials, NotAPdf,
        // OversizedBody, 4xx (non-408/429) are deterministic and return
        // on the first occurrence. GET is idempotent so a retried
        // attempt re-streams the body from scratch.
        let mut attempt: u32 = 0;
        loop {
            let send_result = client
                .get(url.clone())
                .headers(header_map.clone())
                .send()
                .await;
            let response = match send_result {
                Ok(r) => r,
                Err(e) => {
                    if attempt < MAX_FETCH_RETRIES && reqwest_is_transient(&e) {
                        let d = backoff_delay(attempt);
                        tracing::warn!(
                            source,
                            attempt,
                            delay_ms = d.as_millis() as u64,
                            error = %e,
                            "transient send failure; retrying"
                        );
                        tokio::time::sleep(d).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(HttpError::Network(e));
                }
            };
            let final_url = response.url().clone();

            // Status check before body read so we can fail fast.
            let status = response.status();
            if !status.is_success() {
                let code = status.as_u16();
                if attempt < MAX_FETCH_RETRIES && is_transient_status(code) {
                    // Prefer the server's `Retry-After` over our backoff
                    // when present (429/503 commonly carry it).
                    let d = parse_retry_after(response.headers())
                        .unwrap_or_else(|| backoff_delay(attempt));
                    tracing::warn!(
                        source,
                        attempt,
                        status = code,
                        delay_ms = d.as_millis() as u64,
                        "transient HTTP status; retrying"
                    );
                    tokio::time::sleep(d).await;
                    attempt += 1;
                    continue;
                }
                return Err(HttpError::HttpStatus {
                    status: code,
                    // Issue #146: Springer Nature authenticates via an
                    // `api_key` URL query parameter (no header path
                    // upstream). This error string is logged and may
                    // surface to the user, so strip any `api_key`
                    // value before it leaves the client. No other
                    // source puts a secret in the query string, so
                    // this is a no-op for them.
                    url: redact_api_key_query(&final_url),
                });
            }

            // Content-Length fast-path: if header is present and exceeds
            // the cap, fail without reading any body (deterministic — not
            // retried). Per `docs/SECURITY.md` §1.2.
            if let Some(len) = response.content_length() {
                if len > PDF_MAX_BYTES {
                    return Err(HttpError::OversizedBody {
                        actual: len,
                        cap: PDF_MAX_BYTES,
                    });
                }
            }

            // Stream body and enforce the cap as bytes accumulate. A
            // mid-stream transport error is transient (retry); an
            // oversized body is deterministic (return).
            let mut buf = BytesMut::new();
            let mut stream = response.bytes_stream();
            let mut oversized_at: Option<u64> = None;
            let mut stream_err: Option<reqwest::Error> = None;
            while let Some(chunk) = stream.next().await {
                let chunk = match chunk {
                    Ok(c) => c,
                    Err(e) => {
                        stream_err = Some(e);
                        break;
                    }
                };
                let projected = (buf.len() as u64).saturating_add(chunk.len() as u64);
                if projected > PDF_MAX_BYTES {
                    oversized_at = Some(projected);
                    break;
                }
                buf.extend_from_slice(&chunk);
            }
            if let Some(actual) = oversized_at {
                return Err(HttpError::OversizedBody {
                    actual,
                    cap: PDF_MAX_BYTES,
                });
            }
            if let Some(e) = stream_err {
                if attempt < MAX_FETCH_RETRIES && reqwest_is_transient(&e) {
                    let d = backoff_delay(attempt);
                    tracing::warn!(
                        source,
                        attempt,
                        delay_ms = d.as_millis() as u64,
                        error = %e,
                        "transient mid-stream failure; retrying"
                    );
                    tokio::time::sleep(d).await;
                    attempt += 1;
                    continue;
                }
                return Err(HttpError::Network(e));
            }
            let body = buf.freeze();

            if check_pdf_magic {
                let mut got = [0u8; 5];
                let n = body.len().min(5);
                got[..n].copy_from_slice(&body[..n]);
                if got != PDF_MAGIC {
                    return Err(HttpError::NotAPdf { got });
                }
            }

            return Ok((body, final_url));
        }
    }
}

/// Return `url` rendered as a string with the value of any `api_key`
/// query parameter replaced by `REDACTED` (issue #146).
///
/// Springer Nature's TDM API authenticates **only** via an `api_key`
/// query parameter — there is no header-auth path upstream — so the key
/// is unavoidably in the request URL. This keeps it out of *our* log
/// and error sinks (the `HttpError::HttpStatus` string in particular,
/// which is `tracing`-logged and can surface to the user). It is a
/// structural no-op for every other source, none of which carry a
/// secret in the query string. Other pairs and their order are
/// preserved; a URL with no `api_key` pair is rendered unchanged.
fn redact_api_key_query(url: &url::Url) -> String {
    const API_KEY_PARAM: &str = "api_key";
    if url.query_pairs().all(|(k, _)| k != API_KEY_PARAM) {
        return url.to_string();
    }
    let mut redacted = url.clone();
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| {
            if k == API_KEY_PARAM {
                (k.into_owned(), "REDACTED".to_string())
            } else {
                (k.into_owned(), v.into_owned())
            }
        })
        .collect();
    redacted.query_pairs_mut().clear().extend_pairs(pairs);
    redacted.to_string()
}

/// Test-oriented [`HttpClient`] constructor. Originally `cfg(test)`; now
/// also reachable from the `doiget-cli` orchestrator's integration tests
/// (which live outside this crate and therefore cannot see `cfg(test)`-gated
/// items). The constructor name retains its `for_tests_allow_http` signal —
/// production code MUST use [`HttpClient::new`] with [`tier_1_allowlist`].
#[allow(clippy::expect_used)]
impl HttpClient {
    /// Build a test-oriented `HttpClient` against an `http://` wiremock
    /// origin. The redirect closure still rejects insecure schemes — we only
    /// relax `https_only` at the connection level so wiremock can serve.
    /// This is acceptable because the redirect closure (which is the
    /// security-load-bearing path) is exercised by the
    /// `redirect_to_http_is_rejected_by_closure` test below.
    ///
    /// Production callers MUST use [`HttpClient::new`] with
    /// [`tier_1_allowlist`] — the `for_tests_allow_http` suffix is the load-
    /// bearing signal that this constructor lifts the initial-leg HTTPS-only
    /// requirement.
    pub fn new_for_tests_allow_http(source: &str, allowlist_host: &str) -> Self {
        let allowlist = SourceAllowlist::new(source, vec![allowlist_host.to_string()]);
        let client = build_client_allow_http(allowlist.clone()).expect("test client builds");
        let mut map = HashMap::new();
        let mut allowlist_map = HashMap::new();
        allowlist_map.insert(allowlist.source.clone(), allowlist.clone());
        map.insert(allowlist.source.clone(), client);
        Self {
            clients: Arc::new(map),
            allowlists: Arc::new(allowlist_map),
        }
    }

    /// Multi-source variant of [`HttpClient::new_for_tests_allow_http`].
    ///
    /// Builds a relaxed-`https_only` client per `(source, allowlist_host)`
    /// pair. Used by the `doiget-cli` orchestrator's integration tests when
    /// more than one upstream needs to be wiremocked simultaneously
    /// (e.g. Crossref + Unpaywall against two different mock servers).
    /// Production callers MUST use [`HttpClient::new`] with
    /// [`tier_1_allowlist`].
    pub fn new_for_tests_allow_http_multi(entries: &[(&str, &str)]) -> Self {
        let mut map = HashMap::with_capacity(entries.len());
        let mut allowlist_map = HashMap::with_capacity(entries.len());
        for (source, host) in entries {
            let allowlist = SourceAllowlist::new(*source, vec![host.to_string()]);
            let client = build_client_allow_http(allowlist.clone()).expect("test client builds");
            allowlist_map.insert(allowlist.source.clone(), allowlist.clone());
            map.insert(allowlist.source.clone(), client);
        }
        Self {
            clients: Arc::new(map),
            allowlists: Arc::new(allowlist_map),
        }
    }
}

fn build_client_allow_http(allowlist: SourceAllowlist) -> Result<Client, reqwest::Error> {
    ensure_crypto_provider();
    let allowlist_for_closure = allowlist.clone();
    let redirect_policy = Policy::custom(move |attempt| {
        let scheme = attempt.url().scheme().to_string();
        let host_opt = attempt.url().host_str().map(|h| h.to_ascii_lowercase());
        let prev_count = attempt.previous().len();
        if scheme != "https" {
            return attempt.error(HttpError::InsecureRedirect { scheme });
        }
        if prev_count >= MAX_REDIRECTS {
            return attempt.stop();
        }
        let host = match host_opt {
            Some(h) => h,
            None => {
                return attempt.error(HttpError::RedirectDenied {
                    source_key: allowlist_for_closure.source.clone(),
                    host: String::new(),
                    expected_hosts: allowlist_for_closure.redirect_hosts.clone(),
                });
            }
        };
        if !allowlist_for_closure.matches(&host) {
            return attempt.error(HttpError::RedirectDenied {
                source_key: allowlist_for_closure.source.clone(),
                host,
                expected_hosts: allowlist_for_closure.redirect_hosts.clone(),
            });
        }
        attempt.follow()
    });
    ClientBuilder::new()
        // `https_only(false)` only at this scope — production builders
        // (the public `HttpClient::new`) keep it on.
        .https_only(false)
        .redirect(redirect_policy)
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(TOTAL_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .user_agent(format!(
            "doiget/{} (+https://github.com/QAtlasHub/doiget)",
            VERSION
        ))
        .tls_backend_rustls()
        .build()
}

// ---------------------------------------------------------------------------
// ClientBuilder helpers
// ---------------------------------------------------------------------------

/// Install the `ring` `rustls` crypto provider as the process default,
/// exactly once.
///
/// reqwest is built with the `rustls-no-provider` feature (ADR-0020
/// Amendment 1: drop aws-lc-rs so `cargo install` needs no cmake/C
/// toolchain and musl-static builds cleanly). With no bundled provider,
/// `reqwest::ClientBuilder::build` calls
/// `rustls::crypto::CryptoProvider::get_default()` and **panics**
/// (`"No provider set"`) unless a process-default provider was installed
/// first. Every client constructor below calls this; the `Once` makes it
/// safe to invoke from many sites and from concurrent tests.
fn ensure_crypto_provider() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        // `install_default` errors only if a provider is already set;
        // under `Once` that is unreachable, but ignore it rather than
        // panic (another linked crate could have installed one first).
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// Public entry point for callers that build their own `reqwest::Client`
/// outside of [`HttpClient`] and need the process-default TLS provider
/// installed first (ADR-0020 Amendment 1).
///
/// Safe to call multiple times; the underlying `Once` makes it idempotent.
pub fn init_tls() {
    ensure_crypto_provider();
}

/// Upgrade an `http://` URL to `https://` for legacy publisher
/// metadata. Loopback hosts (`localhost`, any RFC 6761 `.localhost`
/// TLD subdomain, `127.0.0.0/8`, `::1`, IPv4-mapped IPv6 loopback)
/// are returned unchanged so the `new_for_tests_allow_http*` wiremock
/// path continues to talk plain HTTP to the local fixture server.
///
/// Non-`http` schemes (`https`, `file`, anything else) and cannot-be-
/// base URLs are returned unchanged. The function is total: it never
/// panics and never returns an error.
///
/// # Audit / posture
///
/// On a successful upgrade the function emits a `tracing::info!` event
/// so the rewrite appears in the operator's default-level structured
/// log. On the (in-practice unreachable) `set_scheme` failure path a
/// `tracing::warn!` event is emitted before returning the original
/// URL; the production client's `https_only(true)` then rejects the
/// send with a clear network error, preserving the TLS posture
/// established by ADR-0020.
///
/// # `Domain("localhost")` arm subtlety
///
/// The url crate resolves the bare host `localhost` to `127.0.0.1`
/// (Ipv4 variant) when parsing an `http://` URL, so the `Domain` arm
/// does NOT fire for that case (the `Ipv4` arm catches it). The arm
/// IS load-bearing for the RFC 6761 `.localhost` TLD (e.g.
/// `myservice.localhost`, `api.localhost`), which the url crate does
/// NOT auto-resolve to an IP and keeps as `Host::Domain`.
fn upgrade_http_to_https(url: Url) -> Url {
    if url.scheme() != "http" {
        return url;
    }
    match url.host() {
        None => {
            // Cannot-be-base URL (e.g. `http:foo`) — `set_scheme`
            // would reject the conversion.
            return url;
        }
        Some(url::Host::Domain(d)) if is_localhost_domain(d) => return url,
        Some(url::Host::Ipv4(ip)) if ip.is_loopback() => return url,
        Some(url::Host::Ipv6(ip)) if is_ipv6_loopback(ip) => return url,
        Some(_) => {}
    }
    let mut upgraded = url.clone();
    if upgraded.set_scheme("https").is_err() {
        // url-crate `set_scheme` is documented to fail only for
        // cannot-be-base URLs and a few cross-family transitions;
        // `http -> https` is supported because both are "special"
        // schemes. The fallback below is defence-in-depth.
        tracing::warn!(
            url = %url,
            "set_scheme(http -> https) failed unexpectedly; \
             sending original URL — https_only(true) will reject",
        );
        return url;
    }
    tracing::info!(
        original = %url,
        upgraded = %upgraded,
        "upgraded http -> https for legacy publisher metadata"
    );
    upgraded
}

/// `true` for the `localhost` literal and any RFC 6761 `.localhost`
/// TLD subdomain (`myservice.localhost`, `api.localhost`, etc.).
/// ASCII-case-insensitive per host-name conventions.
fn is_localhost_domain(d: &str) -> bool {
    if d.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let suffix = ".localhost";
    let d_bytes = d.as_bytes();
    let s_bytes = suffix.as_bytes();
    if d_bytes.len() <= s_bytes.len() {
        return false;
    }
    let tail = &d_bytes[d_bytes.len() - s_bytes.len()..];
    tail.eq_ignore_ascii_case(s_bytes)
}

/// `true` for `::1` and any IPv4-mapped loopback
/// (`::ffff:127.0.0.0/8`). `Ipv6Addr::is_loopback()` covers only `::1`,
/// so dual-stack callers that hit `[::ffff:127.0.0.1]` would otherwise
/// be silently upgraded.
fn is_ipv6_loopback(ip: std::net::Ipv6Addr) -> bool {
    if ip.is_loopback() {
        return true;
    }
    matches!(ip.to_ipv4_mapped(), Some(v4) if v4.is_loopback())
}

fn build_client(allowlist: SourceAllowlist, ua: &str) -> Result<Client, reqwest::Error> {
    ensure_crypto_provider();

    let user_agent = ua.to_string();

    // Redirect policy: capture the per-source allowlist by value. The
    // closure is called for every redirect hop — there is no global
    // fallback, every hop is checked. Hard cap at MAX_REDIRECTS via the
    // attempt counter (mirrors reqwest's built-in limit).
    let allowlist_for_closure = allowlist.clone();
    let redirect_policy = Policy::custom(move |attempt| {
        // Inspect the candidate URL via owned copies so we can move
        // `attempt` into `error()` / `follow()` / `stop()` later without
        // the borrow checker complaining about an outstanding borrow of
        // `attempt`.
        let scheme = attempt.url().scheme().to_string();
        let host_opt = attempt.url().host_str().map(|h| h.to_ascii_lowercase());
        let prev_count = attempt.previous().len();

        // 1. Reject non-HTTPS up front. The `https_only(true)` builder
        //    flag below also catches this, but we want the dedicated
        //    `InsecureRedirect` error path (not a generic `https_only`
        //    abort) — see `docs/SECURITY.md` §1.3.
        if scheme != "https" {
            return attempt.error(HttpError::InsecureRedirect { scheme });
        }

        // 2. Hop limit (`docs/SECURITY.md` §1.3 redirect_limit row).
        if prev_count >= MAX_REDIRECTS {
            return attempt.stop();
        }

        // 3. Allowlist check on the candidate target host.
        //    `host_str()` is `None` for URLs without a host (e.g. data
        //    URIs); treat that as an allowlist miss.
        let host = match host_opt {
            Some(h) => h,
            None => {
                return attempt.error(HttpError::RedirectDenied {
                    source_key: allowlist_for_closure.source.clone(),
                    host: String::new(),
                    expected_hosts: allowlist_for_closure.redirect_hosts.clone(),
                });
            }
        };
        if !allowlist_for_closure.matches(&host) {
            return attempt.error(HttpError::RedirectDenied {
                source_key: allowlist_for_closure.source.clone(),
                host,
                expected_hosts: allowlist_for_closure.redirect_hosts.clone(),
            });
        }

        attempt.follow()
    });

    ClientBuilder::new()
        .https_only(true)
        .redirect(redirect_policy)
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(TOTAL_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .user_agent(user_agent)
        // `tls_backend_rustls()` is the non-deprecated equivalent of the
        // older `use_rustls_tls()`. The workspace pins reqwest with
        // `rustls-no-provider` (ADR-0020 Amendment 1), so this is a
        // re-assertion at builder level rather than a feature switch; the
        // `ring` provider installed by `ensure_crypto_provider()` above
        // is what reqwest picks up via `CryptoProvider::get_default()`.
        .tls_backend_rustls()
        .build()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // ---------------------------------------------------------------
    // http -> https scheme upgrade (#220) — pure unit tests, no network.
    // ---------------------------------------------------------------

    #[test]
    fn upgrade_http_to_https_rewrites_public_http_url() {
        let input = Url::parse("http://link.aps.org/pdf/10.1103/PhysRev.123.456").unwrap();
        let out = upgrade_http_to_https(input.clone());
        assert_eq!(out.scheme(), "https");
        assert_eq!(out.host_str(), Some("link.aps.org"));
        assert_eq!(out.path(), "/pdf/10.1103/PhysRev.123.456");
    }

    #[test]
    fn upgrade_http_to_https_preserves_port_path_query_fragment() {
        let input = Url::parse("http://example.org:8080/a/b?q=1#frag").unwrap();
        let out = upgrade_http_to_https(input);
        assert_eq!(out.as_str(), "https://example.org:8080/a/b?q=1#frag");
    }

    #[test]
    fn upgrade_http_to_https_is_idempotent_on_https() {
        let input = Url::parse("https://api.crossref.org/works/10.1234/foo").unwrap();
        let out = upgrade_http_to_https(input.clone());
        assert_eq!(out, input);
    }

    #[test]
    fn upgrade_http_to_https_skips_localhost() {
        // wiremock binds to `127.0.0.1:PORT`; the loopback exception
        // is the load-bearing rule that keeps `new_for_tests_allow_http*`
        // working alongside the production fetch path.
        let input = Url::parse("http://localhost:7878/pdf").unwrap();
        let out = upgrade_http_to_https(input.clone());
        assert_eq!(out, input, "localhost MUST NOT be upgraded");
    }

    #[test]
    fn upgrade_http_to_https_skips_127_loopback_block() {
        for host in ["127.0.0.1", "127.0.0.42", "127.255.255.254"] {
            let raw = format!("http://{host}:1234/x");
            let input = Url::parse(&raw).unwrap();
            let out = upgrade_http_to_https(input.clone());
            assert_eq!(out, input, "host `{host}` MUST NOT be upgraded");
        }
    }

    #[test]
    fn upgrade_http_to_https_skips_ipv6_loopback() {
        let input = Url::parse("http://[::1]:9000/path").unwrap();
        let out = upgrade_http_to_https(input.clone());
        assert_eq!(out, input, "IPv6 loopback MUST NOT be upgraded");
    }

    #[test]
    fn upgrade_http_to_https_preserves_case_in_path() {
        // Some publishers (e.g. APS legacy redirects) use mixed-case
        // path segments; upgrade must NOT lowercase or canonicalise.
        let input = Url::parse("http://link.aps.org/PDF/10.1103/PhysRevB.109.045136").unwrap();
        let out = upgrade_http_to_https(input);
        assert_eq!(out.path(), "/PDF/10.1103/PhysRevB.109.045136");
    }

    // ---- Review-pass extensions ------------------------------------

    #[test]
    fn upgrade_http_to_https_skips_dot_localhost_tld() {
        // RFC 6761 reserves the entire `.localhost` TLD for loopback.
        // A developer running `http://myservice.localhost:8080/` MUST
        // NOT see their URL silently upgraded to https.
        for raw in [
            "http://myservice.localhost/",
            "http://api.localhost:8080/x",
            "http://a.b.LOCALHOST/y",
        ] {
            let input = Url::parse(raw).unwrap();
            let out = upgrade_http_to_https(input.clone());
            assert_eq!(out, input, "{raw} MUST NOT be upgraded");
        }
    }

    #[test]
    fn upgrade_http_to_https_skips_ipv4_mapped_ipv6_loopback() {
        // `::ffff:127.0.0.1` is the IPv4-mapped IPv6 form of 127.0.0.1.
        // `Ipv6Addr::is_loopback()` alone returns false for this form,
        // so dual-stack callers binding wiremock to it would be
        // silently upgraded without the `to_ipv4_mapped()` check.
        for raw in [
            "http://[::ffff:127.0.0.1]:9000/x",
            "http://[::ffff:127.0.0.42]/y",
        ] {
            let input = Url::parse(raw).unwrap();
            let out = upgrade_http_to_https(input.clone());
            assert_eq!(out, input, "{raw} MUST NOT be upgraded");
        }
    }

    #[test]
    fn upgrade_http_to_https_is_noop_on_non_http_schemes() {
        // The first guard (`url.scheme() != "http"`) covers everything
        // that isn't http: https (idempotent), file, data, ftp...
        for raw in [
            "https://api.crossref.org/works/10.1234/foo",
            "file:///etc/passwd",
            "data:text/plain,hello",
            "ftp://ftp.example.org/papers/",
        ] {
            let input = Url::parse(raw).unwrap();
            let out = upgrade_http_to_https(input.clone());
            assert_eq!(
                out, input,
                "{raw} non-http scheme MUST be returned unchanged"
            );
        }
    }

    #[test]
    fn upgrade_http_to_https_http_url_always_has_host() {
        // The url crate's parser enforces authority for "special"
        // schemes (`http`, `https`, `ws`, `wss`, `ftp`, `file`).
        // `Url::parse("http:foo")` synthesises a Domain("foo")
        // authority, so an http URL with `host() == None` is
        // unreachable from `Url::parse`. The `None` arm in
        // `upgrade_http_to_https` is defence-in-depth only — pinned
        // here so a future url-crate behavior change is caught.
        let url = Url::parse("http:foo").expect("parse");
        assert!(
            url.host().is_some(),
            "http URLs always carry a host per WHATWG URL spec"
        );
        // The fn still produces a sensible result (upgrade applies).
        let out = upgrade_http_to_https(url.clone());
        assert_eq!(out.scheme(), "https");
    }

    #[test]
    fn upgrade_http_to_https_skips_localhost_case_insensitive() {
        // The literal `localhost` is resolved by the url crate to
        // `127.0.0.1` (Ipv4) at parse time for `http://` URLs, so the
        // Ipv4 arm catches lowercase. The Domain-arm coverage is
        // load-bearing only for the `.localhost` TLD case, but we
        // still pin the casefold semantics in case the url crate
        // changes its parsing rules.
        for raw in ["http://LOCALHOST/", "http://Localhost:8080/x"] {
            let input = Url::parse(raw).unwrap();
            let out = upgrade_http_to_https(input.clone());
            assert_eq!(out, input, "{raw} MUST NOT be upgraded");
        }
    }

    #[test]
    fn is_localhost_domain_matches_literal_and_tld_suffix() {
        assert!(is_localhost_domain("localhost"));
        assert!(is_localhost_domain("LOCALHOST"));
        assert!(is_localhost_domain("api.localhost"));
        assert!(is_localhost_domain("nested.api.localhost"));
        assert!(is_localhost_domain("X.LocalHost"));
        assert!(!is_localhost_domain("localhost.example.org"));
        assert!(!is_localhost_domain("notlocalhost"));
        assert!(!is_localhost_domain(""));
        assert!(!is_localhost_domain(".localhost")); // empty label not valid
    }

    #[test]
    fn is_ipv6_loopback_covers_both_pure_and_mapped() {
        use std::net::Ipv6Addr;
        assert!(is_ipv6_loopback(Ipv6Addr::LOCALHOST)); // ::1
        assert!(is_ipv6_loopback("::ffff:127.0.0.1".parse().unwrap()));
        assert!(is_ipv6_loopback("::ffff:127.0.0.42".parse().unwrap()));
        assert!(!is_ipv6_loopback("::".parse().unwrap()));
        assert!(!is_ipv6_loopback("2001:db8::1".parse().unwrap()));
        // IPv4-mapped non-loopback must NOT be considered loopback.
        assert!(!is_ipv6_loopback("::ffff:1.2.3.4".parse().unwrap()));
    }

    // ---------------------------------------------------------------
    // Allowlist matching — pure unit tests, no network.
    // ---------------------------------------------------------------

    #[test]
    fn tier_1_allowlist_includes_crossref() {
        let lists = tier_1_allowlist();
        let crossref = lists
            .iter()
            .find(|a| a.source == "crossref")
            .expect("crossref entry");
        assert!(
            crossref
                .redirect_hosts
                .iter()
                .any(|h| h.contains("crossref.org")),
            "crossref allowlist must contain a crossref.org pattern; got {:?}",
            crossref.redirect_hosts,
        );
    }

    #[test]
    fn tier_1_allowlist_includes_unpaywall_and_arxiv() {
        let lists = tier_1_allowlist();
        assert!(lists.iter().any(|a| a.source == "unpaywall"));
        assert!(lists.iter().any(|a| a.source == "arxiv"));
    }

    #[test]
    fn fulltext_allowlist_registers_ar5iv_host_under_distinct_key() {
        // ADR-0032 D3: the ar5iv renderer is registered under its own
        // `"ar5iv"` source key (not `"arxiv"`) so provenance distinguishes
        // full-text HTML from the arXiv PDF/Atom API.
        let lists = fulltext_allowlist();
        assert_eq!(lists.len(), 1, "exactly one full-text source entry");
        let ar5iv = &lists[0];
        assert_eq!(ar5iv.source, "ar5iv");
        assert!(ar5iv.matches("ar5iv.labs.arxiv.org"));
        // It is also an arXiv subdomain — the existing `*.arxiv.org` glob
        // already covers the host, so no new registrable domain is added.
        let arxiv = tier_1_allowlist()
            .into_iter()
            .find(|a| a.source == "arxiv")
            .expect("arxiv entry");
        assert!(
            arxiv.matches("ar5iv.labs.arxiv.org"),
            "ar5iv host must fall under the existing *.arxiv.org surface"
        );
    }

    #[test]
    fn oa_publisher_allowlist_groups_under_one_synthetic_source() {
        // The OA-publisher fan-out from Unpaywall's `best_oa_location.url`
        // is keyed under a single synthetic `"oa-publisher"` source so the
        // orchestrator can pass that one source key to
        // `HttpClient::fetch_pdf`. See `docs/REDIRECT_ALLOWLIST.md` §3 (the
        // informed-best-effort note) and the function-level docs in
        // [`oa_publisher_allowlist`].
        let lists = oa_publisher_allowlist();
        assert_eq!(lists.len(), 1, "exactly one synthetic source entry");
        assert_eq!(lists[0].source, "oa-publisher");
    }

    #[test]
    fn oa_publisher_allowlist_matches_known_oa_hosts() {
        let lists = oa_publisher_allowlist();
        let oa = lists
            .iter()
            .find(|a| a.source == "oa-publisher")
            .expect("oa-publisher entry");
        // Spot-check a representative entry per host family.
        assert!(oa.matches("link.springer.com"));
        assert!(oa.matches("nature.com"));
        assert!(oa.matches("onlinelibrary.wiley.com"));
        assert!(oa.matches("www.frontiersin.org"));
        assert!(oa.matches("www.mdpi.com"));
        assert!(oa.matches("journals.plos.org"));
        assert!(oa.matches("www.biorxiv.org"));
        assert!(oa.matches("europepmc.org"));
        assert!(oa.matches("www.ncbi.nlm.nih.gov"));
        assert!(oa.matches("arxiv.org"));
        // #193: physics-society / diamond-OA hosts (empirically observed
        // as Unpaywall best_oa_location targets in the dogfood run).
        assert!(oa.matches("link.aps.org"));
        assert!(oa.matches("journals.aps.org"));
        assert!(oa.matches("scipost.org"));
        assert!(oa.matches("www.scipost.org"));
        assert!(oa.matches("iopscience.iop.org"));
        // Document intent of the `*.<suffix>` form: per
        // `REDIRECT_ALLOWLIST.md` §2.2 rule 3 it matches the bare
        // registrable domain AND any subdomain. Unpaywall has not been
        // observed returning bare-domain PDF URLs for these publishers,
        // but accepting them is consistent with every other `*.` entry in
        // this list (e.g. `arxiv.org` matched by `*.arxiv.org`) and is
        // what the matching rule already implements.
        assert!(oa.matches("aps.org"));
        assert!(oa.matches("iop.org"));
        // Multi-level subdomains also match (e.g. SciPost's deep paths);
        // documents the wildcard scope rather than testing a known URL.
        assert!(oa.matches("submissions.scipost.org"));
        // Negative: an attacker host is not covered.
        assert!(!oa.matches("attacker.test"));
        // Negative: dot-boundary safety for the new entries — a different
        // suffix that merely ends with the registrable name must NOT match.
        assert!(!oa.matches("notaps.org"));
        assert!(!oa.matches("evilscipost.org"));
        assert!(!oa.matches("notiop.org"));
        // Negative: dot-boundary safety — `*.springer.com` must not match
        // `notspringer.com`.
        assert!(!oa.matches("notspringer.com"));
    }

    #[test]
    fn allowlist_matches_exact_fqdn() {
        let a = SourceAllowlist::new("crossref", vec!["api.crossref.org".to_string()]);
        assert!(a.matches("api.crossref.org"));
        assert!(!a.matches("crossref.org"));
        assert!(!a.matches("xapi.crossref.org"));
    }

    #[test]
    fn allowlist_matches_subdomain_glob() {
        // Per docs/REDIRECT_ALLOWLIST.md §2.2 rule 3: `*.<suffix>`
        // matches both `<suffix>` itself AND any `*.<suffix>` subdomain,
        // but never matches a different suffix that happens to end with
        // `<suffix>` without a dot boundary.
        let a = SourceAllowlist::new("crossref", vec!["*.crossref.org".to_string()]);
        assert!(a.matches("doi.crossref.org"));
        assert!(a.matches("crossref.org"));
        assert!(!a.matches("notcrossref.org"));
        assert!(!a.matches("crossref.org.attacker.test"));
    }

    #[test]
    fn allowlist_matches_is_case_insensitive() {
        let a = SourceAllowlist::new("crossref", vec!["API.crossref.ORG".to_string()]);
        assert!(a.matches("api.crossref.org"));
        assert!(a.matches("API.CROSSREF.ORG"));
    }

    #[test]
    fn allowlist_with_no_redirect_hosts_matches_nothing() {
        // §2.2 rule 5: an empty `redirect_hosts` means "no redirects
        // permitted from this source".
        let a = SourceAllowlist::new("ghost", Vec::<String>::new());
        assert!(!a.matches("anything.test"));
        assert!(!a.matches(""));
    }

    // ---------------------------------------------------------------
    // PDF magic-byte handling — tests on the body-parsing path. We
    // exercise the magic-byte branch via the public API against a
    // wiremock server so the assertion runs through the full
    // streaming codepath.
    // ---------------------------------------------------------------

    /// Build a test-only `HttpClient` against an `http://` wiremock
    /// origin.
    ///
    /// Slice 5 (PR #84 advisory item A4 refactor): this helper now
    /// delegates to the public
    /// [`HttpClient::new_for_tests_allow_http`] constructor (defined
    /// just above the test module) instead of re-implementing the
    /// redirect-policy + `https_only(false)` builder. The two
    /// implementations had drifted into duplicates — keeping a private
    /// re-implementation only meant a future security tweak to the
    /// builder would silently leave the tests on a stale path.
    fn build_test_client_for_http(source: &str, allowlist_host: &str) -> HttpClient {
        HttpClient::new_for_tests_allow_http(source, allowlist_host)
    }

    #[tokio::test]
    async fn pdf_magic_byte_match_succeeds() {
        let server = MockServer::start().await;
        let body = b"%PDF-1.7\n...some pdf bytes...".to_vec();
        Mock::given(method("GET"))
            .and(path("/paper.pdf"))
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
        let client = build_test_client_for_http("crossref", &host);
        let url: Url = format!("{}/paper.pdf", server.uri()).parse().unwrap();
        let (got_body, _final_url) = client.fetch_pdf("crossref", url).await.expect("ok");
        assert_eq!(&got_body[..], &body[..]);
    }

    #[tokio::test]
    async fn pdf_magic_byte_mismatch_rejects() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/not_a_pdf"))
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
        let client = build_test_client_for_http("crossref", &host);
        let url: Url = format!("{}/not_a_pdf", server.uri()).parse().unwrap();
        let err = client
            .fetch_pdf("crossref", url)
            .await
            .expect_err("not pdf");
        match err {
            HttpError::NotAPdf { got } => {
                assert_eq!(&got, b"<html");
            }
            other => panic!("expected NotAPdf, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn fetch_bytes_does_not_check_pdf_magic() {
        // The non-PDF path returns the body unchanged regardless of
        // magic bytes. This pins the boundary between the JSON/text
        // path and the PDF path.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/data.json"))
            .respond_with(
                ResponseTemplate::new(200).set_body_bytes(br#"{"hello":"world"}"#.to_vec()),
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
        let client = build_test_client_for_http("crossref", &host);
        let url: Url = format!("{}/data.json", server.uri()).parse().unwrap();
        let (body, _final_url) = client.fetch_bytes("crossref", url).await.expect("ok");
        assert_eq!(&body[..], br#"{"hello":"world"}"#);
    }

    #[tokio::test]
    async fn oversized_body_via_content_length_short_circuits() {
        // Wiremock can advertise a `Content-Length` larger than the body
        // it actually serves; hyper accepts the mismatch and our
        // fast-path check fires before any body bytes are consumed.
        let server = MockServer::start().await;
        let oversized = PDF_MAX_BYTES + 1;
        Mock::given(method("GET"))
            .and(path("/huge"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-length", oversized.to_string().as_str())
                    .set_body_bytes(b"%PDF-".to_vec()),
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
        let client = build_test_client_for_http("crossref", &host);
        let url: Url = format!("{}/huge", server.uri()).parse().unwrap();
        let err = client
            .fetch_bytes("crossref", url)
            .await
            .expect_err("should reject");
        match err {
            HttpError::OversizedBody { actual, cap } => {
                assert!(actual > cap, "actual {} should exceed cap {}", actual, cap);
                assert_eq!(cap, PDF_MAX_BYTES);
            }
            // The mismatched Content-Length may also trip an underlying
            // transport error before our fast-path runs. Either outcome
            // satisfies the security goal (the transfer was aborted
            // without buffering 100 GB), so accept Network here as a
            // wiremock idiosyncrasy rather than a contract relaxation.
            HttpError::Network(_) => {}
            other => panic!("expected OversizedBody or Network, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn unknown_source_rejected() {
        let client = HttpClient::new(tier_1_allowlist()).expect("client builds");
        let url: Url = "https://api.crossref.org/works/10.1234/x".parse().unwrap();
        let err = client
            .fetch_bytes("not-a-source", url)
            .await
            .expect_err("unknown source");
        match err {
            HttpError::UnknownSource { source_key } => {
                assert_eq!(source_key, "not-a-source")
            }
            other => panic!("expected UnknownSource, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn http_status_error_surfaces() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/missing"))
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
        let client = build_test_client_for_http("crossref", &host);
        let url: Url = format!("{}/missing", server.uri()).parse().unwrap();
        let err = client.fetch_bytes("crossref", url).await.expect_err("404");
        match err {
            HttpError::HttpStatus { status, .. } => assert_eq!(status, 404),
            other => panic!("expected HttpStatus, got {:?}", other),
        }
    }

    // ---------------------------------------------------------------
    // Redirect policy tests — drive the closure via wiremock 30x
    // responses pointing at insecure / off-allowlist targets. With
    // `https_only(true)` on the production builder the request never
    // leaves the initial leg — we run these against the test builder
    // (which relaxes `https_only` for the *initial* leg only) so the
    // redirect closure is reached and exercised.
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn redirect_to_http_is_rejected_by_closure() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/redir"))
            .respond_with(
                ResponseTemplate::new(302).insert_header("location", "http://attacker.test/file"),
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
        let client = build_test_client_for_http("crossref", &host);
        let url: Url = format!("{}/redir", server.uri()).parse().unwrap();
        let err = client
            .fetch_bytes("crossref", url)
            .await
            .expect_err("redirect to http rejected");
        match err {
            HttpError::Network(e) => {
                let msg = format!("{:?}", e);
                assert!(
                    msg.contains("InsecureRedirect") || msg.contains("non-HTTPS"),
                    "expected insecure-redirect signal in error chain, got {}",
                    msg
                );
            }
            other => panic!("expected Network(InsecureRedirect), got {:?}", other),
        }
    }

    #[tokio::test]
    async fn redirect_outside_allowlist_is_rejected_by_closure() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/redir"))
            .respond_with(
                ResponseTemplate::new(302).insert_header("location", "https://attacker.test/file"),
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
        let client = build_test_client_for_http("crossref", &host);
        let url: Url = format!("{}/redir", server.uri()).parse().unwrap();
        let err = client
            .fetch_bytes("crossref", url)
            .await
            .expect_err("redirect to attacker rejected");
        match err {
            HttpError::Network(e) => {
                let msg = format!("{:?}", e);
                assert!(
                    msg.contains("RedirectDenied") || msg.contains("not in allowlist"),
                    "expected redirect-denied signal in error chain, got {}",
                    msg
                );
            }
            other => panic!("expected Network(RedirectDenied), got {:?}", other),
        }
    }

    #[tokio::test]
    async fn redirect_to_allowlisted_https_host_is_followed_by_closure() {
        // 302 to an https host that IS in the allowlist. The redirect
        // dispatch will fail (DNS won't resolve `mirror.allowed.test`)
        // but the closure must NOT short-circuit — failure mode is a
        // transport error, not InsecureRedirect / RedirectDenied.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/redir"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", "https://mirror.allowed.test/file"),
            )
            .mount(&server)
            .await;
        let initial_host = server
            .uri()
            .parse::<Url>()
            .unwrap()
            .host_str()
            .unwrap()
            .to_string();
        // Allow the initial host AND the redirect target host.
        let allowlist = SourceAllowlist::new(
            "crossref",
            vec![initial_host.clone(), "*.allowed.test".to_string()],
        );
        let allowlist_for_closure = allowlist.clone();
        let policy = Policy::custom(move |attempt| {
            let scheme = attempt.url().scheme().to_string();
            let host_opt = attempt.url().host_str().map(|h| h.to_ascii_lowercase());
            if scheme != "https" {
                return attempt.error(HttpError::InsecureRedirect { scheme });
            }
            let h = match host_opt {
                Some(h) => h,
                None => {
                    return attempt.error(HttpError::RedirectDenied {
                        source_key: allowlist_for_closure.source.clone(),
                        host: String::new(),
                        expected_hosts: allowlist_for_closure.redirect_hosts.clone(),
                    });
                }
            };
            if !allowlist_for_closure.matches(&h) {
                return attempt.error(HttpError::RedirectDenied {
                    source_key: allowlist_for_closure.source.clone(),
                    host: h,
                    expected_hosts: allowlist_for_closure.redirect_hosts.clone(),
                });
            }
            attempt.follow()
        });
        ensure_crypto_provider();
        let raw_client = ClientBuilder::new()
            .https_only(false)
            .redirect(policy)
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(Duration::from_secs(5))
            .user_agent("doiget/test")
            .tls_backend_rustls()
            .build()
            .expect("client builds");
        let url: Url = format!("{}/redir", server.uri()).parse().unwrap();
        let err = raw_client.get(url).send().await.expect_err("DNS fails");
        // The error should NOT carry our InsecureRedirect / RedirectDenied
        // marker — the closure approved the redirect.
        let msg = format!("{:?}", err);
        assert!(
            !msg.contains("RedirectDenied") && !msg.contains("InsecureRedirect"),
            "closure short-circuited an allowed redirect: {}",
            msg,
        );
    }

    #[test]
    fn http_client_clone_is_cheap() {
        // Sanity: cloning shares the inner Arc<HashMap<...>>.
        let a = HttpClient::new(tier_1_allowlist()).expect("builds");
        let b = a.clone();
        assert_eq!(a.clients.len(), b.clients.len());
        assert!(Arc::ptr_eq(&a.clients, &b.clients));
    }

    // ---------------------------------------------------------------
    // HttpError -> Option<DenialContext>  (ADR-0023 §4 mapping)
    // ---------------------------------------------------------------

    #[test]
    fn denial_from_redirect_denied_carries_attempted_and_expected() {
        use crate::{DenialContext, DenialReason};
        let e = HttpError::RedirectDenied {
            source_key: "crossref".to_string(),
            host: "evil.example.com".to_string(),
            expected_hosts: vec!["api.crossref.org".to_string(), "*.crossref.org".to_string()],
        };
        let dc: Option<DenialContext> = (&e).into();
        let dc = dc.expect("RedirectDenied -> Some(DenialContext)");
        assert_eq!(dc.reason, DenialReason::RedirectNotInAllowlist);
        assert_eq!(dc.source.as_deref(), Some("crossref"));
        assert_eq!(dc.attempted.as_deref(), Some("evil.example.com"));
        assert_eq!(
            dc.expected.as_deref(),
            Some(&["api.crossref.org".to_string(), "*.crossref.org".to_string()][..])
        );
        assert!(dc.cap.is_none());
        assert!(dc.actual.is_none());
        assert!(dc.hop_index.is_none());
    }

    #[test]
    fn denial_from_oversized_body_carries_cap_and_actual() {
        use crate::{DenialContext, DenialReason};
        let e = HttpError::OversizedBody {
            actual: 209_715_200,
            cap: PDF_MAX_BYTES,
        };
        let dc: Option<DenialContext> = (&e).into();
        let dc = dc.expect("OversizedBody -> Some(DenialContext)");
        assert_eq!(dc.reason, DenialReason::SizeCapExceeded);
        assert_eq!(dc.cap, Some(PDF_MAX_BYTES));
        assert_eq!(dc.actual, Some(209_715_200));
        assert!(dc.source.is_none());
        assert!(dc.attempted.is_none());
        // OversizedBody has no allowlist channel: producer leaves
        // `expected` at `None` (NOT `Some(vec![])`). See the field doc on
        // `DenialContext::expected` for the disambiguation.
        assert!(dc.expected.is_none());
    }

    #[test]
    fn denial_from_not_a_pdf_hex_encodes_got_bytes() {
        use crate::{DenialContext, DenialReason};
        // First 5 bytes of "<html" — what the magic-byte check sees when
        // a publisher returns an HTML interstitial instead of a PDF.
        let e = HttpError::NotAPdf {
            got: [0x3c, 0x68, 0x74, 0x6d, 0x6c],
        };
        let dc: Option<DenialContext> = (&e).into();
        let dc = dc.expect("NotAPdf -> Some(DenialContext)");
        assert_eq!(dc.reason, DenialReason::ContentTypeMismatch);
        assert_eq!(dc.attempted.as_deref(), Some("3c68746d6c"));
        assert_eq!(dc.expected.as_deref(), Some(&["%PDF-".to_string()][..]));
    }

    #[test]
    fn denial_from_insecure_redirect_marks_insecure_scheme() {
        use crate::{DenialContext, DenialReason};
        let e = HttpError::InsecureRedirect {
            scheme: "http".to_string(),
        };
        let dc: Option<DenialContext> = (&e).into();
        let dc = dc.expect("InsecureRedirect -> Some(DenialContext)");
        // ADR-0023 §4 (post-incorporation review): InsecureRedirect maps
        // to its own dedicated `InsecureScheme` reason, not the host-
        // allowlist reason — they are semantically distinct denials.
        assert_eq!(dc.reason, DenialReason::InsecureScheme);
        assert_eq!(dc.attempted.as_deref(), Some("http:..."));
        assert_eq!(dc.expected.as_deref(), Some(&["https".to_string()][..]));
    }

    #[test]
    fn denial_from_non_denial_variants_returns_none() {
        use crate::DenialContext;
        // Network / HttpStatus / UnknownSource are not denials; they
        // map to None per ADR-0023 §4.
        let e = HttpError::HttpStatus {
            status: 503,
            url: "https://api.crossref.org/works/x".to_string(),
        };
        let dc: Option<DenialContext> = (&e).into();
        assert!(dc.is_none(), "HttpStatus must not produce a DenialContext");

        let e = HttpError::UnknownSource {
            source_key: "ghost".to_string(),
        };
        let dc: Option<DenialContext> = (&e).into();
        assert!(
            dc.is_none(),
            "UnknownSource must not produce a DenialContext"
        );
    }

    // ---------------------------------------------------------------
    // Issue #117 — transient retry / backoff. Real time: wiremock
    // serves over real localhost IO and tokio `start_paused` is
    // incompatible with that (it auto-advances past reqwest's
    // timeout). Backoff is small enough that the slowest case
    // (persistent 503, 3 retries ≈ 3.5s) stays within the suite budget.
    // ---------------------------------------------------------------

    fn host_of(server: &MockServer) -> String {
        server
            .uri()
            .parse::<Url>()
            .unwrap()
            .host_str()
            .unwrap()
            .to_string()
    }

    #[tokio::test]
    async fn transient_503_then_200_succeeds() {
        let server = MockServer::start().await;
        // Catch-all 200 mounted first (lowest precedence); the
        // single-shot 503 mounted last takes precedence for the first
        // request only, then falls through to the 200.
        Mock::given(method("GET"))
            .and(path("/p"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"ok":1}"#))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/p"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .mount(&server)
            .await;

        let client = build_test_client_for_http("crossref", &host_of(&server));
        let url: Url = format!("{}/p", server.uri()).parse().unwrap();
        let (body, _) = client
            .fetch_bytes("crossref", url)
            .await
            .expect("503-then-200 must succeed after one retry");
        assert_eq!(&body[..], br#"{"ok":1}"#);
    }

    #[tokio::test]
    async fn persistent_503_exhausts_and_returns_httpstatus() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/p"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let client = build_test_client_for_http("crossref", &host_of(&server));
        let url: Url = format!("{}/p", server.uri()).parse().unwrap();
        let err = client
            .fetch_bytes("crossref", url)
            .await
            .expect_err("persistent 503 must exhaust retries");
        match err {
            HttpError::HttpStatus { status, .. } => assert_eq!(status, 503),
            other => panic!("expected HttpStatus 503, got {other:?}"),
        }
        // First attempt + MAX_FETCH_RETRIES retries.
        let reqs = server
            .received_requests()
            .await
            .expect("wiremock records requests");
        assert_eq!(reqs.len(), (MAX_FETCH_RETRIES + 1) as usize);
    }

    #[tokio::test]
    async fn retry_after_429_then_200_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/p"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/p"))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "1"))
            .up_to_n_times(1)
            .mount(&server)
            .await;

        let client = build_test_client_for_http("crossref", &host_of(&server));
        let url: Url = format!("{}/p", server.uri()).parse().unwrap();
        let (body, _) = client
            .fetch_bytes("crossref", url)
            .await
            .expect("429+Retry-After then 200 must succeed");
        assert_eq!(&body[..], b"ok");
    }

    #[tokio::test]
    async fn permanent_404_is_not_retried() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/p"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let client = build_test_client_for_http("crossref", &host_of(&server));
        let url: Url = format!("{}/p", server.uri()).parse().unwrap();
        let _ = client
            .fetch_bytes("crossref", url)
            .await
            .expect_err("404 must fail");
        let reqs = server
            .received_requests()
            .await
            .expect("wiremock records requests");
        assert_eq!(reqs.len(), 1, "4xx (non-408/429) must NOT be retried");
    }
}
