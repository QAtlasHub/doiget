//! Dry-run preview shape for `--dry-run` CLI fetches and the
//! `doiget_metadata_only` / `doiget_fetch_paper` MCP tools.
//!
//! Binding spec: [`docs/DECISIONS/0022-dry-run-mode.md`](../../../docs/DECISIONS/0022-dry-run-mode.md)
//! §1 (NORMATIVE wire shape) and [`docs/MCP_TOOLS.md`](../../../docs/MCP_TOOLS.md)
//! §10 (MCP envelope mirror).
//!
//! The types here live in `doiget-core` rather than `doiget-cli` so that
//! both `doiget-cli` (the `--dry-run` flag path) and `doiget-mcp` (the
//! `dry_run: true` tool variants) can serialize bit-identical envelopes
//! without `doiget-mcp` having to depend on `doiget-cli` (which would
//! invert the existing `doiget-cli -> doiget-mcp` wiring).
//!
//! ## Honesty about candidate uncertainty
//!
//! The `pdf_sources[].candidate_hosts` list is the **static allowlist**
//! for the named resolver, not the host the actual fetch would have hit.
//! doiget cannot know the post-Unpaywall OA URL host without making the
//! Unpaywall network call, and `--dry-run` MUST NOT make it. The preview
//! is therefore an *upper-bound* on the hosts a real fetch could touch,
//! not a prediction of the single host it would touch (ADR-0022 §4).

use camino::{Utf8Path, Utf8PathBuf};
use serde::Serialize;

use crate::http::{oa_publisher_allowlist, tier_1_allowlist, SourceAllowlist};
use crate::{RateLimits, Ref};

/// Per-PDF-source row inside [`FetchPlan::pdf_sources`].
///
/// `candidate_hosts` is the static allowlist for the named resolver, not
/// a prediction of the single host the real fetch would touch — see
/// [module docs](self) and ADR-0022 §4 ("Honesty about candidate
/// uncertainty").
#[derive(Debug, Clone, Serialize)]
pub struct PdfSourcePlan {
    /// Resolver source key (e.g. `"oa-publisher"`, `"arxiv"`).
    pub key: String,
    /// Allowlist hosts the real fetch would be permitted to touch.
    pub candidate_hosts: Vec<String>,
}

/// Per-process rate-limit context surfaced alongside [`FetchPlan`] so an
/// agent can predict the politeness ceiling without a separate
/// `doiget_capability_profile` round-trip.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct RateLimitBudget {
    /// Process-wide cap (matches [`RateLimits::HARD_CODED`]).
    pub global_per_sec: f32,
    /// Per-source minimum gap between consecutive requests, ms.
    pub per_source_min_gap_ms: u64,
}

/// Structured dry-run preview returned by `--dry-run` and the
/// `dry_run: true` MCP variants. Wire shape matches ADR-0022 §1 and
/// `docs/MCP_TOOLS.md` §10.
#[derive(Debug, Clone, Serialize)]
pub struct FetchPlan {
    /// Metadata sources the real fetch would consult, in dispatch order.
    pub metadata_sources: Vec<String>,
    /// PDF sources the real fetch could attempt. `candidate_hosts` is an
    /// upper-bound on the hosts a real fetch would touch (see
    /// [`PdfSourcePlan`]).
    pub pdf_sources: Vec<PdfSourcePlan>,
    /// Source keys whose redirect allowlists are loaded into the HTTP
    /// client. Useful for validating `CapabilityProfile` configuration
    /// drift.
    pub redirect_allowlists_loaded: Vec<String>,
    /// Where the PDF would land on disk (always `<root>/<safekey>.pdf`).
    pub target_pdf_path: Utf8PathBuf,
    /// Where the metadata TOML would land
    /// (always `<root>/.metadata/<safekey>.toml`).
    pub target_metadata_path: Utf8PathBuf,
    /// `true` in Phase 1+ — every successful fetch appends a provenance
    /// row. Named explicitly so future fetch modes can declare "this
    /// fetch would NOT append" without inverting the flag's meaning
    /// (ADR-0022 §1).
    pub would_append_provenance: bool,
    /// Always `true` in Phase 1: [`PdfSourcePlan::candidate_hosts`] is the
    /// **static allowlist** for the resolver, NOT a prediction of the
    /// single host the real fetch would touch. See ADR-0022 §4 ("Honesty
    /// about candidate uncertainty"). The field is machine-parseable so
    /// an agent can detect the upper-bound semantics without reading the
    /// spec — encoding the §4 disclaimer into the wire shape itself.
    pub candidate_hosts_are_upper_bound: bool,
}

/// Hard-coded rate-limit budget surfaced with every [`FetchPlan`] preview.
/// Mirrors [`RateLimits::HARD_CODED`] / `docs/LEGAL.md` §6 safeguard 8.
pub fn rate_limit_budget() -> RateLimitBudget {
    RateLimitBudget {
        global_per_sec: RateLimits::HARD_CODED.max_fetches_per_second(),
        per_source_min_gap_ms: RateLimits::HARD_CODED.per_source_backoff_ms(),
    }
}

/// Build the dry-run preview ([`FetchPlan`]) for the given ref and store
/// root, without contacting the network or filesystem.
///
/// Per-ref-kind shape (ADR-0022 §1, NORMATIVE):
///
/// - **DOI** → `metadata_sources = ["crossref", "unpaywall"]`,
///   `pdf_sources = [{ key: "oa-publisher", candidate_hosts: oa_publisher_allowlist hosts }]`.
/// - **arXiv** → `metadata_sources = []`,
///   `pdf_sources = [{ key: "arxiv", candidate_hosts: tier_1 arxiv hosts }]`.
///
/// `redirect_allowlists_loaded` always contains the four source keys the
/// production HTTP client is built with (Tier 1 + the synthetic OA
/// publisher), reflecting `doiget-cli::commands::fetch::build_http_client`'s
/// composition.
///
/// # Panics
///
/// Panics with a self-documenting message if the in-crate allowlist
/// builders ([`oa_publisher_allowlist`] / [`tier_1_allowlist`]) ever stop
/// returning the source keys this function looks up. That signals an
/// internal-contract drift bug, not a user error — fail-fast at preview
/// time is preferable to silently emitting an empty `candidate_hosts`
/// list. The workspace `clippy::expect_used` lint is `warn`-level
/// (promoted to `deny` under `-D warnings`); the localized `#[allow]` is
/// the minimal intervention here, mirroring the pattern in
/// `crate::http::HttpClient::new_for_tests_allow_http`.
#[allow(clippy::expect_used)]
pub fn build_fetch_plan(ref_: &Ref, store_root: &Utf8Path) -> FetchPlan {
    let safekey = ref_.safekey();
    let target_pdf_path = store_root.join(format!("{}.pdf", safekey.as_str()));
    let target_metadata_path = store_root
        .join(".metadata")
        .join(format!("{}.toml", safekey.as_str()));

    let (metadata_sources, pdf_sources) = match ref_ {
        Ref::Doi(_) => {
            // Internal contract: `oa_publisher_allowlist()` MUST always
            // return an entry whose `.source == "oa-publisher"`. Silent
            // `.unwrap_or_default()` here would mask drift between this
            // function and the allowlist source-of-truth — the resulting
            // empty `candidate_hosts` would mislead an agent into
            // believing the OA leg has no allowed hosts.
            let oa_hosts = oa_publisher_allowlist()
                .into_iter()
                .find(|a: &SourceAllowlist| a.source == "oa-publisher")
                .map(|a| a.redirect_hosts)
                .expect(
                    "oa-publisher allowlist must exist (see \
                     crates/doiget-core/src/http.rs::oa_publisher_allowlist); \
                     if this fires, build_fetch_plan and oa_publisher_allowlist \
                     have drifted",
                );
            (
                vec!["crossref".to_string(), "unpaywall".to_string()],
                vec![PdfSourcePlan {
                    key: "oa-publisher".to_string(),
                    candidate_hosts: oa_hosts,
                }],
            )
        }
        Ref::Arxiv(_) => {
            // Same internal-contract rationale as the DOI branch above.
            let arxiv_hosts = tier_1_allowlist()
                .into_iter()
                .find(|a: &SourceAllowlist| a.source == "arxiv")
                .map(|a| a.redirect_hosts)
                .expect(
                    "tier-1 allowlist must include 'arxiv' (see \
                     crates/doiget-core/src/http.rs::tier_1_allowlist); \
                     if this fires, build_fetch_plan and tier_1_allowlist \
                     have drifted",
                );
            (
                Vec::<String>::new(),
                vec![PdfSourcePlan {
                    key: "arxiv".to_string(),
                    candidate_hosts: arxiv_hosts,
                }],
            )
        }
    };

    FetchPlan {
        metadata_sources,
        pdf_sources,
        redirect_allowlists_loaded: vec![
            "crossref".to_string(),
            "unpaywall".to_string(),
            "arxiv".to_string(),
            "oa-publisher".to_string(),
        ],
        target_pdf_path,
        target_metadata_path,
        would_append_provenance: true,
        // Always `true` in Phase 1 per ADR-0022 §4 ("Honesty about
        // candidate uncertainty"): `candidate_hosts` is the static
        // resolver allowlist, NOT a prediction of the single host the
        // real fetch would touch. Surfaced on the wire so agents can
        // detect the upper-bound semantics without parsing the spec.
        candidate_hosts_are_upper_bound: true,
    }
}

/// Build the dry-run envelope as a `serde_json::Value`, without writing
/// anywhere. Used by both the CLI (which prints it to stdout) and the
/// MCP tool wrapper (which routes the bytes via JSON-RPC). Wire shape:
///
/// ```jsonc
/// {
///   "ok": true,
///   "dry_run": true,
///   "ref": { "doi": "10.1234/foo" } | { "arxiv": "2401.12345" },
///   "plan": { ... see FetchPlan ... },
///   "rate_limit_budget": { "global_per_sec": 5.0, "per_source_min_gap_ms": 200 }
/// }
/// ```
pub fn build_dry_run_envelope(ref_: &Ref, plan: &FetchPlan) -> serde_json::Value {
    serde_json::json!({
        "ok": true,
        "dry_run": true,
        "ref": ref_kind_object(ref_),
        "plan": plan,
        "rate_limit_budget": rate_limit_budget(),
    })
}

/// Build the `ref` field of the dry-run envelope per ADR-0022 §1:
/// `{"doi": "10.1234/foo"}` for a DOI ref, `{"arxiv": "2401.12345"}` for
/// an arXiv ref. We intentionally do NOT serialize the full `Ref` enum
/// (which would emit `{"kind":"doi","id":"10.1234/foo"}` per the
/// internally-tagged `#[serde(tag,content)]` form), because the wire
/// shape in the ADR uses a flat single-key object.
fn ref_kind_object(ref_: &Ref) -> serde_json::Value {
    match ref_ {
        Ref::Doi(d) => serde_json::json!({ "doi": d.as_str() }),
        Ref::Arxiv(a) => serde_json::json!({ "arxiv": a.as_str() }),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::{ArxivId, Doi};

    fn temp_root() -> Utf8PathBuf {
        Utf8PathBuf::from("/tmp/doiget-test-store")
    }

    #[test]
    fn doi_plan_carries_crossref_and_unpaywall_metadata_sources() {
        let r = Ref::Doi(Doi("10.1234/example".to_string()));
        let plan = build_fetch_plan(&r, &temp_root());
        assert_eq!(plan.metadata_sources, vec!["crossref", "unpaywall"]);
        assert_eq!(plan.pdf_sources.len(), 1);
        assert_eq!(plan.pdf_sources[0].key, "oa-publisher");
        assert!(
            !plan.pdf_sources[0].candidate_hosts.is_empty(),
            "OA publisher hosts must be populated"
        );
        assert!(plan.would_append_provenance);
    }

    #[test]
    fn arxiv_plan_has_empty_metadata_sources_and_arxiv_pdf_source() {
        let r = Ref::Arxiv(ArxivId("2401.12345".to_string()));
        let plan = build_fetch_plan(&r, &temp_root());
        assert!(plan.metadata_sources.is_empty());
        assert_eq!(plan.pdf_sources.len(), 1);
        assert_eq!(plan.pdf_sources[0].key, "arxiv");
        assert!(plan.pdf_sources[0]
            .candidate_hosts
            .iter()
            .any(|h| h == "arxiv.org"));
    }

    #[test]
    fn plan_target_paths_are_safekey_derived() {
        let r = Ref::Doi(Doi("10.1234/example".to_string()));
        let root = Utf8PathBuf::from("/tmp/store");
        let plan = build_fetch_plan(&r, &root);
        assert_eq!(plan.target_pdf_path, root.join("doi_10.1234_example.pdf"));
        assert_eq!(
            plan.target_metadata_path,
            root.join(".metadata").join("doi_10.1234_example.toml")
        );
    }

    #[test]
    fn dry_run_envelope_has_top_level_ok_dry_run_and_rate_budget() {
        let r = Ref::Doi(Doi("10.1234/foo".to_string()));
        let plan = build_fetch_plan(&r, &temp_root());
        let env = build_dry_run_envelope(&r, &plan);
        assert_eq!(env["ok"], serde_json::json!(true));
        assert_eq!(env["dry_run"], serde_json::json!(true));
        assert_eq!(env["ref"], serde_json::json!({ "doi": "10.1234/foo" }));
        assert_eq!(
            env["rate_limit_budget"]["global_per_sec"],
            serde_json::json!(5.0)
        );
        assert_eq!(
            env["rate_limit_budget"]["per_source_min_gap_ms"],
            serde_json::json!(200)
        );
    }

    #[test]
    fn dry_run_envelope_arxiv_ref_uses_arxiv_key() {
        let r = Ref::Arxiv(ArxivId("2401.12345".to_string()));
        let plan = build_fetch_plan(&r, &temp_root());
        let env = build_dry_run_envelope(&r, &plan);
        assert_eq!(env["ref"], serde_json::json!({ "arxiv": "2401.12345" }));
    }

    #[test]
    fn fetch_plan_carries_candidate_hosts_are_upper_bound_true() {
        // ADR-0022 §4 ("Honesty about candidate uncertainty"): the field
        // is always `true` in Phase 1, and the wire envelope must
        // surface it inside `plan` so agents can detect the upper-bound
        // semantics without consulting the spec.
        let r = Ref::Doi(Doi("10.1234/example".to_string()));
        let plan = build_fetch_plan(&r, &temp_root());
        assert!(plan.candidate_hosts_are_upper_bound);
        let env = build_dry_run_envelope(&r, &plan);
        assert_eq!(
            env["plan"]["candidate_hosts_are_upper_bound"],
            serde_json::json!(true),
            "plan.candidate_hosts_are_upper_bound must be true on the \
             wire (ADR-0022 §4); got: {env}"
        );
    }

    #[test]
    fn redirect_allowlists_loaded_lists_all_four_sources() {
        let r = Ref::Doi(Doi("10.1234/example".to_string()));
        let plan = build_fetch_plan(&r, &temp_root());
        // All four allowlist entries must be present (matches the
        // production `build_http_client` composition).
        assert_eq!(
            plan.redirect_allowlists_loaded,
            vec!["crossref", "unpaywall", "arxiv", "oa-publisher"]
        );
    }
}
