//! Crossref source — DOI metadata + OA URL discovery via `link[]` array.
//!
//! Spec: docs/SOURCES.md §4 (Crossref). No auth; polite-pool User-Agent
//! contact email is REQUIRED — see [`CrossrefSource::new`].

use std::collections::HashSet;

use async_trait::async_trait;
use serde::Deserialize;
use url::Url;

use crate::provenance::{Capability, LogEvent, LogResult, RowInput};
use crate::source::{FetchContext, FetchError, FetchResult, Source};
use crate::{CapabilityProfile, Ref};

/// Production Crossref REST API base URL. Hard-coded per `docs/SOURCES.md`
/// §4; tests inject a wiremock origin via [`CrossrefSource::with_base`].
const DEFAULT_BASE: &str = "https://api.crossref.org";

/// Minimum token overlap similarity score to include a candidate in
/// [`CrossrefSource::resolve_citation`] results.
const MIN_CITATION_SCORE: f64 = 0.5;

/// Crossref [`Source`] impl — DOI → metadata; OA URL via `message.link[]`.
///
/// See `docs/SOURCES.md` §4 for the access policy (no auth, polite pool).
#[derive(Clone, Debug)]
pub struct CrossrefSource {
    /// API base URL. Production constructor pins this to
    /// `https://api.crossref.org`; the [`with_base`](Self::with_base)
    /// test-only constructor lets wiremock substitute an `http://127.0.0.1:N`
    /// origin.
    base: Url,
    /// Polite-pool contact email per `docs/SOURCES.md` §4 Crossref.
    /// Concretely formatted into the `User-Agent` header by [`crate::http::HttpClient`].
    /// (Phase 1: caller injects via [`CrossrefSource::new`]; CLI / config wiring
    /// lands in a follow-up PR.)
    #[allow(dead_code)]
    contact_email: String,
}

impl CrossrefSource {
    /// Production constructor: hard-codes `https://api.crossref.org` as the
    /// base URL. The `contact_email` value is appended to the polite-pool
    /// User-Agent (config plumbing arrives in a later PR — see
    /// `docs/SOURCES.md` §4).
    #[must_use]
    pub fn new(contact_email: String) -> Self {
        Self {
            // The hard-coded constant is a known-valid URL; the `expect`
            // here is the documented exception to the workspace
            // `expect_used` lint (it can never fire in practice).
            #[allow(clippy::expect_used)]
            base: Url::parse(DEFAULT_BASE).expect("hard-coded base URL is valid"),
            contact_email,
        }
    }

    /// Construct with an arbitrary base URL.
    ///
    /// The orchestrator (`doiget-cli::commands::fetch`) uses this to honor
    /// the `DOIGET_CROSSREF_BASE` env var, which lets integration tests point
    /// the source at a wiremock origin without compile-time gates. Production
    /// callers use [`CrossrefSource::new`].
    pub fn with_base(base: Url, contact_email: String) -> Self {
        Self {
            base,
            contact_email,
        }
    }

    /// Build the `/works/{doi}` URL for the configured base. Returns
    /// [`FetchError::SourceSchema`] if joining the path produces an invalid
    /// URL (only possible if the base URL is malformed — should never happen
    /// in production).
    fn request_url(&self, doi: &crate::Doi) -> Result<Url, FetchError> {
        // Crossref accepts the bare DOI (no `doi:` scheme). `Doi::as_str()`
        // already returns it without the scheme. The `/` inside the suffix
        // is URL-encoded by `reqwest` when the request is built; wiremock
        // sees the decoded path on its `path()` matcher.
        let path = format!("/works/{}", doi.as_str());
        self.base.join(&path).map_err(|e| FetchError::SourceSchema {
            hint: format!("crossref URL construction failed: {e}"),
        })
    }

    /// Resolves a free-form bibliographic citation string to ranked DOI candidates.
    pub async fn resolve_citation(
        &self,
        query: &str,
        rows: u8,
        ctx: &FetchContext,
    ) -> Result<Vec<crate::ResolvedCandidate>, FetchError> {
        // 1. Rate limiter
        let _permit = ctx.rate_limiter.acquire(self.name()).await;

        // 2. Build works query URL
        // /works?query.bibliographic=<query>&rows=<rows>&mailto=<email>
        let mut url = self
            .base
            .join("/works")
            .map_err(|e| FetchError::SourceSchema {
                hint: format!("crossref resolve_citation URL construction failed: {e}"),
            })?;
        url.query_pairs_mut()
            .append_pair("query.bibliographic", query)
            .append_pair("rows", &rows.to_string())
            .append_pair("mailto", &self.contact_email);

        // 3. HTTP fetch
        let (body, _final_url) = ctx.http.fetch_bytes(self.name(), url).await?;

        // 4. Parse JSON
        let envelope: serde_json::Value =
            serde_json::from_slice(&body).map_err(|e| FetchError::SourceSchema {
                hint: format!("crossref returned non-JSON for search: {e}"),
            })?;

        let items = envelope
            .get("message")
            .and_then(|m| m.get("items"))
            .and_then(|i| i.as_array())
            .ok_or_else(|| FetchError::SourceSchema {
                hint: "crossref response missing message.items".to_string(),
            })?;

        // 5. Tokenize query (unique tokens, sorted)
        let query_tokens = {
            let mut t: Vec<String> = query
                .split(|c: char| !c.is_alphanumeric())
                .map(|s| s.to_lowercase())
                .filter(|s| !s.is_empty())
                .collect();
            t.sort();
            t.dedup();
            t
        };

        if query_tokens.is_empty() {
            return Ok(Vec::new());
        }

        let mut candidates = Vec::new();

        for item in items {
            let doi = match item.get("DOI").and_then(|v| v.as_str()) {
                Some(d) => d.to_string(),
                None => continue,
            };

            let fields = crate::orchestrator::extract_crossref_fields(item);

            // Construct search text from candidate
            let mut candidate_text = String::new();
            if let Some(t) = &fields.title {
                candidate_text.push_str(&t.to_lowercase());
                candidate_text.push(' ');
            }
            if let Some(author) = fields.authors.first() {
                candidate_text.push_str(&author.to_lowercase());
                candidate_text.push(' ');
            }
            if let Some(v) = &fields.venue {
                candidate_text.push_str(&v.to_lowercase());
                candidate_text.push(' ');
            }
            if let Some(y) = fields.year {
                candidate_text.push_str(&y.to_string());
                candidate_text.push(' ');
            }

            // Simple tokenize of candidate into a HashSet for O(1) lookup.
            let candidate_tokens: HashSet<String> = candidate_text
                .split(|c: char| !c.is_alphanumeric())
                .map(|s| s.to_lowercase())
                .filter(|s| !s.is_empty())
                .collect();

            let matched = query_tokens
                .iter()
                .filter(|q| candidate_tokens.contains(*q))
                .count();

            let score = matched as f64 / query_tokens.len() as f64;

            if score >= MIN_CITATION_SCORE {
                let first_author = fields.authors.first().cloned().unwrap_or_default();
                candidates.push(crate::ResolvedCandidate {
                    doi,
                    title: fields.title.unwrap_or_default(),
                    author: first_author,
                    year: fields.year,
                    score,
                    source: "crossref".to_string(),
                });
            }
        }

        // 6. Sort candidates by score descending
        candidates.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        Ok(candidates)
    }
}

#[async_trait]
impl Source for CrossrefSource {
    fn name(&self) -> &str {
        "crossref"
    }

    fn can_serve(&self, _profile: &CapabilityProfile, ref_: &Ref) -> bool {
        matches!(ref_, Ref::Doi(_))
    }

    async fn fetch(
        &self,
        ref_: &Ref,
        _profile: &CapabilityProfile,
        ctx: &FetchContext,
    ) -> Result<FetchResult, FetchError> {
        let doi = match ref_ {
            Ref::Doi(d) => d,
            Ref::Arxiv(_) => {
                return Err(FetchError::NotEligible {
                    source_key: "crossref".into(),
                });
            }
        };

        // Step 1: rate limiter (politeness — `docs/SOURCES.md` §6).
        let _permit = ctx.rate_limiter.acquire(self.name()).await;

        // Step 2: HTTP fetch. Body is JSON; the `PDF_MAX_BYTES` size cap in
        // `HttpClient` applies. Crossref responses are well under 100 MB
        // even for bibliographically rich DOIs.
        let url = self.request_url(doi)?;
        let (body, final_url) = ctx.http.fetch_bytes(self.name(), url).await?;

        // Step 3: parse the response envelope. Crossref wraps the work
        // record in a top-level `{ "status": "ok", "message": { ... } }`
        // envelope (per <https://api.crossref.org/swagger-ui/index.html>).
        let envelope: CrossrefEnvelope =
            serde_json::from_slice(&body).map_err(|e| FetchError::SourceSchema {
                hint: format!("crossref returned non-JSON: {e}"),
            })?;
        if envelope.status != "ok" {
            return Err(FetchError::SourceSchema {
                hint: format!("crossref status = {}", envelope.status),
            });
        }

        // Step 4: log the fetch event (`docs/PROVENANCE_LOG.md` §3).
        // ADR-0021 §1 canonical-digest: promote the ref under the
        // "crossref" resolver profile (no version — Crossref does not
        // expose a per-call version token in Phase 1).
        let canonical = ref_.promote(self.name(), None).digest_hex();
        ctx.log.append(RowInput {
            event: LogEvent::Fetch,
            result: LogResult::Ok,
            capability: Capability::Oa,
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
            // Crossref is metadata; PDF retrieval is the job of Unpaywall /
            // publisher sources (Phase 1+ sibling PRs).
            pdf_bytes: None,
            final_url: Some(final_url),
            metadata_json: Some(envelope.message),
        })
    }
}

/// Top-level Crossref response envelope. Only `status` and `message` are
/// load-bearing here; `message-type`, `message-version`, etc. are ignored.
#[derive(Debug, Deserialize)]
struct CrossrefEnvelope {
    status: String,
    message: serde_json::Value,
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
    use crate::{ArxivId, CapabilityProfile, Doi, RateLimits, Ref};

    /// Build a `FetchContext` whose [`HttpClient`] allows the wiremock
    /// `http://` origin under the `crossref` source key, plus a
    /// tempdir-backed `ProvenanceLog`. Returns the tempdir so the caller
    /// keeps it alive for the duration of the test.
    fn build_test_context(wiremock_host: &str) -> (TempDir, FetchContext) {
        let td = TempDir::new().expect("tempdir");
        // Workspace lints ban `std::path::PathBuf`; convert via camino.
        let log_dir =
            Utf8PathBuf::try_from(td.path().to_path_buf()).expect("temp dir path must be UTF-8");
        let log_path = log_dir.join("test.jsonl");

        // Use the test-only constructor that relaxes `https_only` for the
        // initial leg so wiremock (which serves over plain HTTP) can be
        // reached. Redirect closure still rejects http:// targets — see
        // `http.rs::build_client_allow_http`.
        let http = Arc::new(HttpClient::new_for_tests_allow_http(
            "crossref",
            wiremock_host,
        ));
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
                cache_root: None,
            },
        )
    }

    /// Extract the host string of a wiremock server's URI.
    fn server_host(server: &MockServer) -> String {
        server
            .uri()
            .parse::<Url>()
            .expect("wiremock uri parses")
            .host_str()
            .expect("wiremock uri has host")
            .to_string()
    }

    /// Build a [`CrossrefSource`] pointing at the given wiremock URI.
    fn crossref_for(server: &MockServer) -> CrossrefSource {
        let base = server.uri().parse::<Url>().expect("wiremock uri parses");
        CrossrefSource::with_base(base, "test@example.org".to_string())
    }

    #[test]
    fn crossref_can_serve_returns_true_for_doi() {
        let s = CrossrefSource::new("test@example.org".into());
        let profile = CapabilityProfile::from_env().expect("clean env");
        let r = Ref::Doi(Doi::parse("10.1234/example").unwrap());
        assert!(s.can_serve(&profile, &r));
    }

    #[test]
    fn crossref_can_serve_returns_false_for_arxiv() {
        let s = CrossrefSource::new("test@example.org".into());
        let profile = CapabilityProfile::from_env().expect("clean env");
        let r = Ref::Arxiv(ArxivId::parse("2401.12345").unwrap());
        assert!(!s.can_serve(&profile, &r));
    }

    #[tokio::test]
    async fn crossref_fetch_returns_envelope_message() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/works/10.1234/example"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"status":"ok","message":{"title":["Example"]}}"#),
            )
            .mount(&server)
            .await;

        let host = server_host(&server);
        let s = crossref_for(&server);
        let (_td, ctx) = build_test_context(&host);
        let profile = CapabilityProfile::from_env().expect("clean env");
        let r = Ref::Doi(Doi::parse("10.1234/example").unwrap());

        let res = s.fetch(&r, &profile, &ctx).await.expect("fetch ok");
        assert_eq!(res.source, "crossref");
        assert_eq!(
            res.metadata_json,
            Some(serde_json::json!({ "title": ["Example"] })),
        );
        assert!(res.pdf_bytes.is_none());
        assert!(res.final_url.is_some());
    }

    #[tokio::test]
    async fn crossref_fetch_with_arxiv_ref_errors_not_eligible() {
        // wiremock not needed: the arxiv branch short-circuits before any
        // outbound call. Construct the source with a dummy base, and pass
        // a dummy allowlist host since fetch never reaches the HTTP layer.
        let s = CrossrefSource::with_base(
            Url::parse("http://127.0.0.1:1/").unwrap(),
            "test@example.org".into(),
        );
        let (_td, ctx) = build_test_context("127.0.0.1");
        let profile = CapabilityProfile::from_env().expect("clean env");
        let r = Ref::Arxiv(ArxivId::parse("2401.12345").unwrap());

        let err = s.fetch(&r, &profile, &ctx).await.expect_err("not eligible");
        match err {
            FetchError::NotEligible { source_key } => {
                assert_eq!(source_key, "crossref");
            }
            other => panic!("expected NotEligible, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn crossref_fetch_writes_log_row() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/works/10.1234/example"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"status":"ok","message":{"title":["Example"]}}"#),
            )
            .mount(&server)
            .await;

        let host = server_host(&server);
        let s = crossref_for(&server);
        let (_td, ctx) = build_test_context(&host);
        let profile = CapabilityProfile::from_env().expect("clean env");
        let r = Ref::Doi(Doi::parse("10.1234/example").unwrap());

        let _res = s.fetch(&r, &profile, &ctx).await.expect("fetch ok");

        // Reopen the log file as raw JSON Lines and assert the single row's
        // semantic fields. We deliberately don't reach into ProvenanceLog
        // internals — the public read path is "parse the JSONL by line".
        let log_path = _td.path().join("test.jsonl");
        let raw = std::fs::read_to_string(&log_path).expect("log file readable");
        let lines: Vec<&str> = raw.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 1, "expected exactly one row, got {:?}", lines);
        let row: serde_json::Value = serde_json::from_str(lines[0]).expect("row is valid JSON");
        assert_eq!(row["event"], "fetch");
        assert_eq!(row["result"], "ok");
        assert_eq!(row["source"], "crossref");
        assert_eq!(row["ref"], "10.1234/example");
    }

    #[tokio::test]
    async fn crossref_404_maps_to_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/works/10.1234/example"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let host = server_host(&server);
        let s = crossref_for(&server);
        let (_td, ctx) = build_test_context(&host);
        let profile = CapabilityProfile::from_env().expect("clean env");
        let r = Ref::Doi(Doi::parse("10.1234/example").unwrap());

        let err = s.fetch(&r, &profile, &ctx).await.expect_err("404 errors");
        match err {
            FetchError::Http(_) => {}
            other => panic!("expected Http(_) on 404, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn crossref_non_ok_status_field_errors_source_schema() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/works/10.1234/example"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(r#"{"status":"error","message":{}}"#),
            )
            .mount(&server)
            .await;

        let host = server_host(&server);
        let s = crossref_for(&server);
        let (_td, ctx) = build_test_context(&host);
        let profile = CapabilityProfile::from_env().expect("clean env");
        let r = Ref::Doi(Doi::parse("10.1234/example").unwrap());

        let err = s
            .fetch(&r, &profile, &ctx)
            .await
            .expect_err("non-ok status errors");
        match err {
            FetchError::SourceSchema { hint } => {
                assert!(
                    hint.contains("status"),
                    "expected status mention in hint, got {hint}"
                );
            }
            other => panic!("expected SourceSchema, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_resolve_citation_success() {
        let server = MockServer::start().await;
        let mock_body = serde_json::json!({
            "status": "ok",
            "message": {
                "items": [
                    {
                        "DOI": "10.1000/xyz123",
                        "title": ["Lars Onsager, Crystal Statistics. I. A Two-Dimensional Model with an Order-Disorder Transition"],
                        "author": [
                            {"family": "Onsager", "given": "Lars"}
                        ],
                        "issued": {
                            "date-parts": [[1944, 2, 1]]
                        },
                        "container-title": ["Physical Review"]
                    },
                    {
                        "DOI": "10.1000/unrelated",
                        "title": ["Some Unrelated Paper"],
                        "author": [
                            {"family": "Smith", "given": "John"}
                        ],
                        "issued": {
                            "date-parts": [[2020]]
                        }
                    }
                ]
            }
        });

        Mock::given(method("GET"))
            .and(path("/works"))
            .respond_with(ResponseTemplate::new(200).set_body_json(mock_body))
            .mount(&server)
            .await;

        let host = server_host(&server);
        let s = crossref_for(&server);
        let (_td, ctx) = build_test_context(&host);

        let candidates = s
            .resolve_citation("Onsager 1944", 2, &ctx)
            .await
            .expect("resolve ok");

        // The query "Onsager 1944" has tokens ["onsager", "1944"].
        // The first candidate has both "onsager" (author family) and "1944" (issued year). Score is 1.0.
        // The second candidate has neither. Score is 0.0, filtered out.
        assert_eq!(candidates.len(), 1);
        let cand = &candidates[0];
        assert_eq!(cand.doi, "10.1000/xyz123");
        assert_eq!(cand.title, "Lars Onsager, Crystal Statistics. I. A Two-Dimensional Model with an Order-Disorder Transition");
        assert_eq!(cand.author, "Onsager, Lars");
        assert_eq!(cand.year, Some(1944));
        assert_eq!(cand.score, 1.0);
    }
}
