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
//! is *supposed* to gate on `can_serve` first). The key value is read
//! once at startup and carried in
//! [`TdmGrant::api_key`](crate::TdmGrant) (issue #153); this source
//! consumes it from the grant and never re-reads
//! `DOIGET_KEY_SPRINGER` at fetch time.
//!
//! ## Credential hygiene — key in the URL (issue #146)
//!
//! Unlike Elsevier (`X-ELS-APIKey` header) and APS (`X-API-Key`
//! header), the Springer Nature API authenticates **only** via an
//! `api_key` URL query parameter. The official Springer Nature API
//! client sends it the same way (`params["api_key"] = api_key`); there
//! is no header-auth path for either the regular or the TDM endpoint.
//! Moving the key to a header (the preferred #146 fix) is therefore
//! not possible against this upstream.
//!
//! Residual-risk mitigation instead: the key never reaches any log or
//! recorded-provenance sink. The request URL is built with the key,
//! but every URL this module hands back (the `FetchResult::final_url`
//! and any error string) is first passed through
//! `redact_api_key_in_url`, which replaces the `api_key` value with
//! `REDACTED`. The key still appears on the wire and in Springer's own
//! server-side / proxy logs — that is inherent to query-param auth and
//! is documented here and in `docs/CAPABILITY.md` §1 as accepted
//! residual risk.
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
use secrecy::ExposeSecret;
use url::Url;

use crate::provenance::{Capability, LogEvent, LogResult, RowInput};
use crate::source::{FetchContext, FetchError, FetchResult, Source};
use crate::{CapabilityProfile, Ref};

/// Production Springer Nature TDM API base URL.
const DEFAULT_BASE: &str = "https://api.springernature.com";

/// DOI registrant prefixes Springer Nature actually owns.
///
/// #442: without this the source answered `can_serve` for ANY DOI, so
/// wiring it into the chain would have sent every lookup — including
/// DOIs belonging to other publishers — to Springer Nature. That is a
/// politeness problem (`docs/SOURCES.md` §6) and a privacy one
/// (`docs/PRIVACY.md`): it would disclose the user's whole reading list
/// to a publisher who has no part in it.
///
/// Scoped to the DOIs this publisher registered, the disclosure is
/// nil — resolving such a DOI goes through them anyway.
///
/// Verified against the Crossref registrant registry
/// (`api.crossref.org/prefixes/<prefix>`) on 2026-08-23.
/// `10.1007` is Springer proper, `10.1038` Nature, `10.1057` Palgrave Macmillan, `10.1140` the European Physical Journal.
///
/// Deliberately conservative: a publisher may own prefixes not listed
/// here. A miss is not silent — it is recorded in the attempt trace as
/// `not consulted (DOI prefix ... is not ...)`, so it is diagnosable
/// from the error message rather than looking like a lookup failure.
pub(crate) const PUBLISHER_PREFIXES: &[&str] = &["10.1007", "10.1038", "10.1057", "10.1140"];

/// The `api_key` query-parameter name Springer Nature expects, and the
/// value it is replaced with in any URL that leaves this module bound
/// for a log or recorded-provenance sink (issue #146).
const API_KEY_PARAM: &str = "api_key";
const REDACTED: &str = "REDACTED";

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
            .append_pair(API_KEY_PARAM, api_key);
        Ok(url)
    }
}

/// Return a copy of `url` with the `api_key` query-parameter value
/// replaced by `REDACTED`, preserving every other pair and ordering.
///
/// Springer Nature only supports query-param auth (issue #146), so the
/// key unavoidably appears on the wire and in Springer's own logs. This
/// keeps it out of *our* sinks: the `FetchResult::final_url` (which the
/// orchestrator persists into the metadata TOML `url` field) and any
/// error string this module produces. If no `api_key` pair is present
/// the URL is returned structurally unchanged.
fn redact_api_key_in_url(url: &Url) -> Url {
    if url.query_pairs().all(|(k, _)| k != API_KEY_PARAM) {
        return url.clone();
    }
    let mut redacted = url.clone();
    // Re-serialize the pairs, swapping only the api_key value. `clear()`
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
        let Ref::Doi(doi) = ref_ else {
            return false;
        };
        // Both halves matter: the grant is the user's opt-in, the prefix
        // keeps that opt-in from leaking unrelated DOIs to the publisher
        // (#442). The orchestrator checks the same two conditions so it
        // can tell them apart in the trace; this is the defensive mirror.
        profile.tdm_springer.is_some() && PUBLISHER_PREFIXES.contains(&doi.prefix())
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

        // Defensive gate (1/3 + 2/3): the runtime grant must be
        // populated, and it now carries the key validated at startup
        // (issue #153). The orchestrator is supposed to call
        // `can_serve` first, but we re-check here so a misrouted call
        // still fail-closes per ADR-0019. The key is no longer
        // re-read from the env at fetch time — `CapabilityProfile` is
        // immutable for the process lifetime (`docs/CAPABILITY.md`
        // §6), so the startup grant is the single source of truth.
        let grant = profile
            .tdm_springer
            .as_ref()
            .ok_or_else(|| FetchError::NotEligible {
                source_key: "tdm-springer".into(),
            })?;
        let api_key = grant.api_key.expose_secret();
        if api_key.is_empty() {
            return Err(FetchError::NotEligible {
                source_key: "tdm-springer".into(),
            });
        }

        let _permit = ctx.rate_limiter.acquire(self.name()).await;

        let url = self.request_url(doi, api_key)?;
        let (body, final_url) = ctx.http.fetch_bytes(self.name(), url).await?;
        // Issue #146: Springer auth is query-param only; strip the key
        // from the URL before it can reach the metadata TOML / any log.
        let final_url = redact_api_key_in_url(&final_url);

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
        "query": "doi:10.1007/example",
        "records": [
            {
                "identifier": "doi:10.1007/example",
                "title": "Example Springer OA Article",
                "publicationName": "Example Journal",
                "openaccess": "true"
            }
        ]
    }"#;

    const SAMPLE_ENVELOPE_EMPTY: &str = r#"{
        "apiMessage": "ok",
        "query": "doi:10.1007/example",
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
            cache_root: None,
        };
        (td, ctx)
    }

    /// Sentinel test key used in the happy-path wiremock matcher.
    const TEST_KEY: &str = "test-key-xyz";

    fn profile_with_springer_grant() -> CapabilityProfile {
        let mut p = CapabilityProfile::from_env().expect("clean env never errors");
        p.tdm_springer = Some(TdmGrant {
            // Issue #153: the key now flows through the grant, not the
            // env var, so the fixture seeds it directly.
            api_key: secrecy::SecretString::from(TEST_KEY.to_string()),
            agree_env_var: "DOIGET_AGREE_TDM_SPRINGER".to_string(),
            ..Default::default()
        });
        p
    }

    /// Grant whose key is empty — exercises the fail-closed branch.
    fn profile_with_empty_key_grant() -> CapabilityProfile {
        let mut p = CapabilityProfile::from_env().expect("clean env never errors");
        p.tdm_springer = Some(TdmGrant {
            api_key: secrecy::SecretString::from(String::new()),
            agree_env_var: "DOIGET_AGREE_TDM_SPRINGER".to_string(),
            ..Default::default()
        });
        p
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn fetch_doi_returns_first_record_and_passes_key_in_query() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/openaccess/json"))
            .and(query_param("q", "doi:10.1007/example"))
            .and(query_param("api_key", TEST_KEY))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_ENVELOPE_HIT))
            .mount(&server)
            .await;

        let (_td, ctx) = build_test_context(&server.uri());
        let src =
            TdmSpringerSource::with_base(Url::parse(&server.uri()).expect("wiremock URI parses"));
        let profile = profile_with_springer_grant();
        let ref_ = Ref::Doi(Doi::parse("10.1007/example").expect("DOI parses"));

        // Key comes from the grant now — no env var is touched.
        let result = src.fetch(&ref_, &profile, &ctx).await.expect("fetch ok");

        assert_eq!(result.source, "tdm-springer");
        assert!(result.pdf_bytes.is_none(), "metadata-only contract");
        // Issue #146: the returned final_url must NOT carry the real key.
        let final_url = result.final_url.expect("final_url present");
        assert!(
            !final_url.as_str().contains(TEST_KEY),
            "api_key must be redacted out of final_url: {final_url}"
        );
        assert!(
            final_url
                .query_pairs()
                .any(|(k, v)| k == "api_key" && v == "REDACTED"),
            "redacted api_key sentinel must be present: {final_url}"
        );
        let meta = result.metadata_json.expect("metadata_json present");
        assert_eq!(meta["title"], "Example Springer OA Article");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn fetch_with_empty_grant_key_is_not_eligible() {
        let (_td, ctx) = build_test_context("http://127.0.0.1:1");
        let src = TdmSpringerSource::with_base(Url::parse("http://127.0.0.1:1").expect("parses"));
        let profile = profile_with_empty_key_grant();
        let ref_ = Ref::Doi(Doi::parse("10.1007/example").expect("DOI parses"));

        let err = src
            .fetch(&ref_, &profile, &ctx)
            .await
            .expect_err("empty grant key must fail-close");
        assert!(matches!(err, FetchError::NotEligible { .. }));
    }

    #[test]
    fn redact_api_key_in_url_replaces_only_the_key() {
        let u = Url::parse(
            "https://api.springernature.com/openaccess/json?q=doi:10.1/x&api_key=SUPERSECRET",
        )
        .expect("parses");
        let r = redact_api_key_in_url(&u);
        assert!(!r.as_str().contains("SUPERSECRET"), "key must be gone: {r}");
        assert!(
            r.query_pairs().any(|(k, v)| k == "q" && v == "doi:10.1/x"),
            "other pairs preserved: {r}"
        );
        assert!(r
            .query_pairs()
            .any(|(k, v)| k == "api_key" && v == "REDACTED"));
        // No-op when there is no api_key pair.
        let clean = Url::parse("https://api.springernature.com/openaccess/json?q=doi:10.1/x")
            .expect("parses");
        assert_eq!(redact_api_key_in_url(&clean), clean);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn fetch_without_grant_is_not_eligible() {
        let (_td, ctx) = build_test_context("http://127.0.0.1:1");
        let src = TdmSpringerSource::with_base(Url::parse("http://127.0.0.1:1").expect("parses"));
        let profile = CapabilityProfile::from_env().expect("clean env never errors");
        let ref_ = Ref::Doi(Doi::parse("10.1007/example").expect("DOI parses"));

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
        let ref_ = Ref::Doi(Doi::parse("10.1007/example").expect("DOI parses"));

        let result = src.fetch(&ref_, &profile, &ctx).await;

        let err = result.expect_err("empty records must surface as SourceSchema");
        assert!(matches!(err, FetchError::SourceSchema { .. }));
    }
    /// #442: the grant is the user's opt-in; the prefix keeps that opt-in
    /// from disclosing unrelated DOIs to Springer Nature. Asserted with the grant
    /// PRESENT so the prefix is the only thing under test — otherwise a
    /// false `can_serve` would prove nothing about scoping.
    #[test]
    fn can_serve_is_scoped_to_this_publishers_prefixes() {
        let src = TdmSpringerSource::new();
        let profile = profile_with_springer_grant();

        let own = Ref::Doi(Doi::parse("10.1007/example").expect("DOI parses"));
        assert!(
            src.can_serve(&profile, &own),
            "must serve its own publisher's DOI when the grant is present"
        );

        let foreign = Ref::Doi(Doi::parse("10.1103/PhysRevX.10.011001").expect("DOI parses"));
        assert!(
            !src.can_serve(&profile, &foreign),
            "must NOT serve a APS DOI even with a valid grant -- that              would disclose an unrelated lookup to Springer Nature"
        );

        for p in PUBLISHER_PREFIXES {
            let doi = Doi::parse(&format!("{p}/probe")).expect("DOI parses");
            assert!(
                src.can_serve(&profile, &Ref::Doi(doi)),
                "declared prefix {p} must be served"
            );
        }
    }
}
