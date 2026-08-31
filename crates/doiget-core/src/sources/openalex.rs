//! OpenAlex source — DOI metadata enrichment (Phase 4 / Tier 2).
//!
//! Spec: `docs/SOURCES.md` §1 Tier 2 row + §4 "OpenAlex / Semantic
//! Scholar / DOAJ". OpenAlex is a free, no-auth metadata API. Polite
//! pool is opted into via a `mailto` query parameter; the contact
//! email is supplied through [`OpenalexSource::new`] (same channel
//! Crossref uses).
//!
//! ## Capability gate
//!
//! [`OpenalexSource::can_serve`] returns `true` only when
//! [`CapabilityProfile.metadata.openalex`](crate::CapabilityProfile)
//! is `true` AND the ref is a [`Ref::Doi`]. The metadata bool is set
//! by [`CapabilityProfile::from_env`] from the
//! `DOIGET_ENABLE_OPENALEX` environment variable (presence-checked),
//! and only when the `metadata` Cargo feature is compiled in
//! (`docs/CAPABILITY.md` §2).
//!
//! ## Metadata-only contract
//!
//! Per `docs/SOURCES.md` §4, this source is **metadata-only**.
//! [`OpenalexSource::fetch`] never returns PDF bytes
//! (`FetchResult.pdf_bytes` is always `None`). The citation graph
//! orchestrator (Slice 14) consumes the `referenced_works` array
//! from the response metadata to expand the graph; that array lists
//! OpenAlex Work IDs, not DOIs, so the orchestrator does its own ID
//! resolution.

use async_trait::async_trait;
use url::Url;

use crate::provenance::{Capability, LogEvent, LogResult, RowInput};
use crate::source::{FetchContext, FetchError, FetchResult, Source};
use crate::{CapabilityProfile, Ref};

/// Production OpenAlex REST API base URL.
///
/// Hard-coded per `docs/SOURCES.md` §1 Tier-2 row. Tests inject a
/// wiremock origin via [`OpenalexSource::with_base`], identical to
/// the pattern used by `CrossrefSource`.
const DEFAULT_BASE: &str = "https://api.openalex.org";

/// OpenAlex [`Source`] impl — DOI → enriched bibliographic metadata.
///
/// See module docs for the capability-gate and metadata-only contract.
#[derive(Clone, Debug)]
pub struct OpenalexSource {
    /// API base URL. Production constructor pins this to
    /// `https://api.openalex.org`; the [`with_base`](Self::with_base)
    /// test-only constructor lets wiremock substitute an
    /// `http://127.0.0.1:N` origin.
    base: Url,
    /// Polite-pool contact email per `docs/SOURCES.md` §6. OpenAlex
    /// accepts this as a `?mailto=<email>` query parameter; doiget
    /// uses the query-parameter route so callers reading raw URLs in
    /// the provenance log can see the polite-pool opt-in directly.
    contact_email: String,
}

impl OpenalexSource {
    /// Production constructor: hard-codes `https://api.openalex.org`
    /// as the base URL.
    #[must_use]
    pub fn new(contact_email: String) -> Self {
        Self {
            #[allow(clippy::expect_used)]
            base: Url::parse(DEFAULT_BASE).expect("hard-coded base URL is valid"),
            contact_email,
        }
    }

    /// Construct with an arbitrary base URL.
    ///
    /// The orchestrator uses this to honor the `DOIGET_OPENALEX_BASE`
    /// env var (Slice 11+ wiring), which lets integration tests point
    /// the source at a wiremock origin without compile-time gates.
    /// Production callers use [`OpenalexSource::new`].
    pub fn with_base(base: Url, contact_email: String) -> Self {
        Self {
            base,
            contact_email,
        }
    }

    /// Build the `/works/doi:{doi}?mailto=<contact>` URL.
    ///
    /// OpenAlex's `/works/{id}` endpoint does **not** accept a bare DOI
    /// in the path — `GET /works/10.1103/PhysRevLett.102.190601` returns
    /// HTTP 404. It accepts an OpenAlex Work ID (`W…`), the namespaced
    /// `doi:<doi>` form, or a full `https://doi.org/<doi>` URL. We use
    /// the `doi:` prefix: it is unambiguous, needs no percent-encoding,
    /// and (unlike the `https://doi.org/` form) does not confuse
    /// `Url::join`'s scheme detection. The `mailto` query parameter opts
    /// into the polite pool per `docs/SOURCES.md` §6; when
    /// `contact_email` is empty the query parameter is omitted.
    fn request_url(&self, doi: &crate::Doi) -> Result<Url, FetchError> {
        let path = format!("/works/doi:{}", doi.as_str());
        let mut url = self
            .base
            .join(&path)
            .map_err(|e| FetchError::SourceSchema {
                hint: format!("openalex URL construction failed: {e}"),
            })?;
        if !self.contact_email.is_empty() {
            url.query_pairs_mut()
                .append_pair("mailto", &self.contact_email);
        }
        Ok(url)
    }
}

#[async_trait]
impl Source for OpenalexSource {
    fn name(&self) -> &str {
        "openalex"
    }

    fn can_serve(&self, profile: &CapabilityProfile, ref_: &Ref) -> bool {
        // Gated by both the runtime capability flag AND the ref kind.
        // arXiv ids are not OpenAlex Work IDs and OpenAlex's
        // `/works/<id>` endpoint expects either a DOI or an OpenAlex
        // Work ID; we only accept DOI here.
        profile.metadata.openalex && matches!(ref_, Ref::Doi(_))
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
                    source_key: "openalex".into(),
                });
            }
        };

        // Defense-in-depth capability gate — the orchestrator should
        // have called `can_serve` first, but the source enforces too.
        if !profile.metadata.openalex {
            return Err(FetchError::NotEligible {
                source_key: "openalex".into(),
            });
        }

        // Step 1: rate limiter (politeness — `docs/SOURCES.md` §6).
        let _permit = ctx.rate_limiter.acquire(self.name()).await;

        // Step 2: HTTP fetch. Body is JSON; OpenAlex Work records are
        // tens of KB even for highly-cited papers, well under the
        // `PDF_MAX_BYTES` cap.
        let url = self.request_url(doi)?;
        let (body, final_url) = ctx.http.fetch_bytes(self.name(), url).await?;

        // Step 3: parse the response. OpenAlex returns the Work
        // record directly at the top level (no envelope, unlike
        // Crossref).
        let work: serde_json::Value =
            serde_json::from_slice(&body).map_err(|e| FetchError::SourceSchema {
                hint: format!("openalex returned non-JSON: {e}"),
            })?;

        // Defensive shape check — every real Work record has an
        // `id` field. An error payload has an `error` field instead
        // (and no `id`). Use missing `id` as the "not a Work record"
        // signal.
        if work.get("id").is_none() {
            return Err(FetchError::SourceSchema {
                hint: format!(
                    "openalex response missing `id` field — likely an error \
                     payload (got: {})",
                    truncate_for_hint(&body)
                ),
            });
        }

        // Step 4: provenance row. Tier 2 sources emit under
        // `Capability::Metadata` per `docs/PROVENANCE_LOG.md` §3.
        // ADR-0021 §1 canonical-digest: promote the ref under the
        // "openalex" resolver profile.
        let canonical = ref_.promote(self.name(), None).digest_hex();
        ctx.log.append(RowInput {
            event: LogEvent::Fetch,
            result: LogResult::Ok,
            capability: Capability::Metadata,
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
            // OpenAlex Work records carry a `best_oa_location.license`
            // field but a missing-or-null value is the common case
            // for non-OA works. Surface it via `metadata_json` and let
            // the orchestrator decide; report a neutral marker here.
            license: "unknown".into(),
            // Metadata-only contract (docs/SOURCES.md §4).
            pdf_bytes: None,
            final_url: Some(final_url),
            metadata_json: Some(work),
        })
    }
}

/// Truncate a response body to a short prefix for inclusion in error
/// hints. Avoids dumping a multi-KB payload into a single log line
/// when the response is malformed; 200 chars is enough to identify the
/// shape (HTML 404 page vs. JSON error envelope vs. truncated work).
/// Extract an open-access PDF URL from an OpenAlex Work record.
///
/// #461. OpenAlex carries a `locations[]` array -- every place it knows the
/// work exists -- where Unpaywall's `best_oa_location` is one. For a
/// hybrid-OA article whose publisher leg is refused, the copy that satisfies
/// the fetch is often an institutional repository sitting in that array and
/// in no curated list.
///
/// Returns the first entry with `is_oa == true` and a non-empty `pdf_url`.
/// `landing_page_url` is deliberately NOT used: it is a page about the paper,
/// not the paper, and `try_fetch_oa_pdf` would reject the HTML as `NotAPdf`
/// after having made the request.
///
/// Like the CORE / HAL / Europe PMC accessors, this only SURFACES a
/// candidate. The fetch is still performed by the `oa-publisher` leg, under
/// that leg's allowlist and its ADR-0023 denial context -- so a repository
/// host the user has not trusted is refused exactly as it is today, and the
/// access ceiling in `LEGAL.md` §2a is unchanged: this is still case (a), a
/// location an enabled source reported.
///
/// Known cost: `locations[0]` is usually the same location Unpaywall already
/// offered and the chain already failed on, so the first candidate is often a
/// retry of a request that was just refused. The three existing accessors
/// share the property. Fixing it means telling the accessor which URL already
/// failed, which none of them can see.
#[must_use]
pub fn open_access_pdf_url(record: &serde_json::Value) -> Option<&str> {
    record
        .get("locations")
        .and_then(serde_json::Value::as_array)?
        .iter()
        .find_map(|loc| {
            if loc.get("is_oa").and_then(serde_json::Value::as_bool) != Some(true) {
                return None;
            }
            loc.get("pdf_url")
                .and_then(serde_json::Value::as_str)
                .filter(|u| !u.is_empty())
        })
}

/// What OpenAlex actually said about where this work lives, for the case
/// where [`open_access_pdf_url`] found nothing usable (#547).
///
/// That function needs `is_oa == true` AND a non-empty `pdf_url`, and a
/// location can fail both while still being a real repository copy. The
/// reported DOI has two locations, the second of which IS the institutional
/// deposit -- but its `landing_page_url` is an author-listing page
/// (`/view/author/70486.html`), not an item, and `is_oa` is `false`. So the
/// extractor correctly returns `None`, the run reports "no OA PDF available",
/// and the fact that a repository was NAMED goes nowhere.
///
/// "OpenAlex named 1 repository location; its URL is not an item page" points
/// the reader at the repository. "no OA PDF available" points them at giving
/// up. Returns `None` when there is nothing to say.
#[must_use]
pub fn describe_locations(record: &serde_json::Value) -> Option<String> {
    let locations = record
        .get("locations")
        .and_then(serde_json::Value::as_array)?;
    if locations.is_empty() {
        return None;
    }

    let mut oa = 0usize;
    let mut with_pdf = 0usize;
    let mut named: Vec<&str> = Vec::new();
    for loc in locations {
        if loc.get("is_oa").and_then(serde_json::Value::as_bool) == Some(true) {
            oa += 1;
        }
        if loc
            .get("pdf_url")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|u| !u.is_empty())
        {
            with_pdf += 1;
        }
        if let Some(host) = loc
            .get("source")
            .and_then(|s| s.get("display_name"))
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
        {
            if !named.contains(&host) {
                named.push(host);
            }
        }
    }

    let hosts = if named.is_empty() {
        String::new()
    } else {
        format!(" ({})", named.join("; "))
    };
    Some(format!(
        "openalex named {} location(s){hosts}: {oa} flagged open access,          {with_pdf} with a PDF URL. A location without a PDF URL may still be          a real deposit whose landing page is not an item page",
        locations.len()
    ))
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

    /// #547, with the shape the report measured for `10.1109/tsp.2023.3269664`.
    ///
    /// Two locations. The second IS the Strathprints deposit -- and it has no
    /// `pdf_url`, `is_oa: false`, and a `landing_page_url` pointing at an
    /// author-listing page rather than an item. `open_access_pdf_url`
    /// correctly returns `None`; the run then reported `no OA PDF available`,
    /// which is a different claim from what OpenAlex actually said.
    #[test]
    fn a_named_but_unusable_location_is_described_rather_than_dropped() {
        let record = serde_json::json!({
            "locations": [
                {
                    "is_oa": false,
                    "pdf_url": serde_json::Value::Null,
                    "landing_page_url": "https://doi.org/10.1109/tsp.2023.3269664",
                    "source": { "display_name": "IEEE Transactions on Signal Processing" }
                },
                {
                    "is_oa": false,
                    "pdf_url": serde_json::Value::Null,
                    "landing_page_url": "https://strathprints.strath.ac.uk/view/author/70486.html",
                    "source": { "display_name": "Strathprints: The University of Strathclyde" }
                }
            ]
        });

        assert!(
            open_access_pdf_url(&record).is_none(),
            "premise: the extractor still finds nothing followable"
        );

        let d = describe_locations(&record).expect("locations were named");
        assert!(d.contains("2 location"), "says how many: {d}");
        assert!(
            d.contains("Strathprints"),
            "NAMES the repository, which is what the reader can act on: {d}"
        );
        assert!(
            d.contains("0 with a PDF URL"),
            "and why none was followed: {d}"
        );
    }

    /// The control from the report: a proper item PDF URL at the same
    /// repository still resolves, so this describes a gap rather than
    /// papering over one.
    #[test]
    fn a_usable_location_still_resolves_and_needs_no_description() {
        let record = serde_json::json!({
            "locations": [{
                "is_oa": true,
                "pdf_url": "https://strathprints.strath.ac.uk/91130/7/Khattak-etal.pdf",
                "source": { "display_name": "Strathprints: The University of Strathclyde" }
            }]
        });
        assert_eq!(
            open_access_pdf_url(&record),
            Some("https://strathprints.strath.ac.uk/91130/7/Khattak-etal.pdf")
        );
    }

    /// Nothing to say when nothing was named.
    #[test]
    fn no_locations_means_no_description() {
        assert!(describe_locations(&serde_json::json!({})).is_none());
        assert!(describe_locations(&serde_json::json!({"locations": []})).is_none());
    }
    use super::*;

    use std::sync::Arc;

    use camino::Utf8PathBuf;
    use tempfile::TempDir;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::http::HttpClient;
    use crate::provenance::ProvenanceLog;
    use crate::rate_limiter::RateLimiter;
    use crate::{ArxivId, CapabilityProfile, Doi, MetadataAccess, RateLimits, Ref};

    /// Hand-crafted (not a snapshot) OpenAlex Work record. Kept small
    /// and synthetic to avoid third-party redistribution concerns.
    const SAMPLE_WORK: &str = r#"{
        "id": "https://openalex.org/W2741809807",
        "doi": "https://doi.org/10.1234/example",
        "display_name": "Example Work Title",
        "publication_year": 2024,
        "referenced_works": [
            "https://openalex.org/W2000000001",
            "https://openalex.org/W2000000002"
        ]
    }"#;

    fn build_test_context(wiremock_host: &str) -> (TempDir, FetchContext) {
        let td = TempDir::new().expect("tempdir");
        let log_dir =
            Utf8PathBuf::try_from(td.path().to_path_buf()).expect("temp dir path must be UTF-8");
        let log_path = log_dir.join("test.jsonl");

        let http = Arc::new(HttpClient::new_for_tests_allow_http(
            "openalex",
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

    fn profile_with_openalex_enabled() -> CapabilityProfile {
        // Build a clean profile, then flip the openalex flag. We avoid
        // touching the real env vars so the test runs single-threaded
        // without `serial_test`.
        let mut p = CapabilityProfile::for_tests();
        p.metadata = MetadataAccess {
            openalex: true,
            semantic_scholar: false,
            doaj: false,
            datacite: false,
            hal: false,
            openaire: false,
            core: false,
            europe_pmc: false,
        };
        p
    }

    #[tokio::test]
    async fn fetch_doi_returns_work_metadata() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/works/doi:10.1234/example"))
            .and(query_param("mailto", "doiget@localhost"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_WORK))
            .mount(&server)
            .await;

        let (_td, ctx) = build_test_context(&server.uri());
        let src = OpenalexSource::with_base(
            Url::parse(&server.uri()).expect("wiremock URI parses"),
            "doiget@localhost".to_string(),
        );
        let profile = profile_with_openalex_enabled();
        let ref_ = Ref::Doi(Doi::parse("10.1234/example").expect("DOI parses"));

        let result = src.fetch(&ref_, &profile, &ctx).await.expect("fetch ok");
        assert_eq!(result.source, "openalex");
        assert!(result.pdf_bytes.is_none(), "metadata-only contract");
        let meta = result.metadata_json.expect("metadata_json present");
        assert_eq!(meta["display_name"], "Example Work Title");
        assert_eq!(
            meta["referenced_works"][0],
            "https://openalex.org/W2000000001"
        );
    }

    #[tokio::test]
    async fn fetch_arxiv_id_is_not_eligible() {
        let (_td, ctx) = build_test_context("http://127.0.0.1:1");
        let src = OpenalexSource::with_base(
            Url::parse("http://127.0.0.1:1").expect("URI parses"),
            "doiget@localhost".to_string(),
        );
        let profile = profile_with_openalex_enabled();
        let ref_ = Ref::Arxiv(ArxivId::parse("2401.12345").expect("arXiv id parses"));

        let err = src
            .fetch(&ref_, &profile, &ctx)
            .await
            .expect_err("arXiv ref must be rejected");
        assert!(matches!(err, FetchError::NotEligible { .. }));
    }

    #[tokio::test]
    async fn fetch_without_capability_flag_is_not_eligible() {
        let (_td, ctx) = build_test_context("http://127.0.0.1:1");
        let src = OpenalexSource::with_base(
            Url::parse("http://127.0.0.1:1").expect("URI parses"),
            "doiget@localhost".to_string(),
        );
        // Profile with metadata.openalex == false (default).
        let profile = CapabilityProfile::for_tests();
        let ref_ = Ref::Doi(Doi::parse("10.1234/example").expect("DOI parses"));

        assert!(
            !src.can_serve(&profile, &ref_),
            "can_serve must be false without DOIGET_ENABLE_OPENALEX"
        );
        let err = src
            .fetch(&ref_, &profile, &ctx)
            .await
            .expect_err("fetch must reject when capability is denied");
        assert!(matches!(err, FetchError::NotEligible { .. }));
    }

    #[tokio::test]
    async fn fetch_malformed_response_returns_source_schema_error() {
        let server = MockServer::start().await;
        // Response has no `id` field — defensive shape check trips.
        Mock::given(method("GET"))
            .and(path("/works/doi:10.1234/example"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"error":"not found"}"#))
            .mount(&server)
            .await;

        let (_td, ctx) = build_test_context(&server.uri());
        let src = OpenalexSource::with_base(
            Url::parse(&server.uri()).expect("wiremock URI parses"),
            "doiget@localhost".to_string(),
        );
        let profile = profile_with_openalex_enabled();
        let ref_ = Ref::Doi(Doi::parse("10.1234/example").expect("DOI parses"));

        let err = src
            .fetch(&ref_, &profile, &ctx)
            .await
            .expect_err("missing `id` must surface as SourceSchema");
        assert!(matches!(err, FetchError::SourceSchema { .. }));
    }

    // ---- #461: locations[] as a candidate source of OA copies ----------

    /// The point of the accessor: OpenAlex reports EVERY location, so the
    /// repository copy that satisfies a refused hybrid-OA article is in the
    /// array even when it is not the primary one.
    #[test]
    fn open_access_pdf_url_finds_a_repository_copy_past_the_primary() {
        let work = serde_json::json!({
            "locations": [
                // The publisher's own landing page: not OA, and no pdf_url.
                // This is the one that just refused us.
                { "is_oa": false, "landing_page_url": "https://ieeexplore.example/doc/1" },
                // An institutional repository, which is the interesting case.
                { "is_oa": true, "pdf_url": "https://repo.example.ac.uk/1/paper.pdf" }
            ]
        });
        assert_eq!(
            open_access_pdf_url(&work),
            Some("https://repo.example.ac.uk/1/paper.pdf")
        );
    }

    /// A location that is open but records only a landing page is skipped.
    /// Returning it would spend a request to be told the HTML is `NotAPdf`.
    #[test]
    fn open_access_pdf_url_skips_a_landing_page_only_location() {
        let work = serde_json::json!({
            "locations": [
                { "is_oa": true, "landing_page_url": "https://repo.example.ac.uk/1" },
                { "is_oa": true, "pdf_url": "https://other.example.ac.uk/2/paper.pdf" }
            ]
        });
        assert_eq!(
            open_access_pdf_url(&work),
            Some("https://other.example.ac.uk/2/paper.pdf")
        );
    }

    /// `is_oa: false` is not a candidate even with a `pdf_url`. Offering it
    /// would send doiget at a copy the index itself says is not open.
    #[test]
    fn open_access_pdf_url_ignores_a_non_oa_location() {
        let work = serde_json::json!({
            "locations": [
                { "is_oa": false, "pdf_url": "https://paywall.example/1.pdf" }
            ]
        });
        assert_eq!(open_access_pdf_url(&work), None);
    }

    /// An empty string is not a URL. Absent `locations` is not an error.
    #[test]
    fn open_access_pdf_url_rejects_empty_and_absent() {
        let empty = serde_json::json!({
            "locations": [{ "is_oa": true, "pdf_url": "" }]
        });
        assert_eq!(open_access_pdf_url(&empty), None);
        assert_eq!(open_access_pdf_url(&serde_json::json!({})), None);
    }
}
