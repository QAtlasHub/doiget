//! Europe PMC source — biomedical OA full text (Tier 2, #415).
//!
//! Spec: `docs/SOURCES.md` §1 Tier 2 row + §4. Europe PMC holds OA full
//! text (the PMC mirror plus Europe-specific deposits) that Unpaywall's
//! `best_oa_location` frequently misses, or for which Unpaywall returns a
//! landing page rather than a `url_for_pdf`.
//!
//! ## Capability gate
//!
//! `EuropePmcSource::can_serve` returns `true` only when
//! [`CapabilityProfile.metadata.europe_pmc`](crate::CapabilityProfile) is
//! `true` AND the ref is a `Ref::Doi`. The bool is set by
//! `CapabilityProfile::from_env` from `DOIGET_ENABLE_EUROPE_PMC`, which
//! is **off by default** (ADR-0040).
//!
//! ## OA subset only
//!
//! Europe PMC indexes far more than it can give you: a record can be
//! `inEPMC = "Y"` — present in the archive — while `isOpenAccess = "N"`,
//! meaning the full text sits behind the publisher subscription. Only
//! `isOpenAccess = "Y"` is accepted, and anything else is an explicit
//! refusal rather than a hit, exactly as #415 requires. `inEPMC` is
//! deliberately **not** the gate; it answers a different question.
//!
//! ## Retrieval goes through the existing OA chain, not this source
//!
//! Per `docs/SOURCES.md` §4 this source returns no PDF bytes. It surfaces
//! the OA PDF location from `fullTextUrlList` via
//! `open_access_pdf_url`, and the fetch itself is performed by the
//! existing `oa-publisher` leg — `europepmc.org` is already on that
//! allowlist.
//!
//! That is a deliberate choice about where a failure surfaces. #415 asks
//! for a distinct error when a proxy blocks the download; routing through
//! `oa-publisher` gives that for free, and gives it *better*, because the
//! existing denial machinery already carries an ADR-0023 `denial_context`
//! naming the attempted host and the allowlist that rejected it. A bespoke
//! `ErrorCode` would carry strictly less information and would widen the
//! closed set in `docs/ERRORS.md` §3 for no gain.
//!
//! API: public REST, no auth, no key.
//! About / terms: <https://europepmc.org/About>

use async_trait::async_trait;
use url::Url;

use crate::provenance::{Capability, LogEvent, LogResult, RowInput};
use crate::source::{FetchContext, FetchError, FetchResult, Source};
use crate::{CapabilityProfile, Ref};

/// Production Europe PMC REST base.
const DEFAULT_BASE: &str = "https://www.ebi.ac.uk";

/// Europe PMC [`Source`] impl — DOI to indexed record.
#[derive(Clone, Debug)]
pub struct EuropePmcSource {
    /// API base URL. Production pins `https://www.ebi.ac.uk`;
    /// [`with_base`](Self::with_base) lets wiremock substitute an
    /// `http://127.0.0.1:N` origin.
    base: Url,
}

impl EuropePmcSource {
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

    /// Build the search URL for one DOI.
    ///
    /// `resultType=core` is required: the default `lite` response omits
    /// `fullTextUrlList`, which is the whole reason to consult this
    /// source. The DOI is quoted so the query parser treats it as a phrase
    /// rather than tokenising on `.` and `/`, and `query_pairs_mut`
    /// percent-encodes the value so no query syntax escapes from the
    /// suffix.
    fn request_url(&self, doi: &crate::Doi) -> Result<Url, FetchError> {
        let mut url = self
            .base
            .join("/europepmc/webservices/rest/search")
            .map_err(|e| FetchError::SourceSchema {
                hint: format!("europepmc URL construction failed: {e}"),
            })?;
        url.query_pairs_mut()
            .append_pair("query", &format!("DOI:\"{}\"", doi.as_str()))
            .append_pair("format", "json")
            .append_pair("resultType", "core")
            .append_pair("pageSize", "1");
        Ok(url)
    }
}

impl Default for EuropePmcSource {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Source for EuropePmcSource {
    fn name(&self) -> &str {
        "europe-pmc"
    }

    fn can_serve(&self, profile: &CapabilityProfile, ref_: &Ref) -> bool {
        profile.metadata.europe_pmc && matches!(ref_, Ref::Doi(_))
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
                    source_key: "europe-pmc".into(),
                });
            }
        };

        if !profile.metadata.europe_pmc {
            return Err(FetchError::NotEligible {
                source_key: "europe-pmc".into(),
            });
        }

        let _permit = ctx.rate_limiter.acquire(self.name()).await;

        let url = self.request_url(doi)?;
        let (body, final_url) = ctx.http.fetch_bytes(self.name(), url).await?;

        let envelope: serde_json::Value =
            serde_json::from_slice(&body).map_err(|e| FetchError::SourceSchema {
                hint: format!("europepmc returned non-JSON: {e}"),
            })?;

        // Envelope: { hitCount, resultList: { result: [ .. ] } }.
        let results = envelope
            .get("resultList")
            .and_then(|r| r.get("result"))
            .and_then(|r| r.as_array())
            .ok_or_else(|| FetchError::SourceSchema {
                hint: format!(
                    "europepmc response missing `resultList.result` (got: {})",
                    truncate_for_hint(&body)
                ),
            })?;
        let record = results.first().ok_or_else(|| FetchError::NotFound {
            hint: format!("europepmc has no record for {}", doi.as_str()),
        })?;

        // #415: OA subset only, as an explicit refusal rather than a retry.
        if !is_open_access(record) {
            return Err(FetchError::SourceSchema {
                hint: format!(
                    "europepmc record is indexed but not open access \
                     (isOpenAccess = {}, inEPMC = {})",
                    flag(record, "isOpenAccess").unwrap_or("absent"),
                    flag(record, "inEPMC").unwrap_or("absent"),
                ),
            });
        }

        let license = record
            .get("license")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("unknown")
            .to_string();

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

/// Read one of the Europe PMC `"Y"` / `"N"` string flags.
fn flag<'a>(record: &'a serde_json::Value, name: &str) -> Option<&'a str> {
    record.get(name).and_then(|v| v.as_str())
}

/// True only for `isOpenAccess == "Y"`.
///
/// Deliberately **not** `inEPMC`: a record can be in the archive while its
/// full text is subscription-only, so gating on presence rather than
/// openness would return records doiget cannot retrieve. Absent counts as
/// not open.
#[must_use]
pub fn is_open_access(record: &serde_json::Value) -> bool {
    flag(record, "isOpenAccess") == Some("Y")
}

/// The OA PDF URL from `fullTextUrlList`, if the record advertises one.
///
/// Prefers a `documentStyle == "pdf"` entry whose availability code is
/// free/open (`F` or `OA`), and returns `None` rather than a landing page
/// — a landing page is precisely what Unpaywall already gives, and what
/// makes this source worth consulting when it does not.
///
/// The URL is handed to the existing `oa-publisher` fetch leg rather than
/// downloaded here; see the module docs for why that is also the right
/// place for a proxy-blocked download to surface.
#[must_use]
pub fn open_access_pdf_url(record: &serde_json::Value) -> Option<&str> {
    record
        .get("fullTextUrlList")
        .and_then(|l| l.get("fullTextUrl"))
        .and_then(|l| l.as_array())
        .and_then(|entries| {
            entries.iter().find_map(|e| {
                let style = e.get("documentStyle").and_then(|v| v.as_str())?;
                let code = e.get("availabilityCode").and_then(|v| v.as_str())?;
                let url = e.get("url").and_then(|v| v.as_str())?;
                (style.eq_ignore_ascii_case("pdf") && matches!(code, "F" | "OA")).then_some(url)
            })
        })
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

    /// An OA record with a free PDF entry listed AFTER a subscription DOI
    /// entry — the ordering is deliberate, so the extractor has to pick on
    /// the fields rather than take the first URL.
    const SAMPLE_OA: &str = r#"{
        "hitCount": 1,
        "resultList": {"result": [
            {
                "id": "PMC1234567",
                "source": "PMC",
                "doi": "10.1234/open",
                "title": "An Open Access Article",
                "pubYear": "2024",
                "isOpenAccess": "Y",
                "inEPMC": "Y",
                "license": "cc by",
                "fullTextUrlList": {"fullTextUrl": [
                    {"availability": "Subscription required", "availabilityCode": "S",
                     "documentStyle": "doi", "site": "DOI",
                     "url": "https://doi.org/10.1234/open"},
                    {"availability": "Free", "availabilityCode": "F",
                     "documentStyle": "pdf", "site": "Europe_PMC",
                     "url": "https://europepmc.org/articles/PMC1234567?pdf=render"}
                ]}
            }
        ]}
    }"#;

    /// Real trap, observed live on 10.1038/nature12373: the record IS in
    /// the archive (`inEPMC = Y`) but is NOT open access.
    const SAMPLE_IN_EPMC_BUT_CLOSED: &str = r#"{
        "hitCount": 1,
        "resultList": {"result": [
            {
                "id": "23903748",
                "source": "MED",
                "doi": "10.1038/nature12373",
                "title": "Nanometre-scale thermometry in a living cell.",
                "pubYear": "2013",
                "isOpenAccess": "N",
                "inEPMC": "Y",
                "inPMC": "Y"
            }
        ]}
    }"#;

    const SAMPLE_EMPTY: &str = r#"{"hitCount": 0, "resultList": {"result": []}}"#;

    fn build_test_context(wiremock_host: &str) -> (TempDir, FetchContext) {
        let td = TempDir::new().expect("tempdir");
        let log_dir =
            Utf8PathBuf::try_from(td.path().to_path_buf()).expect("temp dir path must be UTF-8");
        let http = Arc::new(HttpClient::new_for_tests_allow_http(
            "europe-pmc",
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

    fn profile(europe_pmc: bool) -> CapabilityProfile {
        let mut p = CapabilityProfile::from_env().expect("clean env never errors");
        p.metadata = MetadataAccess {
            openalex: false,
            semantic_scholar: false,
            doaj: false,
            datacite: false,
            hal: false,
            openaire: false,
            core: false,
            europe_pmc,
        };
        p
    }

    /// `resultType=core` is not cosmetic: the default `lite` response omits
    /// `fullTextUrlList`, which is the only reason to consult this source.
    #[test]
    fn request_url_asks_for_the_core_result_type() {
        let src = EuropePmcSource::new();
        let doi = Doi::parse("10.1234/open").expect("valid doi");
        let url = src.request_url(&doi).expect("url builds");
        assert_eq!(url.path(), "/europepmc/webservices/rest/search");
        let pairs: Vec<(String, String)> = url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert!(pairs.contains(&("resultType".into(), "core".into())));
        assert!(pairs.contains(&("format".into(), "json".into())));
        assert!(pairs.contains(&("query".into(), "DOI:\"10.1234/open\"".into())));
    }

    #[tokio::test]
    async fn fetch_returns_an_open_access_record() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/europepmc/webservices/rest/search"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_OA))
            .mount(&server)
            .await;

        let (_td, ctx) = build_test_context(&server.address().to_string());
        let src = EuropePmcSource::with_base(Url::parse(&server.uri()).expect("base"));
        let ref_ = Ref::Doi(Doi::parse("10.1234/open").expect("doi"));

        let got = src
            .fetch(&ref_, &profile(true), &ctx)
            .await
            .expect("fetch succeeds");
        assert_eq!(got.source, "europe-pmc");
        assert_eq!(got.license, "cc by");
        assert!(got.pdf_bytes.is_none(), "metadata-only contract");
        let rec = got.metadata_json.expect("record");
        assert_eq!(
            open_access_pdf_url(&rec),
            Some("https://europepmc.org/articles/PMC1234567?pdf=render"),
            "must pick the free PDF entry, not the first URL in the list"
        );
    }

    /// The trap this gate exists for. `inEPMC = Y` means the record is in
    /// the archive; it says nothing about whether the full text is
    /// readable. Gating on presence instead of openness would hand back
    /// records doiget cannot retrieve.
    #[tokio::test]
    async fn in_epmc_but_not_open_access_is_refused() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/europepmc/webservices/rest/search"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_IN_EPMC_BUT_CLOSED))
            .mount(&server)
            .await;

        let (_td, ctx) = build_test_context(&server.address().to_string());
        let src = EuropePmcSource::with_base(Url::parse(&server.uri()).expect("base"));
        let ref_ = Ref::Doi(Doi::parse("10.1038/nature12373").expect("doi"));
        let err = src
            .fetch(&ref_, &profile(true), &ctx)
            .await
            .expect_err("a non-OA record must be refused, not returned");
        let msg = err.to_string();
        assert!(
            matches!(err, FetchError::SourceSchema { .. }),
            "got {err:?}"
        );
        assert!(
            msg.contains("isOpenAccess = N") && msg.contains("inEPMC = Y"),
            "the refusal must say WHY, naming both flags; got: {msg}"
        );
    }

    #[test]
    fn open_access_is_judged_on_isopenaccess_not_inepmc() {
        assert!(is_open_access(&serde_json::json!({"isOpenAccess": "Y"})));
        assert!(!is_open_access(
            &serde_json::json!({"isOpenAccess": "N", "inEPMC": "Y"})
        ));
        assert!(
            !is_open_access(&serde_json::json!({"inEPMC": "Y"})),
            "presence in the archive is not openness"
        );
        assert!(
            !is_open_access(&serde_json::json!({})),
            "absent is not open"
        );
    }

    /// A landing page is what Unpaywall already gives; returning one here
    /// would defeat the point of consulting this source.
    #[test]
    fn pdf_url_extraction_skips_landing_pages_and_subscription_entries() {
        let only_landing = serde_json::json!({"fullTextUrlList": {"fullTextUrl": [
            {"documentStyle": "html", "availabilityCode": "F", "url": "https://x/landing"}
        ]}});
        assert_eq!(open_access_pdf_url(&only_landing), None);

        let paywalled_pdf = serde_json::json!({"fullTextUrlList": {"fullTextUrl": [
            {"documentStyle": "pdf", "availabilityCode": "S", "url": "https://x/paid.pdf"}
        ]}});
        assert_eq!(
            open_access_pdf_url(&paywalled_pdf),
            None,
            "a subscription PDF is not an OA PDF"
        );

        assert_eq!(open_access_pdf_url(&serde_json::json!({})), None);
    }

    #[tokio::test]
    async fn no_record_surfaces_as_not_found() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/europepmc/webservices/rest/search"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_EMPTY))
            .mount(&server)
            .await;

        let (_td, ctx) = build_test_context(&server.address().to_string());
        let src = EuropePmcSource::with_base(Url::parse(&server.uri()).expect("base"));
        let ref_ = Ref::Doi(Doi::parse("10.1234/absent").expect("doi"));
        let err = src
            .fetch(&ref_, &profile(true), &ctx)
            .await
            .expect_err("must not claim success");
        assert!(matches!(err, FetchError::NotFound { .. }), "got {err:?}");
    }

    /// The regression #413 requires of every new source.
    #[tokio::test]
    async fn is_inert_when_the_runtime_flag_is_unset() {
        let server = MockServer::start().await;
        let (_td, ctx) = build_test_context(&server.address().to_string());
        let src = EuropePmcSource::with_base(Url::parse(&server.uri()).expect("base"));
        let ref_ = Ref::Doi(Doi::parse("10.1234/open").expect("doi"));

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
        let src = EuropePmcSource::with_base(Url::parse(&server.uri()).expect("base"));
        let ref_ = Ref::Arxiv(crate::ArxivId::parse("2401.12345").expect("arxiv id"));
        assert!(!src.can_serve(&profile(true), &ref_));
        assert!(matches!(
            src.fetch(&ref_, &profile(true), &ctx).await,
            Err(FetchError::NotEligible { .. })
        ));
    }
}
