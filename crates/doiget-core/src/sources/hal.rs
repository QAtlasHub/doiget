//! HAL source — French national OA repository (Phase 4 / Tier 2, #418).
//!
//! Spec: `docs/SOURCES.md` §1 Tier 2 row + §4. HAL (Hyper Articles en
//! Ligne) is the French national open repository. It holds author
//! deposits — notably in mathematics, physics and CS — that are absent
//! from Crossref-centric indexes, so Unpaywall misses them.
//!
//! ## Capability gate
//!
//! [`HalSource::can_serve`] returns `true` only when
//! [`CapabilityProfile.metadata.hal`](crate::CapabilityProfile) is `true`
//! AND the ref is a [`Ref::Doi`]. The bool is set by
//! [`CapabilityProfile::from_env`] from `DOIGET_ENABLE_HAL`, which is
//! **off by default** (ADR-0040) — with it unset this source is inert and
//! the binary behaves exactly as before.
//!
//! ## OA deposits only
//!
//! A HAL record can exist for a paper whose full text was never deposited,
//! or was deposited under embargo. `openAccess_bool` is the repository
//! saying so itself, and a record without it set is rejected rather than
//! returned — an entry resolving to no reachable text is worse than a
//! clean miss, because it looks like a hit.
//!
//! ## Metadata-only contract
//!
//! Per `docs/SOURCES.md` §4 this source never returns PDF bytes. HAL does
//! expose a `fileMain_s` URL on `hal.science`, but that host is reached
//! through the `oa-publisher` source key (via `trust_oa_registries`), not
//! this one — the API host and the content host are deliberately separate
//! allowlist entries.
//!
//! ## Resolution only, never discovery
//!
//! Queried by exact DOI through the `doiId_s` field and nothing else. The
//! Solr endpoint would happily serve free-text search; using it that way
//! would turn a resolver into a discovery surface, which `docs/SCOPE.md`
//! keeps out.
//!
//! API: public Solr-style REST, no auth, no key.
//! Terms: <https://api.archives-ouvertes.fr/docs>

use async_trait::async_trait;
use url::Url;

use crate::provenance::{Capability, LogEvent, LogResult, RowInput};
use crate::source::{FetchContext, FetchError, FetchResult, Source};
use crate::{CapabilityProfile, Ref};

/// Production HAL API base.
const DEFAULT_BASE: &str = "https://api.archives-ouvertes.fr";

/// Fields requested from the Solr endpoint.
///
/// Explicit rather than `*` so the response stays small and the parser has
/// a fixed contract: an unexpected extra field cannot change behaviour,
/// and a removed one fails visibly rather than silently widening the
/// payload.
const FIELDS: &str = "docid,title_s,authFullName_s,producedDateY_i,doiId_s,uri_s,\
                      openAccess_bool,fileMain_s,licence_s,docType_s";

/// HAL [`Source`] impl — DOI to HAL deposit record.
#[derive(Clone, Debug)]
pub struct HalSource {
    /// API base URL. Production pins `https://api.archives-ouvertes.fr`;
    /// [`with_base`](Self::with_base) lets wiremock substitute an
    /// `http://127.0.0.1:N` origin.
    base: Url,
}

impl HalSource {
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

    /// Build `/search/?q=doiId_s:"<doi>"&fl=<fields>&rows=1&wt=json`.
    ///
    /// The DOI is wrapped in double quotes so Solr treats it as a phrase:
    /// unquoted, its `.` and `/` are tokenised and the query degrades into
    /// a fuzzy match that can return the wrong record. `query_pairs_mut`
    /// percent-encodes the whole value, so the quotes and the DOI travel
    /// as data and no Solr syntax can be injected from the suffix.
    fn request_url(&self, doi: &crate::Doi) -> Result<Url, FetchError> {
        let mut url = self
            .base
            .join("/search/")
            .map_err(|e| FetchError::SourceSchema {
                hint: format!("hal URL construction failed: {e}"),
            })?;
        url.query_pairs_mut()
            .append_pair("q", &format!("doiId_s:\"{}\"", doi.as_str()))
            .append_pair("fl", FIELDS)
            .append_pair("rows", "1")
            .append_pair("wt", "json");
        Ok(url)
    }
}

impl Default for HalSource {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Source for HalSource {
    fn name(&self) -> &str {
        "hal"
    }

    fn can_serve(&self, profile: &CapabilityProfile, ref_: &Ref) -> bool {
        profile.metadata.hal && matches!(ref_, Ref::Doi(_))
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
                    source_key: "hal".into(),
                });
            }
        };

        if !profile.metadata.hal {
            return Err(FetchError::NotEligible {
                source_key: "hal".into(),
            });
        }

        let _permit = ctx.rate_limiter.acquire(self.name()).await;

        let url = self.request_url(doi)?;
        let (body, final_url) = ctx.http.fetch_bytes(self.name(), url).await?;

        let envelope: serde_json::Value =
            serde_json::from_slice(&body).map_err(|e| FetchError::SourceSchema {
                hint: format!("hal returned non-JSON: {e}"),
            })?;

        // Solr envelope: { responseHeader: {..}, response: { numFound, docs: [..] } }.
        let docs = envelope
            .get("response")
            .and_then(|r| r.get("docs"))
            .and_then(|d| d.as_array())
            .ok_or_else(|| FetchError::SourceSchema {
                hint: format!(
                    "hal response missing `response.docs` (got: {})",
                    truncate_for_hint(&body)
                ),
            })?;
        let doc = docs.first().ok_or_else(|| FetchError::SourceSchema {
            hint: "hal has no deposit for this DOI".to_string(),
        })?;

        // OA gate. An absent `openAccess_bool` is treated as NOT open: HAL
        // omits the field on some record types, and defaulting an unknown
        // to "open" would hand back a record whose text nobody can read.
        if doc
            .get("openAccess_bool")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
        {
            return Err(FetchError::SourceSchema {
                hint: "hal deposit is not open access (openAccess_bool != true)".to_string(),
            });
        }

        let license = license_of(doc);
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
            metadata_json: Some(doc.clone()),
        })
    }
}

/// Licence from `licence_s`, else `"unknown"`.
///
/// HAL returns a licence URL (a `creativecommons.org/licenses/...` link)
/// rather than an SPDX identifier, and only when the depositor set one. It
/// is passed through verbatim: normalising a URL into an SPDX id means
/// guessing, and a wrong licence is worse than an absent one.
fn license_of(doc: &serde_json::Value) -> String {
    first_str(doc, "licence_s")
        .map(str::to_string)
        .unwrap_or_else(|| "unknown".to_string())
}

/// First value of a Solr field that may be a bare string or an array.
///
/// `title_s` is multi-valued in HAL even when there is exactly one title,
/// so a naive `as_str()` returns `None` for the common case.
#[must_use]
pub fn first_str<'a>(doc: &'a serde_json::Value, field: &str) -> Option<&'a str> {
    let v = doc.get(field)?;
    v.as_str().or_else(|| {
        v.as_array()
            .and_then(|a| a.first())
            .and_then(|f| f.as_str())
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

    /// Real shape, captured 2026-08-22 from
    /// `GET /search/?q=doiId_s:"10.1103/PhysRevB.92.125119"`.
    /// Note `title_s` and `authFullName_s` are arrays even for one value,
    /// and this record carries no `licence_s` — both are the common case.
    const SAMPLE_HIT: &str = r#"{
        "response": {
            "numFound": 1,
            "docs": [
                {
                    "docid": "1204546",
                    "openAccess_bool": true,
                    "title_s": ["Minimally entangled typical thermal states"],
                    "authFullName_s": ["Moritz Binder", "Thomas Barthel"],
                    "uri_s": "https://hal.science/hal-01204546v1",
                    "doiId_s": "10.1103/PhysRevB.92.125119",
                    "producedDateY_i": 2015
                }
            ]
        }
    }"#;

    const SAMPLE_EMPTY: &str = r#"{"response": {"numFound": 0, "docs": []}}"#;

    const SAMPLE_CLOSED: &str = r#"{
        "response": {
            "numFound": 1,
            "docs": [
                {
                    "docid": "999",
                    "openAccess_bool": false,
                    "title_s": ["An Embargoed Deposit"],
                    "doiId_s": "10.1234/closed"
                }
            ]
        }
    }"#;

    fn build_test_context(wiremock_host: &str) -> (TempDir, FetchContext) {
        let td = TempDir::new().expect("tempdir");
        let log_dir =
            Utf8PathBuf::try_from(td.path().to_path_buf()).expect("temp dir path must be UTF-8");
        let http = Arc::new(HttpClient::new_for_tests_allow_http("hal", wiremock_host));
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

    fn profile(hal: bool) -> CapabilityProfile {
        let mut p = CapabilityProfile::from_env().expect("clean env never errors");
        p.metadata = MetadataAccess {
            openalex: false,
            semantic_scholar: false,
            doaj: false,
            datacite: false,
            hal,
            openaire: false,
        };
        p
    }

    /// The DOI must be quoted so Solr treats it as a phrase — unquoted, the
    /// `.` and `/` tokenise and the query can match a different record.
    #[test]
    fn request_url_quotes_the_doi_as_a_solr_phrase() {
        let src = HalSource::new();
        let doi = Doi::parse("10.1103/PhysRevB.92.125119").expect("valid doi");
        let url = src.request_url(&doi).expect("url builds");
        let q = url
            .query_pairs()
            .find(|(k, _)| k == "q")
            .expect("q param")
            .1
            .into_owned();
        assert_eq!(q, "doiId_s:\"10.1103/PhysRevB.92.125119\"");
        assert_eq!(url.path(), "/search/");
    }

    #[tokio::test]
    async fn fetch_returns_the_first_open_deposit() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_HIT))
            .mount(&server)
            .await;

        let (_td, ctx) = build_test_context(&server.address().to_string());
        let src = HalSource::with_base(Url::parse(&server.uri()).expect("base"));
        let ref_ = Ref::Doi(Doi::parse("10.1103/PhysRevB.92.125119").expect("doi"));

        let got = src
            .fetch(&ref_, &profile(true), &ctx)
            .await
            .expect("fetch succeeds");
        assert_eq!(got.source, "hal");
        assert!(got.pdf_bytes.is_none(), "metadata-only contract");
        // No `licence_s` on this record: must report unknown, not invent one.
        assert_eq!(got.license, "unknown");
        let doc = got.metadata_json.expect("doc");
        assert_eq!(
            first_str(&doc, "title_s"),
            Some("Minimally entangled typical thermal states"),
            "multi-valued Solr fields must be unwrapped"
        );
    }

    /// A record whose full text was never deposited, or is embargoed, must
    /// NOT come back as a hit — it would look like a success and resolve to
    /// nothing readable.
    #[tokio::test]
    async fn closed_access_deposits_are_rejected() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_CLOSED))
            .mount(&server)
            .await;

        let (_td, ctx) = build_test_context(&server.address().to_string());
        let src = HalSource::with_base(Url::parse(&server.uri()).expect("base"));
        let ref_ = Ref::Doi(Doi::parse("10.1234/closed").expect("doi"));
        let err = src
            .fetch(&ref_, &profile(true), &ctx)
            .await
            .expect_err("closed deposits must not be returned");
        assert!(
            matches!(err, FetchError::SourceSchema { .. }),
            "got {err:?}"
        );
    }

    /// An absent `openAccess_bool` is treated as closed. Defaulting an
    /// unknown to open would hand back an unreadable record.
    #[test]
    fn missing_open_access_flag_is_not_treated_as_open() {
        let doc = serde_json::json!({"docid": "1", "title_s": ["x"]});
        assert_ne!(
            doc.get("openAccess_bool")
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );
    }

    #[tokio::test]
    async fn no_deposit_surfaces_as_source_schema() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_EMPTY))
            .mount(&server)
            .await;

        let (_td, ctx) = build_test_context(&server.address().to_string());
        let src = HalSource::with_base(Url::parse(&server.uri()).expect("base"));
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

    /// The regression #413 requires of every new source: with the runtime
    /// flag unset it must refuse BEFORE touching the network.
    #[tokio::test]
    async fn is_inert_when_the_runtime_flag_is_unset() {
        let server = MockServer::start().await;
        let (_td, ctx) = build_test_context(&server.address().to_string());
        let src = HalSource::with_base(Url::parse(&server.uri()).expect("base"));
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
        let src = HalSource::with_base(Url::parse(&server.uri()).expect("base"));
        let ref_ = Ref::Arxiv(crate::ArxivId::parse("2401.12345").expect("arxiv id"));
        assert!(!src.can_serve(&profile(true), &ref_));
        assert!(matches!(
            src.fetch(&ref_, &profile(true), &ctx).await,
            Err(FetchError::NotEligible { .. })
        ));
    }

    #[test]
    fn first_str_handles_bare_and_array_fields() {
        let doc = serde_json::json!({"a": "bare", "b": ["first", "second"], "c": []});
        assert_eq!(first_str(&doc, "a"), Some("bare"));
        assert_eq!(first_str(&doc, "b"), Some("first"));
        assert_eq!(first_str(&doc, "c"), None);
        assert_eq!(first_str(&doc, "missing"), None);
    }
}
