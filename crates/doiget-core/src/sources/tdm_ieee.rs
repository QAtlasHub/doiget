//! IEEE Xplore TDM source — DOI metadata via the
//! `/api/v1/search/articles` endpoint (Tier 3).
//!
//! Spec: `docs/SOURCES.md` §1 Tier 3 row + §4 "TDM sources (Phase 5)",
//! `docs/CAPABILITY.md` §2, ADR-0002 (per-publisher Cargo features),
//! ADR-0019 (eight safeguards), ADR-0039 (per-publisher TDM credentials
//! are the supported route for IEEE / ACM / SIAM / AMS — their web hosts
//! answer a scripted client with `202` + an empty body regardless of
//! entitlement, so `oa-publisher` cannot reach them).
//!
//! Whole module gated by `#[cfg(feature = "tdm-ieee")]` so default
//! release binaries never include the host pattern or the env-var read
//! path (ADR-0002).
//!
//! ## The contract here is INFERRED, not confirmed against a live key
//!
//! Issue #430 is explicit that IEEE's programme contract — endpoint,
//! auth shape, response shape, rate limits, terms — could not be
//! obtained from outside the programme. What is implemented below is
//! the shape IEEE's own developer portal and its published SDKs
//! describe:
//!
//! - base `https://ieeexploreapi.ieee.org`, path `/api/v1/search/articles`
//! - the key as an `apikey` **query parameter** (not a header)
//! - a JSON envelope `{ total_records, total_searched, articles: [...] }`
//!
//! ### Observed on 2026-08-24 (#460), without a key
//!
//! One unauthenticated request confirmed the first bullet and corrected
//! the third's failure path:
//!
//! ```text
//! GET /api/v1/search/articles?doi=...&format=json
//! HTTP=403  content-type=text/xml
//! <h1>Developer Inactive</h1>
//! ```
//!
//! So the **base and path are right** — the host resolves and the
//! endpoint is served. But the failure body is **not JSON**, and
//! `format=json` does not make it so. That also means a 403 is an
//! `HttpError::HttpStatus` and never reaches the JSON parsing below:
//! the branch a key-less or not-yet-activated user hits is the HTTP
//! one, which is why
//! `an_inactive_key_403_says_so_and_does_not_leak_the_key` exists.
//!
//! Still unverified: the 200-response envelope and the rate limits.
//!
//! Everything that could be wrong is wrong *loudly*: a response that is
//! not that shape becomes [`FetchError::SourceSchema`] naming what was
//! missing and quoting the body, so the first run against a real key
//! reports the actual shape rather than silently returning nothing.
//! `DOIGET_IEEE_BASE` exists so that run can be replayed against a
//! recorded fixture without touching production.
//!
//! Do not promote the `docs/SOURCES.md` row out of its "unverified"
//! marking until a **200** with a real key has been observed.
//!
//! ## Three-gate activation
//!
//! Per `docs/CAPABILITY.md` §2 a fetch only succeeds when ALL THREE
//! gates pass:
//!
//! 1. The binary was built with `--features tdm-ieee`.
//! 2. The user set `DOIGET_KEY_IEEE=<api-key>`.
//! 3. The user set `DOIGET_AGREE_TDM_IEEE=1`.
//!
//! Gates 2 + 3 are checked at startup by
//! [`CapabilityProfile::from_env`] and surface as
//! `profile.tdm_ieee = Some(TdmGrant)`. This source mirrors that check
//! in [`can_serve`](TdmIeeeSource::can_serve) and again in
//! [`fetch`](TdmIeeeSource::fetch) (defensive). The key is read once at
//! startup and carried in [`TdmGrant::api_key`](crate::TdmGrant) (issue
//! #153); this source consumes it from the grant and never re-reads
//! `DOIGET_KEY_IEEE` at fetch time.
//!
//! ## Credential hygiene — key in the URL (issue #146)
//!
//! IEEE is the second query-param-auth source after Springer Nature:
//! the documented parameter is `apikey` (one word — Springer's is
//! `api_key`), and no header-auth path is documented for either the
//! metadata or the full-text endpoint. The #146 preference for a header
//! is therefore not available against this upstream.
//!
//! Same residual-risk mitigation as Springer: every URL leaving this
//! module — [`FetchResult::final_url`] and any error string — goes
//! through `redact_apikey_in_url` first, and `http.rs` redacts the same
//! parameter out of `HttpError::HttpStatus`. The key still appears on
//! the wire and in IEEE's own server-side logs; that is inherent to
//! query-param auth and is accepted residual risk, recorded in
//! `docs/CAPABILITY.md` §1.
//!
//! ## Metadata-only contract
//!
//! `FetchResult.pdf_bytes` is always `None`, matching the three
//! existing Tier-3 sources. IEEE does document a separate full-text
//! endpoint, but retrieving it requires the eight ADR-0019 safeguards
//! wired through the orchestrator — and, unlike the shape below, its
//! contract cannot be inferred at all from outside the programme.

#![cfg(feature = "tdm-ieee")]

use async_trait::async_trait;
use secrecy::ExposeSecret;
use url::Url;

use crate::provenance::{Capability, LogEvent, LogResult, RowInput};
use crate::source::{FetchContext, FetchError, FetchResult, Source};
use crate::{CapabilityProfile, Ref};

/// Production IEEE Xplore API base URL.
const DEFAULT_BASE: &str = "https://ieeexploreapi.ieee.org";

/// DOI registrant prefixes IEEE actually owns.
///
/// #442: without this the source would answer `can_serve` for ANY DOI,
/// so wiring it into the chain would send every lookup — including DOIs
/// belonging to other publishers — to IEEE. That is a politeness
/// problem (`docs/SOURCES.md` §6) and a privacy one
/// (`docs/PRIVACY.md`): it would disclose the user's whole reading list
/// to a publisher who has no part in it.
///
/// Scoped to the DOIs this publisher registered, the disclosure is
/// nil — resolving such a DOI goes through them anyway.
///
/// Verified against the Crossref registrant registry
/// (`api.crossref.org/prefixes/<prefix>`) on 2026-08-24; both resolve to
/// "Institute of Electrical and Electronics Engineers (IEEE)".
/// `10.1109` is the journal / transactions / Access family, `10.23919`
/// the conference proceedings IEEE co-publishes.
///
/// Deliberately conservative: a publisher may own prefixes not listed
/// here. A miss is not silent — it is recorded in the attempt trace as
/// `not consulted (DOI prefix ... is not ...)`, so it is diagnosable
/// from the error message rather than looking like a lookup failure.
pub(crate) const PUBLISHER_PREFIXES: &[&str] = &["10.1109", "10.23919"];

/// The query-parameter name IEEE expects, and the value it is replaced
/// with in any URL that leaves this module bound for a log or
/// recorded-provenance sink (issue #146).
///
/// One word, unlike Springer's `api_key`. `http.rs::redact_api_key_query`
/// knows both spellings; a third source with a third spelling must be
/// added there too or its key reaches `HttpError::HttpStatus`.
const API_KEY_PARAM: &str = "apikey";
const REDACTED: &str = "REDACTED";

/// IEEE Xplore TDM [`Source`] impl — DOI → first matching `articles[]`
/// entry from `/api/v1/search/articles`.
#[derive(Clone, Debug)]
pub struct TdmIeeeSource {
    /// API base URL. Production pins `https://ieeexploreapi.ieee.org`;
    /// [`with_base`](Self::with_base) lets wiremock substitute an
    /// `http://127.0.0.1:N` origin — and lets the inferred response
    /// shape above be re-tested against a recorded real response.
    base: Url,
}

impl TdmIeeeSource {
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

    /// Build the `/api/v1/search/articles?doi=<doi>&format=json&apikey=<key>`
    /// URL.
    ///
    /// `doi` is a first-class search parameter on the Metadata Search
    /// API, so no Lucene-style filter string is needed. `format=json`
    /// is sent explicitly rather than relying on the documented JSON
    /// default: this module only knows how to parse JSON, and an
    /// upstream default flip would otherwise surface as a parse error
    /// instead of a request we never made correctly.
    fn request_url(&self, doi: &crate::Doi, api_key: &str) -> Result<Url, FetchError> {
        let mut url =
            self.base
                .join("/api/v1/search/articles")
                .map_err(|e| FetchError::SourceSchema {
                    hint: format!("tdm-ieee URL construction failed: {e}"),
                })?;
        url.query_pairs_mut()
            .append_pair("doi", doi.as_str())
            .append_pair("format", "json")
            .append_pair(API_KEY_PARAM, api_key);
        Ok(url)
    }
}

/// Return a copy of `url` with the `apikey` query-parameter value
/// replaced by `REDACTED`, preserving every other pair and ordering.
///
/// IEEE only documents query-param auth (issue #146), so the key
/// unavoidably appears on the wire and in IEEE's own logs. This keeps it
/// out of *our* sinks: the `FetchResult::final_url` the orchestrator
/// persists into the metadata TOML `url` field, and any error string
/// this module produces. If no `apikey` pair is present the URL is
/// returned structurally unchanged.
fn redact_apikey_in_url(url: &Url) -> Url {
    if url.query_pairs().all(|(k, _)| k != API_KEY_PARAM) {
        return url.clone();
    }
    let mut redacted = url.clone();
    // Re-serialize the pairs, swapping only the apikey value. `clear()`
    // first so we don't append onto the existing query string.
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| {
            if k == API_KEY_PARAM {
                (k.into_owned(), REDACTED.to_string())
            } else {
                (k.into_owned(), v.into_owned())
            }
        })
        .collect();
    redacted.query_pairs_mut().clear().extend_pairs(pairs);
    redacted
}

impl Default for TdmIeeeSource {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Source for TdmIeeeSource {
    fn name(&self) -> &str {
        "tdm-ieee"
    }

    fn can_serve(&self, profile: &CapabilityProfile, ref_: &Ref) -> bool {
        let Ref::Doi(doi) = ref_ else {
            return false;
        };
        // Both halves matter: the grant is the user's opt-in, the prefix
        // keeps that opt-in from leaking unrelated DOIs to the publisher
        // (#442). The orchestrator checks the same two conditions so it
        // can tell them apart in the trace; this is the defensive mirror.
        profile.tdm_ieee.is_some() && PUBLISHER_PREFIXES.contains(&doi.prefix())
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
                    source_key: "tdm-ieee".into(),
                });
            }
        };

        // Defensive gate (1/3 + 2/3): the runtime grant must be
        // populated and carries the key validated at startup (issue
        // #153). `CapabilityProfile` is immutable for the process
        // lifetime (`docs/CAPABILITY.md` §6), so the startup grant is
        // the single source of truth — no env re-read.
        let grant = profile
            .tdm_ieee
            .as_ref()
            .ok_or_else(|| FetchError::NotEligible {
                source_key: "tdm-ieee".into(),
            })?;
        let api_key = grant.api_key.expose_secret();
        if api_key.is_empty() {
            return Err(FetchError::NotEligible {
                source_key: "tdm-ieee".into(),
            });
        }

        let _permit = ctx.rate_limiter.acquire(self.name()).await;

        let url = self.request_url(doi, api_key)?;
        let (body, final_url) = ctx.http.fetch_bytes(self.name(), url).await?;
        // Issue #146: IEEE auth is query-param only; strip the key from
        // the URL before it can reach the metadata TOML / any log.
        let final_url = redact_apikey_in_url(&final_url);

        let envelope: serde_json::Value =
            serde_json::from_slice(&body).map_err(|e| FetchError::SourceSchema {
                hint: format!("tdm-ieee returned non-JSON: {e}"),
            })?;

        // Inferred envelope: { total_records, total_searched, articles: [...] }.
        // The hint quotes the body because this shape is the part of the
        // contract that could not be confirmed from outside the
        // programme (#430) — the first run against a real key has to be
        // able to report what actually arrived.
        let articles = envelope
            .get("articles")
            .and_then(|a| a.as_array())
            .ok_or_else(|| FetchError::SourceSchema {
                hint: format!(
                    "tdm-ieee response missing `articles` array (got: {})",
                    truncate_for_hint(&body)
                ),
            })?;
        let first = articles.first().ok_or_else(|| FetchError::SourceSchema {
            hint: "tdm-ieee returned 0 articles for this DOI".to_string(),
        })?;

        let canonical = ref_.promote(self.name(), None).digest_hex();
        ctx.log.append(RowInput {
            event: LogEvent::Fetch,
            result: LogResult::Ok,
            capability: Capability::TdmIeee,
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
        "total_records": 1,
        "total_searched": 6000000,
        "articles": [
            {
                "doi": "10.1109/TSP.2018.2812747",
                "title": "Example IEEE Transactions Article",
                "publication_title": "IEEE Transactions on Signal Processing",
                "article_number": "8307462"
            }
        ]
    }"#;

    const SAMPLE_ENVELOPE_EMPTY: &str = r#"{
        "total_records": 0,
        "total_searched": 6000000,
        "articles": []
    }"#;

    /// A 200 whose body is not the inferred envelope. Kept generic — the
    /// point is that an unrecognised *successful* response must be a
    /// schema error that quotes what arrived, not a silent miss.
    const SAMPLE_UNEXPECTED_SHAPE: &str = r#"{"totalfound": 1, "records": []}"#;

    /// The real body IEEE serves an unauthorised caller, observed on
    /// 2026-08-24 (#460):
    ///
    /// ```text
    /// GET /api/v1/search/articles?doi=...&format=json
    /// HTTP=403  content-type=text/xml
    /// <h1>Developer Inactive</h1>
    /// ```
    ///
    /// Note it is NOT JSON, and `format=json` does not change that — the
    /// fixture this replaced was a JSON guess at it. It is also the
    /// single most likely first-contact outcome, because a key pending
    /// programme activation reads exactly like no key at all.
    const SAMPLE_INACTIVE_KEY_BODY: &str = "<h1>Developer Inactive</h1>";

    fn build_test_context(wiremock_host: &str) -> (TempDir, FetchContext) {
        let td = TempDir::new().expect("tempdir");
        let log_dir =
            Utf8PathBuf::try_from(td.path().to_path_buf()).expect("temp dir path must be UTF-8");
        let log_path = log_dir.join("test.jsonl");

        let http = Arc::new(HttpClient::new_for_tests_allow_http(
            "tdm-ieee",
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
            cache_root: None,
        };
        (td, ctx)
    }

    /// Sentinel test key used in the happy-path wiremock matcher.
    const TEST_KEY: &str = "test-key-xyz";

    fn profile_with_ieee_grant() -> CapabilityProfile {
        let mut p = CapabilityProfile::from_env().expect("clean env never errors");
        p.tdm_ieee = Some(TdmGrant {
            // Issue #153: the key flows through the grant, not the env
            // var, so the fixture seeds it directly.
            api_key: secrecy::SecretString::from(TEST_KEY.to_string()),
            agree_env_var: "DOIGET_AGREE_TDM_IEEE".to_string(),
            ..Default::default()
        });
        p
    }

    /// Grant whose key is empty — exercises the fail-closed branch.
    fn profile_with_empty_key_grant() -> CapabilityProfile {
        let mut p = CapabilityProfile::from_env().expect("clean env never errors");
        p.tdm_ieee = Some(TdmGrant {
            api_key: secrecy::SecretString::from(String::new()),
            agree_env_var: "DOIGET_AGREE_TDM_IEEE".to_string(),
            ..Default::default()
        });
        p
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn fetch_doi_returns_first_article_and_passes_key_in_query() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/search/articles"))
            .and(query_param("doi", "10.1109/TSP.2018.2812747"))
            .and(query_param("format", "json"))
            .and(query_param("apikey", TEST_KEY))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_ENVELOPE_HIT))
            .mount(&server)
            .await;

        let (_td, ctx) = build_test_context(&server.uri());
        let src = TdmIeeeSource::with_base(Url::parse(&server.uri()).expect("wiremock URI parses"));
        let profile = profile_with_ieee_grant();
        let ref_ = Ref::Doi(Doi::parse("10.1109/TSP.2018.2812747").expect("DOI parses"));

        let result = src.fetch(&ref_, &profile, &ctx).await.expect("fetch ok");

        assert_eq!(result.source, "tdm-ieee");
        assert!(result.pdf_bytes.is_none(), "metadata-only contract");
        // Issue #146: the returned final_url must NOT carry the real key.
        let final_url = result.final_url.expect("final_url present");
        assert!(
            !final_url.as_str().contains(TEST_KEY),
            "apikey must be redacted out of final_url: {final_url}"
        );
        assert!(
            final_url
                .query_pairs()
                .any(|(k, v)| k == "apikey" && v == "REDACTED"),
            "redacted apikey sentinel must be present: {final_url}"
        );
        let meta = result.metadata_json.expect("metadata_json present");
        assert_eq!(meta["title"], "Example IEEE Transactions Article");
    }

    /// #430: the response shape is inferred, so the failure has to name
    /// what was missing AND quote what arrived. Anything less and the
    /// first run against a real key reports "no records" for a response
    /// that may have been a perfectly good record in another shape.
    #[tokio::test]
    #[serial_test::serial]
    async fn an_unexpected_envelope_names_the_field_and_quotes_the_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/search/articles"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_UNEXPECTED_SHAPE))
            .mount(&server)
            .await;

        let (_td, ctx) = build_test_context(&server.uri());
        let src = TdmIeeeSource::with_base(Url::parse(&server.uri()).expect("wiremock URI parses"));
        let profile = profile_with_ieee_grant();
        let ref_ = Ref::Doi(Doi::parse("10.1109/TSP.2018.2812747").expect("DOI parses"));

        let err = src
            .fetch(&ref_, &profile, &ctx)
            .await
            .expect_err("unexpected shape must not look like success");
        let FetchError::SourceSchema { hint } = err else {
            panic!("expected SourceSchema, got a different error");
        };
        assert!(hint.contains("articles"), "name the missing field: {hint}");
        assert!(
            hint.contains("totalfound"),
            "quote what actually arrived, or the real shape stays invisible: {hint}"
        );
    }

    /// #460: the branch a real user hits first, and the one nothing
    /// covered.
    ///
    /// A 403 is an `HttpError::HttpStatus`, so it never reaches the JSON
    /// parsing above — the `SourceSchema` test is not this path. Two
    /// things have to hold on it:
    ///
    /// 1. the surfaced text carries the status, so "not activated yet"
    ///    is distinguishable from "wrong key"; and
    /// 2. the key is gone. IEEE is query-param auth, so the URL inside
    ///    `HttpStatus` carries `apikey=` and that string is
    ///    `tracing`-logged. #457 taught `redact_api_key_query` the
    ///    `apikey` spelling; this is the end-to-end proof, through the
    ///    HTTP layer rather than the module-local redactor alone.
    #[tokio::test]
    #[serial_test::serial]
    async fn an_inactive_key_403_says_so_and_does_not_leak_the_key() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/search/articles"))
            .respond_with(
                ResponseTemplate::new(403)
                    .insert_header("content-type", "text/xml")
                    .set_body_string(SAMPLE_INACTIVE_KEY_BODY),
            )
            .mount(&server)
            .await;

        let (_td, ctx) = build_test_context(&server.uri());
        let src = TdmIeeeSource::with_base(Url::parse(&server.uri()).expect("wiremock URI parses"));
        let profile = profile_with_ieee_grant();
        let ref_ = Ref::Doi(Doi::parse("10.1109/TSP.2018.2812747").expect("DOI parses"));

        let err = src
            .fetch(&ref_, &profile, &ctx)
            .await
            .expect_err("403 must not look like success");
        let rendered = err.to_string();

        assert!(
            rendered.contains("403"),
            "the status is the only thing telling an inactive key from a wrong one: {rendered}"
        );
        assert!(
            !rendered.contains(TEST_KEY),
            "the api key must never reach an error string: {rendered}"
        );
        assert!(
            rendered.contains("REDACTED"),
            "the redaction must be visible, not just absent: {rendered}"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn an_empty_articles_array_is_a_miss_not_a_hit() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/search/articles"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_ENVELOPE_EMPTY))
            .mount(&server)
            .await;

        let (_td, ctx) = build_test_context(&server.uri());
        let src = TdmIeeeSource::with_base(Url::parse(&server.uri()).expect("wiremock URI parses"));
        let profile = profile_with_ieee_grant();
        let ref_ = Ref::Doi(Doi::parse("10.1109/example").expect("DOI parses"));

        let err = src
            .fetch(&ref_, &profile, &ctx)
            .await
            .expect_err("0 articles must not be a hit");
        assert!(matches!(err, FetchError::SourceSchema { .. }));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn fetch_with_empty_grant_key_is_not_eligible() {
        let (_td, ctx) = build_test_context("http://127.0.0.1:1");
        let src = TdmIeeeSource::with_base(Url::parse("http://127.0.0.1:1").expect("parses"));
        let profile = profile_with_empty_key_grant();
        let ref_ = Ref::Doi(Doi::parse("10.1109/example").expect("DOI parses"));

        let err = src
            .fetch(&ref_, &profile, &ctx)
            .await
            .expect_err("empty grant key must fail-close");
        assert!(matches!(err, FetchError::NotEligible { .. }));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn fetch_without_grant_is_not_eligible() {
        let (_td, ctx) = build_test_context("http://127.0.0.1:1");
        let src = TdmIeeeSource::with_base(Url::parse("http://127.0.0.1:1").expect("parses"));
        let profile = CapabilityProfile::from_env().expect("clean env never errors");
        let ref_ = Ref::Doi(Doi::parse("10.1109/example").expect("DOI parses"));

        let err = src
            .fetch(&ref_, &profile, &ctx)
            .await
            .expect_err("no grant must fail-close");
        assert!(matches!(err, FetchError::NotEligible { .. }));
    }

    #[test]
    fn redact_apikey_in_url_replaces_only_the_key() {
        let u = Url::parse(
            "https://ieeexploreapi.ieee.org/api/v1/search/articles?doi=10.1109/x&apikey=SUPERSECRET",
        )
        .expect("parses");
        let r = redact_apikey_in_url(&u);
        assert!(!r.as_str().contains("SUPERSECRET"), "key must be gone: {r}");
        assert!(
            r.query_pairs().any(|(k, v)| k == "doi" && v == "10.1109/x"),
            "other pairs preserved: {r}"
        );
        assert!(r
            .query_pairs()
            .any(|(k, v)| k == "apikey" && v == "REDACTED"));
        // No-op when there is no apikey pair.
        let clean =
            Url::parse("https://ieeexploreapi.ieee.org/api/v1/search/articles?doi=10.1109/x")
                .expect("parses");
        assert_eq!(redact_apikey_in_url(&clean), clean);
    }

    /// #442, restated for the fourth source: a grant is an opt-in to
    /// disclose IEEE's own DOIs to IEEE, not the user's whole reading
    /// list.
    #[test]
    fn can_serve_refuses_another_publishers_doi_even_with_a_valid_grant() {
        let src = TdmIeeeSource::new();
        let profile = profile_with_ieee_grant();
        let own = Ref::Doi(Doi::parse("10.1109/TSP.2018.2812747").expect("DOI parses"));
        let foreign = Ref::Doi(Doi::parse("10.1007/example").expect("DOI parses"));

        assert!(src.can_serve(&profile, &own), "must serve an IEEE DOI");
        assert!(
            !src.can_serve(&profile, &foreign),
            "must NOT serve a Springer DOI even with a valid grant -- that \
             would disclose an unrelated lookup to IEEE"
        );
    }

    /// The conference prefix is the one most likely to be dropped by a
    /// future edit, and it is exactly the surface #407 measured.
    #[test]
    fn both_registered_ieee_prefixes_are_served() {
        let src = TdmIeeeSource::new();
        let profile = profile_with_ieee_grant();
        for doi in ["10.1109/example", "10.23919/example"] {
            let ref_ = Ref::Doi(Doi::parse(doi).expect("DOI parses"));
            assert!(src.can_serve(&profile, &ref_), "must serve {doi}");
        }
    }
}
