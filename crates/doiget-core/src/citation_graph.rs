//! Citation graph BFS expansion (Phase 4 / ADR-0010).
//!
//! Spec: [`docs/DECISIONS/0010-citation-graph-hard-cap.md`].
//!
//! ## Binding contract
//!
//! - Library-const hard caps (depth=3, total=100, per-paper=20)
//!   apply regardless of caller-supplied parameters. Callers MAY ask
//!   for less; they MUST NOT exceed the caps. [`GraphCaps::clamped`]
//!   enforces this — the public entry point [`expand`] runs
//!   `clamped()` on whatever the caller passed.
//! - `truncated: true` is set in [`GraphResult`] when ANY cap is hit
//!   (the total cap or per-paper cap or depth cap).
//! - Cycle detection is mandatory: a `visited` set tracks every
//!   work id added to the graph; duplicates are dropped.
//! - TDM sources (Tier 3) are NEVER consulted during citation graph
//!   expansion. This module only uses OpenAlex via `ctx.http` (the
//!   `openalex` redirect-allowlist key from
//!   [`crate::http::tier_2_allowlist`]).
//!
//! ## How it walks
//!
//! 1. The seed `Doi` is resolved through [`OpenalexSource::fetch`]
//!    so the seed lands in the audit trail under
//!    `Capability::Metadata`. The response yields the seed's
//!    OpenAlex Work ID + its `referenced_works` array.
//! 2. The BFS queue holds `(work_id, depth)` pairs. For each
//!    dequeued work whose depth is `< caps.depth`, we fetch
//!    `<openalex_base>/works/<work_id>` directly via
//!    `ctx.http.fetch_bytes("openalex", url)` (same source-key the
//!    seed used, so the redirect closure stays happy), parse the
//!    `referenced_works` array, and queue up to `caps.per_paper`
//!    children.
//! 3. The BFS terminates when the queue is empty OR
//!    `total_visited >= caps.total`. In the latter case
//!    `truncated = true`.
//!
//! ## What is not in this slice (follow-up)
//!
//! - The fetches issued during the BFS walk reuse the OpenAlex
//!   redirect-allowlist key and the `ctx.http` shared client, but
//!   they do NOT go through [`OpenalexSource::fetch`] (which only
//!   accepts a DOI ref). A future refactor may add
//!   `OpenalexSource::fetch_by_work_id` so the per-fetch provenance
//!   row is emitted the same way the seed's is; today the BFS
//!   walker emits its own `LogEvent::Fetch` rows directly via
//!   `ctx.log.append`.
//! - S2 / DOAJ fallback: not used. The graph walker is OpenAlex-
//!   only by design — only OpenAlex Work records expose the
//!   `referenced_works` array in a single round-trip.

#![cfg(feature = "citation")]

use std::collections::{HashSet, VecDeque};

use serde::Serialize;
use url::Url;

use crate::provenance::{Capability, LogError, LogEvent, LogResult, RowInput};
use crate::source::{FetchContext, FetchError, Source};
use crate::sources::openalex::OpenalexSource;
use crate::{CapabilityProfile, Doi, Ref};

/// Hard caps from ADR-0010. Library const defaults; callers may
/// request a smaller value, but the maximums below are enforced by
/// [`Self::clamped`].
#[derive(Debug, Clone, Copy)]
pub struct GraphCaps {
    /// Maximum BFS depth (default 3, max 3). Depth 0 is the seed
    /// only.
    pub depth: usize,
    /// Maximum total number of nodes in the result (default 100,
    /// max 100). When the cap is hit the result is
    /// `truncated: true`.
    pub total: usize,
    /// Maximum number of children expanded per parent (default 20,
    /// max 20). Excess `referenced_works` entries from a single
    /// parent are dropped (and flip `truncated`).
    pub per_paper: usize,
}

impl GraphCaps {
    /// Library-const maximum depth per ADR-0010.
    pub const MAX_DEPTH: usize = 3;
    /// Library-const maximum total nodes per ADR-0010.
    pub const MAX_TOTAL: usize = 100;
    /// Library-const maximum per-paper children per ADR-0010.
    pub const MAX_PER_PAPER: usize = 20;

    /// Returns a `GraphCaps` clamped to the ADR-0010 maxima.
    ///
    /// Values greater than the corresponding `MAX_*` constants are
    /// silently reduced; `0` is left as-is so callers can ask for
    /// "seed only" with `depth: 0`. This is the load-bearing
    /// enforcement point per ADR-0010: "expand_citation_graph
    /// applies library const hard-caps regardless of caller-supplied
    /// parameters."
    #[must_use]
    pub fn clamped(self) -> Self {
        Self {
            depth: self.depth.min(Self::MAX_DEPTH),
            total: self.total.min(Self::MAX_TOTAL),
            per_paper: self.per_paper.min(Self::MAX_PER_PAPER),
        }
    }
}

impl Default for GraphCaps {
    fn default() -> Self {
        Self {
            depth: Self::MAX_DEPTH,
            total: Self::MAX_TOTAL,
            per_paper: Self::MAX_PER_PAPER,
        }
    }
}

/// One node in the citation graph result.
#[derive(Debug, Clone, Serialize)]
pub struct GraphNode {
    /// OpenAlex Work ID (short form, e.g. `"W2741809807"`).
    pub id: String,
    /// BFS depth from the seed. `0` is the seed itself.
    pub depth: usize,
}

/// One edge: `from` cites `to`.
#[derive(Debug, Clone, Serialize)]
pub struct GraphEdge {
    /// The citing Work (OpenAlex Work ID).
    pub from: String,
    /// The cited Work (OpenAlex Work ID).
    pub to: String,
}

/// Result of [`expand`].
#[derive(Debug, Clone, Serialize)]
pub struct GraphResult {
    /// OpenAlex Work ID the expansion started from.
    pub seed_work_id: String,
    /// Every Work visited, including the seed (`depth == 0`).
    pub nodes: Vec<GraphNode>,
    /// Directed `from` cites `to` relationships discovered during the walk.
    pub edges: Vec<GraphEdge>,
    /// `true` if an ADR-0010 hard cap (depth / total / per-paper) stopped the
    /// walk before the graph was exhausted.
    pub truncated: bool,
    /// Number of Works visited (equals `nodes.len()`).
    pub total_visited: usize,
}

/// Errors from citation-graph expansion.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GraphError {
    /// An OpenAlex fetch failed during the walk.
    #[error("openalex source error: {0}")]
    Source(#[from] FetchError),
    /// Provenance log write failed — fail-closed (the expansion is
    /// aborted partway through). Matches the rest of the codebase's
    /// posture per `docs/PROVENANCE_LOG.md` §5.
    #[error("provenance log error during graph expansion: {0}")]
    Log(#[from] LogError),
    /// The seed DOI is not indexed by OpenAlex (no `id` field in the response).
    #[error("seed DOI not indexed by OpenAlex (no `id` field in response)")]
    SeedNotIndexed,
    /// Citation expansion is not enabled (requires `DOIGET_ENABLE_OPENALEX`
    /// and `--features metadata`).
    #[error("citation graph requires DOIGET_ENABLE_OPENALEX + --features metadata")]
    CapabilityDenied,
}

/// Expand the citation graph for `seed_doi` via OpenAlex.
pub async fn expand(
    seed_doi: &Doi,
    caps: GraphCaps,
    source: &OpenalexSource,
    profile: &CapabilityProfile,
    ctx: &FetchContext,
) -> Result<GraphResult, GraphError> {
    if !profile.metadata.openalex {
        return Err(GraphError::CapabilityDenied);
    }
    let caps = caps.clamped();

    // Step 1: resolve the seed through OpenalexSource so the seed
    // fetch lands in the audit trail under the documented path.
    let ref_ = Ref::Doi(seed_doi.clone());
    let seed_result = source.fetch(&ref_, profile, ctx).await?;
    let seed_work = seed_result
        .metadata_json
        .ok_or(GraphError::SeedNotIndexed)?;

    let seed_work_id = extract_work_id(&seed_work).ok_or(GraphError::SeedNotIndexed)?;
    let seed_refs = extract_referenced_works(&seed_work);

    let mut nodes = vec![GraphNode {
        id: seed_work_id.clone(),
        depth: 0,
    }];
    let mut edges: Vec<GraphEdge> = Vec::new();
    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(seed_work_id.clone());
    let mut truncated = false;
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();

    enqueue_children(
        &seed_work_id,
        &seed_refs,
        1,
        &caps,
        &mut nodes,
        &mut edges,
        &mut visited,
        &mut queue,
        &mut truncated,
    );

    while let Some((work_id, depth)) = queue.pop_front() {
        if depth >= caps.depth {
            truncated = true;
            continue;
        }
        if nodes.len() >= caps.total {
            truncated = true;
            break;
        }
        let work_url = match build_openalex_work_url(&work_id) {
            Ok(u) => u,
            Err(_) => {
                truncated = true;
                continue;
            }
        };
        let body = match ctx.http.fetch_bytes("openalex", work_url).await {
            Ok((b, _final)) => b,
            Err(e) => {
                let _ = ctx.log.append(RowInput {
                    event: LogEvent::Fetch,
                    result: LogResult::Err,
                    capability: Capability::Metadata,
                    ref_: Some(work_id.as_str()),
                    source: Some("openalex"),
                    error_code: None,
                    size_bytes: None,
                    license: None,
                    store_path: None,
                    canonical_digest: None,
                });
                tracing::warn!(work_id = %work_id, error = %e, "citation-graph step failed");
                truncated = true;
                continue;
            }
        };
        ctx.log.append(RowInput {
            event: LogEvent::Fetch,
            result: LogResult::Ok,
            capability: Capability::Metadata,
            ref_: Some(work_id.as_str()),
            source: Some("openalex"),
            error_code: None,
            size_bytes: Some(body.len() as u64),
            license: None,
            store_path: None,
            canonical_digest: None,
        })?;
        let work: serde_json::Value = match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(_) => {
                truncated = true;
                continue;
            }
        };
        let refs = extract_referenced_works(&work);
        enqueue_children(
            &work_id,
            &refs,
            depth + 1,
            &caps,
            &mut nodes,
            &mut edges,
            &mut visited,
            &mut queue,
            &mut truncated,
        );
    }

    Ok(GraphResult {
        seed_work_id,
        total_visited: nodes.len(),
        nodes,
        edges,
        truncated,
    })
}

#[allow(clippy::too_many_arguments)]
fn enqueue_children(
    parent_id: &str,
    refs: &[String],
    child_depth: usize,
    caps: &GraphCaps,
    nodes: &mut Vec<GraphNode>,
    edges: &mut Vec<GraphEdge>,
    visited: &mut HashSet<String>,
    queue: &mut VecDeque<(String, usize)>,
    truncated: &mut bool,
) {
    if refs.len() > caps.per_paper {
        *truncated = true;
    }
    for child in refs.iter().take(caps.per_paper) {
        if visited.contains(child) {
            edges.push(GraphEdge {
                from: parent_id.to_string(),
                to: child.to_string(),
            });
            continue;
        }
        if nodes.len() >= caps.total {
            *truncated = true;
            return;
        }
        visited.insert(child.clone());
        nodes.push(GraphNode {
            id: child.clone(),
            depth: child_depth,
        });
        edges.push(GraphEdge {
            from: parent_id.to_string(),
            to: child.clone(),
        });
        queue.push_back((child.clone(), child_depth));
    }
}

/// Extract the short OpenAlex Work ID from a Work record's `id`
/// field (the response carries the full URL `https://openalex.org/W...`;
/// the graph stores the short form).
fn extract_work_id(work: &serde_json::Value) -> Option<String> {
    let full = work.get("id")?.as_str()?;
    Some(full.rsplit('/').next().unwrap_or(full).to_string())
}

/// Extract `referenced_works` as `Vec<short_work_id>`.
fn extract_referenced_works(work: &serde_json::Value) -> Vec<String> {
    work.get("referenced_works")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.as_str())
                .map(|s| s.rsplit('/').next().unwrap_or(s).to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Build a URL for `<openalex_base>/works/<work_id>`. Honors the
/// `DOIGET_OPENALEX_BASE` env var so wiremock tests can swap the
/// origin.
fn build_openalex_work_url(work_id: &str) -> Result<Url, FetchError> {
    let base_str =
        std::env::var("DOIGET_OPENALEX_BASE").unwrap_or_else(|_| "https://api.openalex.org".into());
    let base = Url::parse(&base_str).map_err(|e| FetchError::SourceSchema {
        hint: format!("openalex base URL invalid: {e}"),
    })?;
    base.join(&format!("/works/{}", work_id))
        .map_err(|e| FetchError::SourceSchema {
            hint: format!("openalex Work URL construction failed: {e}"),
        })
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
    use crate::{CapabilityProfile, Doi, MetadataAccess, RateLimits};

    const SEED_WORK: &str = r#"{
        "id": "https://openalex.org/W0001",
        "doi": "https://doi.org/10.1234/seed",
        "display_name": "Seed Paper",
        "referenced_works": [
            "https://openalex.org/W0002",
            "https://openalex.org/W0003"
        ]
    }"#;

    const HOP_W0002: &str = r#"{
        "id": "https://openalex.org/W0002",
        "referenced_works": ["https://openalex.org/W0004"]
    }"#;

    const HOP_W0003: &str = r#"{
        "id": "https://openalex.org/W0003",
        "referenced_works": []
    }"#;

    const HOP_W0004: &str = r#"{
        "id": "https://openalex.org/W0004",
        "referenced_works": []
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
        let mut p = CapabilityProfile::from_env().expect("clean env never errors");
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
    async fn caps_clamps_to_adr_0010_maxima() {
        let huge = GraphCaps {
            depth: 99,
            total: 99999,
            per_paper: 999,
        };
        let c = huge.clamped();
        assert_eq!(c.depth, GraphCaps::MAX_DEPTH);
        assert_eq!(c.total, GraphCaps::MAX_TOTAL);
        assert_eq!(c.per_paper, GraphCaps::MAX_PER_PAPER);

        let small = GraphCaps {
            depth: 1,
            total: 5,
            per_paper: 2,
        };
        let c2 = small.clamped();
        assert_eq!(c2.depth, 1);
        assert_eq!(c2.total, 5);
        assert_eq!(c2.per_paper, 2);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn expand_walks_depth_2_graph() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/works/doi:10.1234/seed"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SEED_WORK))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/works/W0002"))
            .respond_with(ResponseTemplate::new(200).set_body_string(HOP_W0002))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/works/W0003"))
            .respond_with(ResponseTemplate::new(200).set_body_string(HOP_W0003))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/works/W0004"))
            .respond_with(ResponseTemplate::new(200).set_body_string(HOP_W0004))
            .mount(&server)
            .await;

        let prev = std::env::var("DOIGET_OPENALEX_BASE").ok();
        std::env::set_var("DOIGET_OPENALEX_BASE", server.uri());

        let (_td, ctx) = build_test_context(&server.uri());
        let src = OpenalexSource::with_base(
            Url::parse(&server.uri()).expect("wiremock URI parses"),
            "doiget@localhost".to_string(),
        );
        let profile = profile_with_openalex_enabled();
        let seed = Doi::parse("10.1234/seed").expect("DOI parses");

        let result = expand(&seed, GraphCaps::default(), &src, &profile, &ctx).await;

        // Restore env var BEFORE asserting so a panic doesn't leak it.
        match prev {
            Some(v) => std::env::set_var("DOIGET_OPENALEX_BASE", v),
            None => std::env::remove_var("DOIGET_OPENALEX_BASE"),
        }

        let result = result.expect("expand ok");
        assert_eq!(result.seed_work_id, "W0001");
        assert_eq!(result.total_visited, 4, "nodes: {:?}", result.nodes);
        assert_eq!(result.nodes[0].id, "W0001");
        assert_eq!(result.nodes[0].depth, 0);
        assert_eq!(result.edges.len(), 3);
        assert!(!result.truncated, "graph should be complete");
    }

    #[tokio::test]
    async fn expand_without_capability_flag_errors() {
        let (_td, ctx) = build_test_context("http://127.0.0.1:1");
        let src = OpenalexSource::with_base(
            Url::parse("http://127.0.0.1:1").expect("URI parses"),
            "doiget@localhost".to_string(),
        );
        let profile = CapabilityProfile::from_env().expect("clean env never errors");
        let seed = Doi::parse("10.1234/seed").expect("DOI parses");

        let err = expand(&seed, GraphCaps::default(), &src, &profile, &ctx)
            .await
            .expect_err("missing openalex capability must error");
        assert!(matches!(err, GraphError::CapabilityDenied));
    }
}
