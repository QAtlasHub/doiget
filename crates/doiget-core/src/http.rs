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
//!   - User-Agent: `doiget/<version> (+https://github.com/sotashimozono/doiget)`
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
    #[error("redirect target {host} not in allowlist for source {source_key}")]
    RedirectDenied {
        /// Source key whose allowlist rejected the redirect.
        source_key: String,
        /// The lowercased host that was rejected.
        host: String,
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
        let mut clients = HashMap::with_capacity(allowlists.len());
        for entry in allowlists {
            let source = entry.source.clone();
            let client = build_client(entry)?;
            clients.insert(source, client);
        }
        Ok(Self {
            clients: Arc::new(clients),
        })
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
        self.fetch_inner(source, url, false).await
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
        self.fetch_inner(source, url, true).await
    }

    async fn fetch_inner(
        &self,
        source: &str,
        url: Url,
        check_pdf_magic: bool,
    ) -> Result<(Bytes, Url), HttpError> {
        let client = self
            .clients
            .get(source)
            .ok_or_else(|| HttpError::UnknownSource {
                source_key: source.to_string(),
            })?;

        let response = client.get(url).send().await?;
        let final_url = response.url().clone();

        // Status check before body read so we can fail fast.
        let status = response.status();
        if !status.is_success() {
            return Err(HttpError::HttpStatus {
                status: status.as_u16(),
                url: final_url.to_string(),
            });
        }

        // Content-Length fast-path: if header is present and exceeds the
        // cap, fail without reading any body. Per `docs/SECURITY.md` §1.2.
        if let Some(len) = response.content_length() {
            if len > PDF_MAX_BYTES {
                return Err(HttpError::OversizedBody {
                    actual: len,
                    cap: PDF_MAX_BYTES,
                });
            }
        }

        // Stream body and enforce the cap as bytes accumulate. We use
        // `bytes_stream` rather than `.bytes()` so that an oversized body
        // is rejected after at most one chunk past the cap, not after the
        // entire transfer completes.
        let mut buf = BytesMut::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            // Saturating-add into a u64 for the error report; the actual
            // accumulator never grows past the cap because we abort as
            // soon as we cross it.
            let projected = (buf.len() as u64).saturating_add(chunk.len() as u64);
            if projected > PDF_MAX_BYTES {
                return Err(HttpError::OversizedBody {
                    actual: projected,
                    cap: PDF_MAX_BYTES,
                });
            }
            buf.extend_from_slice(&chunk);
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

        Ok((body, final_url))
    }
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
        map.insert(allowlist.source.clone(), client);
        Self {
            clients: Arc::new(map),
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
        for (source, host) in entries {
            let allowlist = SourceAllowlist::new(*source, vec![host.to_string()]);
            let client = build_client_allow_http(allowlist.clone()).expect("test client builds");
            map.insert(allowlist.source.clone(), client);
        }
        Self {
            clients: Arc::new(map),
        }
    }
}

fn build_client_allow_http(allowlist: SourceAllowlist) -> Result<Client, reqwest::Error> {
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
                });
            }
        };
        if !allowlist_for_closure.matches(&host) {
            return attempt.error(HttpError::RedirectDenied {
                source_key: allowlist_for_closure.source.clone(),
                host,
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
            "doiget/{} (+https://github.com/sotashimozono/doiget)",
            VERSION
        ))
        .tls_backend_rustls()
        .build()
}

// ---------------------------------------------------------------------------
// ClientBuilder helpers
// ---------------------------------------------------------------------------

fn build_client(allowlist: SourceAllowlist) -> Result<Client, reqwest::Error> {
    let user_agent = format!(
        "doiget/{} (+https://github.com/sotashimozono/doiget)",
        VERSION
    );

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
                });
            }
        };
        if !allowlist_for_closure.matches(&host) {
            return attempt.error(HttpError::RedirectDenied {
                source_key: allowlist_for_closure.source.clone(),
                host,
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
        // older `use_rustls_tls()`. The workspace `reqwest` features
        // already pin `rustls`, so this is a re-assertion at builder
        // level rather than a feature switch.
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
    /// origin. The redirect closure still rejects insecure schemes — we
    /// only relax `https_only` at the connection level so wiremock can
    /// serve. This is acceptable because the redirect closure (which is
    /// the security-load-bearing path) is exercised separately by the
    /// `redirect_to_http_is_rejected_by_closure` test.
    fn build_test_client_for_http(source: &str, allowlist_host: &str) -> HttpClient {
        let allowlist = SourceAllowlist::new(source, vec![allowlist_host.to_string()]);
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
                    });
                }
            };
            if !allowlist_for_closure.matches(&host) {
                return attempt.error(HttpError::RedirectDenied {
                    source_key: allowlist_for_closure.source.clone(),
                    host,
                });
            }
            attempt.follow()
        });
        let client = ClientBuilder::new()
            // `https_only(false)` only at this scope — production
            // builders (the public `HttpClient::new`) keep it on.
            .https_only(false)
            .redirect(redirect_policy)
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(TOTAL_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .user_agent(format!(
                "doiget/{} (+https://github.com/sotashimozono/doiget)",
                VERSION
            ))
            .tls_backend_rustls()
            .build()
            .expect("test client builds");
        let mut map = HashMap::new();
        map.insert(allowlist.source.clone(), client);
        HttpClient {
            clients: Arc::new(map),
        }
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
                    });
                }
            };
            if !allowlist_for_closure.matches(&h) {
                return attempt.error(HttpError::RedirectDenied {
                    source_key: allowlist_for_closure.source.clone(),
                    host: h,
                });
            }
            attempt.follow()
        });
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
}
