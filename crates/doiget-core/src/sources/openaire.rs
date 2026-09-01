//! OpenAIRE source — European repository aggregation (Tier 2, #416).
//!
//! Spec: `docs/SOURCES.md` §1 Tier 2 row + §4. OpenAIRE aggregates
//! European institutional and funder repositories, covering OA deposits
//! that never reach Crossref and therefore never reach Unpaywall.
//!
//! ## Graph API v1, not the legacy search endpoint
//!
//! #416 measured both on 2026-08-22:
//!
//! ```text
//! GET /graph/v1/researchProducts?pid=<doi>   -> 200
//! GET /search/publications?doi=<doi>         -> 503
//! ```
//!
//! The legacy `/search/publications` endpoint is unstable and must not be
//! used. Only the Graph API is wired here.
//!
//! ## Capability gate
//!
//! `OpenAireSource::can_serve` returns `true` only when
//! [`CapabilityProfile.metadata.openaire`](crate::CapabilityProfile) is
//! `true` AND the ref is a `Ref::Doi`. The bool is set by
//! `CapabilityProfile::from_env` from `DOIGET_ENABLE_OPENAIRE`, which is
//! **off by default** (ADR-0040) — unset, this source is inert.
//!
//! ## Access rights are honoured, never guessed
//!
//! OpenAIRE aggregates records with **mixed** access rights, so unlike a
//! pure-OA index a hit is not evidence of availability. `bestAccessRight`
//! carries a COAR vocabulary code; only `c_abf2` (OPEN) is accepted, and
//! anything else — EMBARGO, RESTRICTED, CLOSED, or a missing field — is
//! refused. Returning a closed record would look like a hit and resolve to
//! nothing readable.
//!
//! ## Overlap with DataCite is expected
//!
//! OpenAIRE mirrors a large slice of Zenodo, which `super::datacite`
//! also resolves. That is why #413 ordered DataCite first: this source is
//! consulted later in the chain, so the overlap costs nothing beyond a
//! request that is not made.
//!
//! ## Metadata-only contract
//!
//! Per `docs/SOURCES.md` §4 this source never returns PDF bytes. Instance
//! URLs point at publisher or repository hosts, which are reached through
//! the `oa-publisher` key, not this one.
//!
//! API: public Graph API v1, no auth for basic queries.
//! Terms: <https://graph.openaire.eu/docs/>

use async_trait::async_trait;
use url::Url;

use crate::provenance::{Capability, LogEvent, LogResult, RowInput};
use crate::source::{FetchContext, FetchError, FetchResult, Source};
use crate::{CapabilityProfile, Ref};

/// Production OpenAIRE API base.
const DEFAULT_BASE: &str = "https://api.openaire.eu";

/// COAR access-rights code for OPEN.
///
/// Matched on the code, not the human label: `label` is display text and
/// may be localised or reworded, while the COAR code is a stable
/// vocabulary term.
const COAR_OPEN: &str = "c_abf2";

/// OpenAIRE [`Source`] impl — DOI to research-product record.
#[derive(Clone, Debug)]
pub struct OpenAireSource {
    /// API base URL. Production pins `https://api.openaire.eu`;
    /// [`with_base`](Self::with_base) lets wiremock substitute an
    /// `http://127.0.0.1:N` origin.
    base: Url,
}

impl OpenAireSource {
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

    /// Build `/graph/v1/researchProducts?pid=<doi>&pageSize=1`.
    ///
    /// The DOI goes in a **query parameter**, so its `/` must be
    /// percent-encoded — an unencoded one makes the endpoint reject the
    /// request (observed while verifying the shape). `query_pairs_mut`
    /// handles that, which is why this source does not share
    /// `super::datacite`'s path-segment approach.
    fn request_url(&self, doi: &crate::Doi) -> Result<Url, FetchError> {
        let mut url =
            self.base
                .join("/graph/v1/researchProducts")
                .map_err(|e| FetchError::SourceSchema {
                    hint: format!("openaire URL construction failed: {e}"),
                })?;
        url.query_pairs_mut()
            .append_pair("pid", doi.as_str())
            .append_pair("pageSize", "1");
        Ok(url)
    }
}

impl Default for OpenAireSource {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Source for OpenAireSource {
    fn name(&self) -> &str {
        "openaire"
    }

    fn can_serve(&self, profile: &CapabilityProfile, ref_: &Ref) -> bool {
        profile.metadata.openaire && matches!(ref_, Ref::Doi(_))
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
                    source_key: "openaire".into(),
                });
            }
        };

        if !profile.metadata.openaire {
            return Err(FetchError::NotEligible {
                source_key: "openaire".into(),
            });
        }

        let _permit = ctx.rate_limiter.acquire(self.name()).await;

        let url = self.request_url(doi)?;
        let (body, final_url) = ctx.http.fetch_bytes(self.name(), url).await?;

        let envelope: serde_json::Value =
            serde_json::from_slice(&body).map_err(|e| FetchError::SourceSchema {
                hint: format!("openaire returned non-JSON: {e}"),
            })?;

        // Graph envelope: { header: { numFound, .. }, results: [ .. ] }.
        let results = envelope
            .get("results")
            .and_then(|r| r.as_array())
            .ok_or_else(|| FetchError::SourceSchema {
                hint: format!(
                    "openaire response missing `results` array (got: {})",
                    truncate_for_hint(&body)
                ),
            })?;
        let record = results.first().ok_or_else(|| FetchError::SourceSchema {
            hint: "openaire has no research product for this DOI".to_string(),
        })?;

        if !is_open_access(record) {
            return Err(FetchError::NotRetrievable {
                source_key: "openaire".to_string(),
                detail: format!(
                    "record is not open access (bestAccessRight.code = {})",
                    access_right_code(record).unwrap_or("absent")
                ),
            });
        }

        let license = license_of(record);
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
            metadata_json: Some(record.clone()),
        })
    }
}

/// `bestAccessRight.code`, if present.
#[must_use]
pub fn access_right_code(record: &serde_json::Value) -> Option<&str> {
    record
        .get("bestAccessRight")
        .and_then(|r| r.get("code"))
        .and_then(|v| v.as_str())
}

/// True only for the COAR OPEN code.
///
/// Absent counts as **not** open. OpenAIRE aggregates mixed rights, so an
/// unknown here is genuinely unknown, and treating it as open would return
/// records that resolve to a paywall.
#[must_use]
pub fn is_open_access(record: &serde_json::Value) -> bool {
    access_right_code(record) == Some(COAR_OPEN)
}

/// Licence from the first instance that declares one, else `"unknown"`.
///
/// OpenAIRE puts licences on `instances[]`, not on the record, and the
/// value is free text from the source repository (e.g. "APS Licenses for
/// Journal Article Re-use") rather than an SPDX id. Passed through
/// verbatim: normalising it would mean guessing, and a wrong licence is
/// worse than an absent one.
fn license_of(record: &serde_json::Value) -> String {
    record
        .get("instances")
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            arr.iter()
                .find_map(|i| i.get("license").and_then(|v| v.as_str()))
        })
        .unwrap_or("unknown")
        .to_string()
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
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::http::HttpClient;
    use crate::provenance::ProvenanceLog;
    use crate::rate_limiter::RateLimiter;
    use crate::{CapabilityProfile, Doi, MetadataAccess, RateLimits, Ref};

    /// Trimmed real shape, captured 2026-08-22 from
    /// `GET /graph/v1/researchProducts?pid=10.1103/PhysRevB.92.125119`.
    /// Note the licence lives on `instances[]`, not on the record, and is
    /// publisher free text rather than an SPDX id.
    const SAMPLE_OPEN: &str = r#"{
        "header": {"numFound": 1, "page": 1, "pageSize": 1},
        "results": [
            {
                "id": "doi_dedup___::8bd761bf23510d840dc1f38adebdc964",
                "type": "publication",
                "mainTitle": "Minimally entangled typical thermal states",
                "publicationDate": "2015-09-10",
                "bestAccessRight": {
                    "code": "c_abf2",
                    "label": "OPEN",
                    "scheme": "http://vocabularies.coar-repositories.org/documentation/access_rights/"
                },
                "openAccessColor": "bronze",
                "isGreen": true,
                "instances": [
                    {
                        "license": "APS Licenses for Journal Article Re-use",
                        "type": "Article",
                        "urls": ["https://doi.org/10.1103/physrevb.92.125119"]
                    }
                ]
            }
        ]
    }"#;

    const SAMPLE_CLOSED: &str = r#"{
        "header": {"numFound": 1},
        "results": [
            {
                "id": "x",
                "mainTitle": "A Restricted Record",
                "bestAccessRight": {"code": "c_16ec", "label": "RESTRICTED"},
                "instances": []
            }
        ]
    }"#;

    const SAMPLE_EMPTY: &str = r#"{"header": {"numFound": 0}, "results": []}"#;

    fn build_test_context(wiremock_host: &str) -> (TempDir, FetchContext) {
        let td = TempDir::new().expect("tempdir");
        let log_dir =
            Utf8PathBuf::try_from(td.path().to_path_buf()).expect("temp dir path must be UTF-8");
        let http = Arc::new(HttpClient::new_for_tests_allow_http(
            "openaire",
            wiremock_host,
        ));
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

    fn profile(openaire: bool) -> CapabilityProfile {
        let mut p = CapabilityProfile::for_tests();
        p.metadata = MetadataAccess {
            openalex: false,
            semantic_scholar: false,
            doaj: false,
            datacite: false,
            hal: false,
            openaire,
            core: false,
            europe_pmc: false,
        };
        p
    }

    /// The DOI travels as a query parameter, so its `/` MUST be
    /// percent-encoded — the endpoint rejects an unencoded one. This is the
    /// opposite requirement to the DataCite source, where the slash has to
    /// stay structural, so it is worth pinning explicitly.
    #[test]
    fn request_url_percent_encodes_the_doi_in_the_query() {
        let src = OpenAireSource::new();
        let doi = Doi::parse("10.1103/PhysRevB.92.125119").expect("valid doi");
        let url = src.request_url(&doi).expect("url builds");
        assert_eq!(url.path(), "/graph/v1/researchProducts");
        assert!(
            url.query()
                .expect("query")
                .contains("pid=10.1103%2FPhysRevB.92.125119"),
            "slash must be encoded; got {}",
            url.query().unwrap_or_default()
        );
        // …and it round-trips back to the original value.
        let pid = url
            .query_pairs()
            .find(|(k, _)| k == "pid")
            .expect("pid param")
            .1
            .into_owned();
        assert_eq!(pid, "10.1103/PhysRevB.92.125119");
    }

    #[tokio::test]
    async fn fetch_returns_an_open_record_with_its_instance_license() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/graph/v1/researchProducts"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_OPEN))
            .mount(&server)
            .await;

        let (_td, ctx) = build_test_context(&server.address().to_string());
        let src = OpenAireSource::with_base(Url::parse(&server.uri()).expect("base"));
        let ref_ = Ref::Doi(Doi::parse("10.1103/PhysRevB.92.125119").expect("doi"));

        let got = src
            .fetch(&ref_, &profile(true), &ctx)
            .await
            .expect("fetch succeeds");
        assert_eq!(got.source, "openaire");
        assert!(got.pdf_bytes.is_none(), "metadata-only contract");
        assert_eq!(got.license, "APS Licenses for Journal Article Re-use");
        let rec = got.metadata_json.expect("record");
        assert!(is_open_access(&rec));
    }

    /// OpenAIRE aggregates MIXED access rights, so a hit is not evidence of
    /// availability. A RESTRICTED record must be refused, not returned.
    #[tokio::test]
    async fn restricted_records_are_refused() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/graph/v1/researchProducts"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_CLOSED))
            .mount(&server)
            .await;

        let (_td, ctx) = build_test_context(&server.address().to_string());
        let src = OpenAireSource::with_base(Url::parse(&server.uri()).expect("base"));
        let ref_ = Ref::Doi(Doi::parse("10.1234/restricted").expect("doi"));
        let err = src
            .fetch(&ref_, &profile(true), &ctx)
            .await
            .expect_err("restricted records must not be returned");
        // #538: the CATEGORY, not "some schema error". `SourceSchema`
        // collapses to INTERNAL_ERROR at the boundary, which said the source
        // broke; a refusal is `NoOaAvailable`, which says the work is not
        // free here.
        assert!(
            matches!(err, FetchError::NotRetrievable { .. }),
            "an access refusal is its own variant, not a schema failure: {err:?}"
        );
        assert_eq!(
            crate::ErrorCode::from(&err),
            crate::ErrorCode::NoOaAvailable,
            "and it must not surface as an internal error: {err:?}"
        );
    }

    /// Only the COAR code counts. `label` is display text that may be
    /// localised or reworded, and an absent field is genuinely unknown —
    /// which must NOT be read as open.
    #[test]
    fn access_rights_are_judged_on_the_coar_code_only() {
        let open = serde_json::json!({"bestAccessRight": {"code": "c_abf2", "label": "OPEN"}});
        assert!(is_open_access(&open));

        let embargo =
            serde_json::json!({"bestAccessRight": {"code": "c_f1cf", "label": "EMBARGO"}});
        assert!(!is_open_access(&embargo));

        // Label says OPEN but the code does not: the code wins.
        let lying = serde_json::json!({"bestAccessRight": {"code": "c_16ec", "label": "OPEN"}});
        assert!(!is_open_access(&lying), "label must not override the code");

        assert!(
            !is_open_access(&serde_json::json!({})),
            "absent is not open"
        );
    }

    #[tokio::test]
    async fn no_record_surfaces_as_source_schema() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/graph/v1/researchProducts"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_EMPTY))
            .mount(&server)
            .await;

        let (_td, ctx) = build_test_context(&server.address().to_string());
        let src = OpenAireSource::with_base(Url::parse(&server.uri()).expect("base"));
        let ref_ = Ref::Doi(Doi::parse("10.1234/absent").expect("doi"));
        let err = src
            .fetch(&ref_, &profile(true), &ctx)
            .await
            .expect_err("must not claim success");
        assert!(
            matches!(err, FetchError::SourceSchema { .. }),
            "got {err:?}"
        );
    }

    /// The regression #413 requires of every new source.
    #[tokio::test]
    async fn is_inert_when_the_runtime_flag_is_unset() {
        let server = MockServer::start().await;
        let (_td, ctx) = build_test_context(&server.address().to_string());
        let src = OpenAireSource::with_base(Url::parse(&server.uri()).expect("base"));
        let ref_ = Ref::Doi(Doi::parse("10.1103/PhysRevB.92.125119").expect("doi"));

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
    async fn arxiv_refs_are_not_eligible() {
        let server = MockServer::start().await;
        let (_td, ctx) = build_test_context(&server.address().to_string());
        let src = OpenAireSource::with_base(Url::parse(&server.uri()).expect("base"));
        let ref_ = Ref::Arxiv(crate::ArxivId::parse("2401.12345").expect("arxiv id"));
        assert!(!src.can_serve(&profile(true), &ref_));
        assert!(matches!(
            src.fetch(&ref_, &profile(true), &ctx).await,
            Err(FetchError::NotEligible { .. })
        ));
    }

    #[test]
    fn license_falls_back_to_unknown_when_no_instance_declares_one() {
        let rec = serde_json::json!({"instances": [{"type": "Article"}]});
        assert_eq!(license_of(&rec), "unknown");
        assert_eq!(license_of(&serde_json::json!({})), "unknown");
    }
}
