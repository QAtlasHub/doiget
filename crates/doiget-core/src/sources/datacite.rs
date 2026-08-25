//! DataCite source — DOI **resolution** (Phase 4 / Tier 2, issue #414).
//!
//! Spec: `docs/SOURCES.md` §1 Tier 2 row + §4. DataCite is the second
//! large DOI registration agency; Crossref and Unpaywall index neither
//! its DOIs nor its records. Without this source a live, open-access
//! Zenodo / figshare / Dryad / OSF DOI resolves to
//! [`ErrorCode::NotFound`](crate::ErrorCode::NotFound) — a false negative
//! already documented in-tree on that variant, and observed as a false
//! positive in `doiget-citation-check`.
//!
//! ## Resolution, not enrichment
//!
//! Its siblings (`openalex` / `s2` /
//! `doaj`) add fields to a record Crossref already
//! returned. This one answers the prior question — *does this DOI exist*
//! — for an agency doiget otherwise cannot see. It therefore belongs
//! **after** Crossref in the DOI fan-out: a Crossref-registered DOI never
//! reaches it, so enabling it cannot change any resolution that works
//! today.
//!
//! ## Capability gate
//!
//! `DataCiteSource::can_serve` returns `true` only when
//! [`CapabilityProfile.metadata.datacite`](crate::CapabilityProfile) is
//! `true` AND the ref is a `Ref::Doi`. The bool is set by
//! `CapabilityProfile::from_env` from `DOIGET_ENABLE_DATACITE`, which is
//! **off by default** (ADR-0040) — with it unset the binary behaves
//! exactly as it did before this source existed.
//!
//! ## Metadata-only contract
//!
//! Per `docs/SOURCES.md` §4 this source never returns PDF bytes. DataCite
//! returns a **landing page**, not a file: `attributes.url` for
//! `10.5281/zenodo.X` is an HTML page under `zenodo.org`. Per-repository
//! file retrieval (Zenodo `/api/records/<id>` then `files[].links.self`,
//! figshare, Dryad, …) is deliberately out of scope until we can measure
//! how often the landing URL alone suffices.
//!
//! ## Resolution only, never discovery
//!
//! DataCite is queried by exact DOI and nothing else. Zenodo accepts
//! arbitrary deposits from anyone, so using it as a *search* surface would
//! pull a large volume of unreviewed material into results. The contract
//! here is narrower: resolve a DOI the user explicitly named.
//!
//! API: public REST, no auth, no key.
//! ToS: <https://datacite.org/terms-and-conditions/>

use async_trait::async_trait;
use url::Url;

use crate::provenance::{Capability, LogEvent, LogResult, RowInput};
use crate::source::{FetchContext, FetchError, FetchResult, Source};
use crate::{CapabilityProfile, Ref};

/// Production DataCite REST API base.
const DEFAULT_BASE: &str = "https://api.datacite.org";

/// DataCite [`Source`] impl — DOI to DataCite record.
#[derive(Clone, Debug)]
pub struct DataCiteSource {
    /// API base URL. Production pins `https://api.datacite.org`;
    /// [`with_base`](Self::with_base) lets wiremock substitute an
    /// `http://127.0.0.1:N` origin.
    base: Url,
}

impl DataCiteSource {
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

    /// Build the `/dois/<doi>` URL.
    ///
    /// The DOI goes in the path and its `/` separator must survive as a
    /// literal, so the suffix is pushed as its own segment rather than
    /// percent-encoded into one blob. Every other reserved character IS
    /// encoded by `push`, so a crafted suffix cannot inject a query string
    /// or climb out of the `/dois/` prefix.
    fn request_url(&self, doi: &crate::Doi) -> Result<Url, FetchError> {
        let mut url = self.base.clone();
        {
            let mut segs = url
                .path_segments_mut()
                .map_err(|()| FetchError::SourceSchema {
                    hint: "datacite base URL cannot be a base".to_string(),
                })?;
            segs.clear();
            segs.push("dois");
            for part in doi.as_str().split('/') {
                segs.push(part);
            }
        }
        Ok(url)
    }
}

impl Default for DataCiteSource {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Source for DataCiteSource {
    fn name(&self) -> &str {
        "datacite"
    }

    fn can_serve(&self, profile: &CapabilityProfile, ref_: &Ref) -> bool {
        profile.metadata.datacite && matches!(ref_, Ref::Doi(_))
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
                    source_key: "datacite".into(),
                });
            }
        };

        if !profile.metadata.datacite {
            return Err(FetchError::NotEligible {
                source_key: "datacite".into(),
            });
        }

        let _permit = ctx.rate_limiter.acquire(self.name()).await;

        let url = self.request_url(doi)?;
        let (body, final_url) = ctx.http.fetch_bytes(self.name(), url).await?;

        let envelope: serde_json::Value =
            serde_json::from_slice(&body).map_err(|e| FetchError::SourceSchema {
                hint: format!("datacite returned non-JSON: {e}"),
            })?;

        // JSON:API envelope: { data: { id, type: "dois", attributes: {..} } }.
        let attributes = envelope
            .get("data")
            .and_then(|d| d.get("attributes"))
            .ok_or_else(|| FetchError::SourceSchema {
                hint: format!(
                    "datacite response missing `data.attributes` (got: {})",
                    truncate_for_hint(&body)
                ),
            })?;

        let license = license_of(attributes);
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
            metadata_json: Some(attributes.clone()),
        })
    }
}

/// Extract a licence identifier from `attributes.rightsList[]`.
///
/// DataCite records are heterogeneous — depositors fill `rightsList`
/// freely — so this takes the first entry carrying a `rightsIdentifier`
/// and otherwise reports `"unknown"`. Reporting `unknown` is honest;
/// inferring a licence from a free-text `rights` string would not be, and
/// a wrong licence is worse than an absent one.
fn license_of(attributes: &serde_json::Value) -> String {
    attributes
        .get("rightsList")
        .and_then(|r| r.as_array())
        .and_then(|list| {
            list.iter()
                .find_map(|r| r.get("rightsIdentifier").and_then(|v| v.as_str()))
        })
        .unwrap_or("unknown")
        .to_string()
}

/// `attributes.types.resourceTypeGeneral`, if present.
///
/// Surfaced because a DataCite DOI is very often **not** an article — on
/// Zenodo, dataset plus software plus image outnumber `JournalArticle`. An
/// agent that cannot tell them apart will treat a software release as a
/// paper (#414).
#[must_use]
pub fn resource_type_general(attributes: &serde_json::Value) -> Option<&str> {
    attributes
        .get("types")
        .and_then(|t| t.get("resourceTypeGeneral"))
        .and_then(|v| v.as_str())
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

    /// Trimmed real shape from `GET /dois/10.5281/zenodo.22053902`.
    const SAMPLE_RECORD: &str = r#"{
        "data": {
            "id": "10.5281/zenodo.22053902",
            "type": "dois",
            "attributes": {
                "doi": "10.5281/zenodo.22053902",
                "titles": [{"title": "An Example Deposit"}],
                "creators": [
                    {"name": "Researcher, Alice"},
                    {"givenName": "Bob", "familyName": "Coauthor"}
                ],
                "publicationYear": 2024,
                "publisher": "Zenodo",
                "types": {"resourceTypeGeneral": "JournalArticle"},
                "rightsList": [
                    {"rights": "Creative Commons Attribution 4.0",
                     "rightsIdentifier": "cc-by-4.0"}
                ],
                "url": "https://zenodo.org/doi/10.5281/zenodo.22053902"
            }
        }
    }"#;

    fn build_test_context(wiremock_host: &str) -> (TempDir, FetchContext) {
        let td = TempDir::new().expect("tempdir");
        let log_dir =
            Utf8PathBuf::try_from(td.path().to_path_buf()).expect("temp dir path must be UTF-8");
        let http = Arc::new(HttpClient::new_for_tests_allow_http(
            "datacite",
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

    fn profile(datacite: bool) -> CapabilityProfile {
        let mut p = CapabilityProfile::for_tests();
        p.metadata = MetadataAccess {
            openalex: false,
            semantic_scholar: false,
            doaj: false,
            datacite,
            hal: false,
            openaire: false,
            core: false,
            europe_pmc: false,
        };
        p
    }

    /// The DOI separator `/` must stay structural in the path — DataCite
    /// routes on `/dois/<prefix>/<suffix>`, so percent-encoding it 404s.
    #[test]
    fn request_url_keeps_the_doi_slash_structural() {
        let src = DataCiteSource::new();
        let doi = Doi::parse("10.5281/zenodo.22053902").expect("valid doi");
        let url = src.request_url(&doi).expect("url builds");
        assert_eq!(
            url.as_str(),
            "https://api.datacite.org/dois/10.5281/zenodo.22053902"
        );
    }

    /// Injection into the path is prevented one layer earlier than the URL
    /// builder: `Doi` is a validated newtype, so a suffix carrying `?`, `#`
    /// or a space never becomes a `Doi` at all and `request_url` can never
    /// be handed one. Pinned here because the builder deliberately pushes
    /// the suffix as structural path segments, which would otherwise be the
    /// place to worry.
    #[test]
    fn doi_newtype_rejects_characters_that_could_escape_the_path() {
        for bad in ["10.5281/a?b", "10.5281/a#b", "10.5281/a b"] {
            assert!(
                Doi::parse(bad).is_err(),
                "{bad} must not parse as a DOI, or request_url could see it"
            );
        }
    }

    /// A DOI suffix may itself contain `/`. Every one of them stays
    /// structural, and nothing climbs above `/dois/`.
    #[test]
    fn request_url_handles_a_multi_segment_suffix() {
        let src = DataCiteSource::new();
        let doi = Doi::parse("10.17605/osf.io/ab3cd").expect("valid doi");
        let url = src.request_url(&doi).expect("url builds");
        assert_eq!(
            url.as_str(),
            "https://api.datacite.org/dois/10.17605/osf.io/ab3cd"
        );
        assert!(url.query().is_none());
    }

    #[tokio::test]
    async fn fetch_returns_attributes_and_license() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/dois/10.5281/zenodo.22053902"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_RECORD))
            .mount(&server)
            .await;

        let (_td, ctx) = build_test_context(&server.address().to_string());
        let src = DataCiteSource::with_base(Url::parse(&server.uri()).expect("base"));
        let ref_ = Ref::Doi(Doi::parse("10.5281/zenodo.22053902").expect("doi"));

        let got = src
            .fetch(&ref_, &profile(true), &ctx)
            .await
            .expect("fetch succeeds");
        assert_eq!(got.source, "datacite");
        assert_eq!(got.license, "cc-by-4.0");
        assert!(got.pdf_bytes.is_none(), "metadata-only contract");
        let attrs = got.metadata_json.expect("attributes");
        assert_eq!(resource_type_general(&attrs), Some("JournalArticle"));
    }

    /// The regression #413 requires of every new source: with the runtime
    /// flag unset it must refuse BEFORE touching the network. No route is
    /// mounted either, so a request would also fail loudly.
    #[tokio::test]
    async fn is_inert_when_the_runtime_flag_is_unset() {
        let server = MockServer::start().await;
        let (_td, ctx) = build_test_context(&server.address().to_string());
        let src = DataCiteSource::with_base(Url::parse(&server.uri()).expect("base"));
        let ref_ = Ref::Doi(Doi::parse("10.5281/zenodo.22053902").expect("doi"));

        assert!(
            !src.can_serve(&profile(false), &ref_),
            "can_serve must be false with DOIGET_ENABLE_DATACITE unset"
        );
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
        let src = DataCiteSource::with_base(Url::parse(&server.uri()).expect("base"));
        let ref_ = Ref::Arxiv(crate::ArxivId::parse("2401.12345").expect("arxiv id"));
        assert!(!src.can_serve(&profile(true), &ref_));
        assert!(matches!(
            src.fetch(&ref_, &profile(true), &ctx).await,
            Err(FetchError::NotEligible { .. })
        ));
    }

    /// A DOI DataCite does not hold returns a JSON error envelope with no
    /// `data.attributes`. That must surface as SourceSchema so the
    /// orchestrator falls through rather than aborting the fetch.
    #[tokio::test]
    async fn missing_record_surfaces_as_source_schema() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/dois/10.1234/not-datacite"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(r#"{"errors":[{"status":"404"}]}"#),
            )
            .mount(&server)
            .await;

        let (_td, ctx) = build_test_context(&server.address().to_string());
        let src = DataCiteSource::with_base(Url::parse(&server.uri()).expect("base"));
        let ref_ = Ref::Doi(Doi::parse("10.1234/not-datacite").expect("doi"));
        let err = src
            .fetch(&ref_, &profile(true), &ctx)
            .await
            .expect_err("must not claim success");
        assert!(
            matches!(err, FetchError::SourceSchema { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn license_falls_back_to_unknown_rather_than_guessing() {
        // Free-text `rights` with no identifier must NOT become a licence:
        // a wrong licence is worse than an absent one.
        let attrs = serde_json::json!({"rightsList": [{"rights": "Open Access"}]});
        assert_eq!(license_of(&attrs), "unknown");
        assert_eq!(license_of(&serde_json::json!({})), "unknown");
    }
}
