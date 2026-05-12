//! doiget MCP server (stdio).
//!
//! Phase 3 foundation. JSON-RPC framing is provided by the official `rmcp`
//! SDK with stdio-only transport (`transport-io`). See ADR-0001 for the
//! permanence of the stdio-only choice and `docs/MCP_TOOLS.md` for the
//! tool surface contract.
//!
//! This module ships the rmcp wiring + the always-on tools that prove the
//! foundation:
//!
//! - `doiget_health` — operational sanity check.
//! - `doiget_capability_profile` — reports the runtime [`CapabilityProfile`].
//! - `doiget_metadata_only` — DOI / arXiv id metadata resolution; in
//!   Phase 1 only the `dry_run: true` path is wired (the live metadata
//!   fetch lands in a follow-up PR).
//!
//! The remaining tools named in `docs/MCP_TOOLS.md` (`doiget_resolve_paper`,
//! `doiget_fetch_paper`, `doiget_batch_fetch`, `doiget_info`,
//! `doiget_search_local`, `doiget_list_recent`, `doiget_paper_pdf_path`)
//! land in follow-up PRs. The exact count is intentionally left unstated
//! in this docstring so it does not rot as tools land.
//!
//! # Stdout safety
//!
//! Per `docs/SECURITY.md` §3, stdout is reserved for JSON-RPC frames only.
//! `clippy::print_stdout` is denied below at the crate root; tracing must
//! be redirected to stderr by the binary entry point. The `doiget-cli`
//! `main()` wires `tracing_subscriber::fmt().with_writer(std::io::stderr)`
//! before calling [`Server::run`].

#![warn(missing_docs)]
#![forbid(unsafe_code)]
// Stricter than the workspace lint: doiget-mcp must NEVER write to stdout
// outside JSON-RPC frames. See docs/SECURITY.md §3.
#![deny(clippy::print_stdout)]

use camino::Utf8PathBuf;

use doiget_core::dry_run::{build_dry_run_envelope, build_fetch_plan};
use doiget_core::{CapabilityProfile, Ref, SCHEMA_VERSION, VERSION};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Implementation, ProtocolVersion, ServerCapabilities, ServerInfo},
    schemars::{self, JsonSchema},
    tool, tool_handler, tool_router,
    transport::stdio,
    ErrorData, ServerHandler, ServiceExt,
};
use serde::Deserialize;
use serde_json::{json, Value};

/// MCP server handle. Owns the resolved [`CapabilityProfile`] plus the
/// statically-built rmcp tool router.
///
/// Construct via [`Server::new`] and drive with [`Server::run`]:
///
/// ```no_run
/// # async fn demo() -> anyhow::Result<()> {
/// let profile = doiget_core::CapabilityProfile::from_env()?;
/// doiget_mcp::Server::new(profile).run().await
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct Server {
    profile: CapabilityProfile,
    /// rmcp tool dispatch table, populated by the `#[tool_router]` macro
    /// on the inherent impl block below. `#[tool_handler]` (in its default
    /// configuration) uses the associated fn `Self::tool_router()` rather
    /// than this field, but holding the router on the struct keeps the
    /// type valid for `router = self.tool_router` if a future refactor
    /// (e.g., merging multiple tool routers) needs that form.
    #[allow(dead_code)]
    tool_router: ToolRouter<Server>,
}

#[tool_router]
impl Server {
    /// Construct a server with the given runtime capability profile.
    pub fn new(profile: CapabilityProfile) -> Self {
        Self {
            profile,
            tool_router: Self::tool_router(),
        }
    }

    /// Run the MCP server until stdin reaches EOF.
    ///
    /// Returns once the underlying rmcp service loop exits — that happens
    /// either when the peer (MCP host) closes stdin, or when the service
    /// is cancelled via the rmcp lifecycle. Callers are expected to invoke
    /// this from the binary's `main()` and surface any error via the
    /// process exit code.
    ///
    /// # Errors
    ///
    /// Returns an error if rmcp service initialization fails (e.g. the
    /// stdio handles are unavailable) or if the service loop terminates
    /// abnormally.
    pub async fn run(self) -> anyhow::Result<()> {
        // `serve` consumes `self` (rmcp's `ServiceExt::serve` signature).
        // `stdio()` returns `(tokio::io::Stdin, tokio::io::Stdout)`, which
        // implements `IntoTransport<RoleServer, _, _>` thanks to the
        // `transport-io` feature.
        let service = self.serve(stdio()).await?;
        service.waiting().await?;
        Ok(())
    }

    /// `doiget_health` — operational sanity check.
    ///
    /// Per `docs/MCP_TOOLS.md` §1, this tool MUST exist for the smoke
    /// test to validate the rmcp wiring. The output shape is
    /// `{ ok: true, version, schema_version, store_writable }`.
    ///
    /// `store_writable` is a best-effort probe: we attempt
    /// `std::fs::create_dir_all(<store_root>)` and report whether it
    /// succeeded. Per `docs/SECURITY.md` §1.5 directory creation is
    /// idempotent and is not user-data write — this matches the spec's
    /// "read-only check" framing.
    #[tool(
        description = "WHEN TO USE: Operational sanity check for the doiget MCP server.\n\
                       INPUTS: none.\n\
                       OUTPUTS: { ok: true, version, schema_version, store_writable }.\n\
                       COSTS: <1 ms.\n\
                       SIDE EFFECTS: idempotent mkdir of the store root.\n\
                       LIMITS: none."
    )]
    async fn doiget_health(&self) -> Result<CallToolResult, ErrorData> {
        let store_root = resolve_store_root();
        let store_writable = match &store_root {
            Some(p) => probe_store_writable(p),
            None => false,
        };

        let payload = json!({
            "ok": true,
            "version": VERSION,
            "schema_version": SCHEMA_VERSION,
            "store_writable": store_writable,
        });
        Ok(CallToolResult::structured(payload))
    }

    /// `doiget_capability_profile` — report the runtime capability tiers.
    ///
    /// Per `docs/MCP_TOOLS.md` §7 ("Capability awareness"), agents call
    /// this first to plan whether a TDM-class fetch will succeed. The
    /// output shape is `{ tier_1: [...], tier_2: [...], tier_3: [...] }`
    /// alongside boolean roll-ups and the (always-5) rate cap.
    #[tool(
        description = "WHEN TO USE: Determine which sources the running doiget instance is allowed to use.\n\
                       INPUTS: none.\n\
                       OUTPUTS: { ok: true, tier_1, tier_2, tier_3, oa_enabled, tdm_enabled, rate_limit_per_sec }.\n\
                       COSTS: <1 ms.\n\
                       SIDE EFFECTS: none.\n\
                       LIMITS: none."
    )]
    async fn doiget_capability_profile(&self) -> Result<CallToolResult, ErrorData> {
        let payload = capability_profile_to_json(&self.profile);
        Ok(CallToolResult::structured(payload))
    }

    /// `doiget_metadata_only` — resolve metadata for a DOI / arXiv id
    /// without paying for or being noticed by a PDF download.
    ///
    /// Per `docs/MCP_TOOLS.md` §11 (NORMATIVE).
    ///
    /// **Phase 1 status:** only the `dry_run: true` path is wired —
    /// the orchestrator that performs a real metadata-only fetch (with
    /// the `metadata-only` provenance tag and no PDF leg) lands in a
    /// follow-up PR. The non-dry-run path returns
    /// `{ok:false, error:{code:"INTERNAL_ERROR", ...}}` with a clear
    /// message. The dry-run path is the user-visible value-add this PR
    /// ships, per ADR-0022 (the `dry_run` companion ADR).
    #[tool(
        description = "WHEN TO USE: User wants metadata for a DOI / arXiv id without paying for or being noticed by a PDF download.\n\
                       INPUTS: ref (DOI or arXiv id), dry_run (optional bool).\n\
                       OUTPUTS: { ok: true, ref, source, license?, oa_url, metadata } OR { ok: true, dry_run: true, ref, plan, rate_limit_budget } OR { ok:false, error }.\n\
                       COSTS: 1-2 s metadata round-trip (or 0 when dry_run).\n\
                       SIDE EFFECTS: Appends a 'metadata-only' provenance row (unless dry_run). Writes the metadata TOML to the store. Never fetches PDF.\n\
                       LIMITS: Subject to the same rate cap as fetch_paper (5/sec). The OA URL is reported but never followed."
    )]
    async fn doiget_metadata_only(
        &self,
        Parameters(input): Parameters<MetadataOnlyInput>,
    ) -> Result<CallToolResult, ErrorData> {
        // Step 1: parse the ref. Failures collapse to INVALID_REF per
        // docs/ERRORS.md §2 / docs/PUBLIC_API.md §4.
        let ref_ = match Ref::parse(&input.ref_) {
            Ok(r) => r,
            Err(e) => {
                return Ok(CallToolResult::structured(metadata_only_error_envelope(
                    "INVALID_REF",
                    &format!("invalid ref: {e}"),
                )));
            }
        };

        // Step 2: dry-run branch (ADR-0022 §2). Build the same envelope
        // the CLI emits and route via JSON-RPC. NO network, NO store
        // write, NO provenance row.
        if input.dry_run {
            // Use the same store-root resolver as `doiget_health` so the
            // path projections in `plan.target_*` match what the live
            // fetch would write to. When neither HOME nor USERPROFILE
            // resolves (locked-down hosts), fall back to a sentinel
            // path so the preview still has a complete shape — the
            // dry-run is a preview, not a writability probe.
            let store_root = resolve_store_root().unwrap_or_else(|| Utf8PathBuf::from("./papers"));
            let plan = build_fetch_plan(&ref_, &store_root);
            return Ok(CallToolResult::structured(build_dry_run_envelope(
                &ref_, &plan,
            )));
        }

        // Step 3: non-dry-run path. The real metadata-only orchestrator
        // lands in a follow-up PR; surface a clear INTERNAL_ERROR
        // envelope so an agent doesn't get a confusing default reply.
        // TODO(phase-1.x): wire the metadata-only orchestrator.
        // It should: dispatch Crossref + Unpaywall (DOI) or arXiv-meta
        // (arXiv), write the metadata TOML, append a `Fetch` row tagged
        // `metadata-only`, and return `{ok:true, ref, source, license?,
        // oa_url, metadata}` per docs/MCP_TOOLS.md §11. The OA URL is
        // reported but never followed (that is the contract that
        // distinguishes this tool from `doiget_fetch_paper`).
        Ok(CallToolResult::structured(metadata_only_error_envelope(
            "INTERNAL_ERROR",
            "metadata_only is not yet wired in Phase 1; only dry_run is supported",
        )))
    }
}

// ---------------------------------------------------------------------------
// doiget_metadata_only — input schema
// ---------------------------------------------------------------------------

/// JSON-schema-derived input for [`Server::doiget_metadata_only`].
///
/// Mirrors `docs/MCP_TOOLS.md` §11 `inputSchema`. The Rust field name
/// `ref_` is renamed on the wire to `ref` (the JSON key the spec uses,
/// reserved in Rust as the `ref` keyword) via `#[serde(rename = "ref")]`.
/// The matching `#[schemars(rename = "ref")]` keeps the generated JSON
/// schema field name aligned with the wire form.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct MetadataOnlyInput {
    /// DOI or arXiv id. Validated via `Ref::parse`; failures surface as
    /// `INVALID_REF` per `docs/ERRORS.md`.
    #[serde(rename = "ref")]
    #[schemars(rename = "ref")]
    pub ref_: String,
    /// When `true`, returns a [`FetchPlan`](doiget_core::dry_run::FetchPlan)
    /// preview without touching the network or writing anything (ADR-0022).
    /// Defaults to `false` (the production metadata-only path, currently
    /// stubbed in Phase 1). Type is plain `bool` (not `Option<bool>`) so the
    /// generated JSON schema declares `"type": "boolean"` and a wire `null`
    /// is rejected at deserialize time — agents that intend "no preview"
    /// either omit the field or pass `false`.
    #[serde(default)]
    pub dry_run: bool,
}

/// Build the `{ok:false, error:{...}}` envelope used by
/// `doiget_metadata_only` for both INVALID_REF (bad input) and
/// INTERNAL_ERROR (Phase 1 stub) cases. Mirrors the wire shape from
/// `docs/MCP_TOOLS.md` §5; `denial_context` is omitted (these failure
/// modes do not produce one — see `docs/ERRORS.md` §3.1).
fn metadata_only_error_envelope(code: &str, message: &str) -> Value {
    json!({
        "ok": false,
        "error": {
            "code": code,
            "message": message,
            // denial_context is intentionally absent for these envelope
            // shapes (parse-error / internal-error); ADR-0023 §3 says
            // consumers MUST tolerate the field being absent.
        },
    })
}

// `tool_handler` wires the router into rmcp's `ServerHandler` trait — it
// generates `call_tool`, `list_tools`, and `get_tool` from
// `Self::tool_router()`. We provide `get_info` ourselves so the server
// identifies itself as `name = "doiget"`, advertises
// `protocolVersion = "2024-11-05"` (the version the smoke test asserts),
// and includes capability-aware `instructions`.
#[tool_handler]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerInfo {
        // Both `ServerInfo` and `Implementation` are `#[non_exhaustive]`
        // in rmcp 1.6, so we go through the public builders rather than
        // struct-literal construction.
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new("doiget", VERSION))
            .with_instructions(format!(
                "doiget v{VERSION} \u{2014} Open Access paper fetcher (stdio MCP). \
                 Tier 1 sources are always-on; Tier 2/3 require build features and \
                 env-var grants. Call `doiget_capability_profile` for the runtime \
                 view; call `doiget_health` for an operational sanity check."
            ))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolve the on-disk store root using the same precedence the CLI
/// applies (`docs/CONFIG.md` §4):
///
/// 1. `DOIGET_STORE_ROOT` env var (when non-empty).
/// 2. `$HOME/papers` (POSIX) or `%USERPROFILE%\papers` (Windows).
///
/// Returns `None` when neither hook resolves — e.g. a locked-down host
/// with no `HOME` and no `USERPROFILE`. Callers downgrade that to
/// `store_writable: false` rather than erroring the whole tool call.
///
/// # Why duplicate the CLI logic?
///
/// `doiget-mcp` cannot depend on `doiget-cli` — that would invert the
/// `doiget-cli -> doiget-mcp` wiring established by this PR and pull
/// `clap` etc. into the MCP crate. Lifting this helper into `doiget-core`
/// is a viable Phase-3 follow-up but is out of scope for this foundation.
fn resolve_store_root() -> Option<Utf8PathBuf> {
    if let Ok(s) = std::env::var("DOIGET_STORE_ROOT") {
        if !s.is_empty() {
            return Some(Utf8PathBuf::from(s));
        }
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()?;
    Some(Utf8PathBuf::from(home).join("papers"))
}

/// Best-effort writability probe for the resolved store root.
///
/// Returns `true` iff `std::fs::create_dir_all(path)` succeeds. The probe
/// is idempotent (per `docs/SECURITY.md` §1.5 — directory creation is
/// not considered a user-data write) and non-destructive.
fn probe_store_writable(path: &camino::Utf8Path) -> bool {
    std::fs::create_dir_all(path).is_ok()
}

/// Serialize a [`CapabilityProfile`] to the JSON surface defined by
/// `docs/MCP_TOOLS.md` §7.
///
/// - Tier 1 is always `["arxiv", "crossref", "unpaywall"]` (sorted for
///   deterministic output).
/// - Tier 2 reflects the `MetadataAccess` booleans.
/// - Tier 3 reflects which `tdm_*` slots are `Some(...)`.
fn capability_profile_to_json(profile: &CapabilityProfile) -> Value {
    let tier_1 = vec!["arxiv", "crossref", "unpaywall"];

    let mut tier_2: Vec<&str> = Vec::new();
    if profile.metadata.openalex {
        tier_2.push("openalex");
    }
    if profile.metadata.semantic_scholar {
        tier_2.push("semantic_scholar");
    }
    if profile.metadata.doaj {
        tier_2.push("doaj");
    }

    let mut tier_3: Vec<&str> = Vec::new();
    if profile.tdm_elsevier.is_some() {
        tier_3.push("tdm-elsevier");
    }
    if profile.tdm_aps.is_some() {
        tier_3.push("tdm-aps");
    }
    if profile.tdm_springer.is_some() {
        tier_3.push("tdm-springer");
    }

    let tdm_enabled = !tier_3.is_empty();

    json!({
        "ok": true,
        "tier_1": tier_1,
        "tier_2": tier_2,
        "tier_3": tier_3,
        "oa_enabled": true,
        "tdm_enabled": tdm_enabled,
        "tdm_elsevier": profile.tdm_elsevier.is_some(),
        "tdm_aps": profile.tdm_aps.is_some(),
        "tdm_springer": profile.tdm_springer.is_some(),
        "rate_limit_per_sec": profile.rate_limits.max_fetches_per_second(),
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn capability_profile_to_json_clean_env_shape() {
        // A clean (no env) profile reports Tier 1 only. We can't construct
        // CapabilityProfile from outside doiget-core (it's #[non_exhaustive]),
        // so we drive `from_env()` in the cleanest state we can guarantee
        // and assert the always-true invariants.
        let profile = CapabilityProfile::from_env().expect("clean env never errors");
        let v = capability_profile_to_json(&profile);
        assert_eq!(v["ok"], true);
        assert_eq!(v["tier_1"], json!(["arxiv", "crossref", "unpaywall"]));
        assert_eq!(v["oa_enabled"], true);
        assert_eq!(v["rate_limit_per_sec"], 5.0);
    }

    #[test]
    fn resolve_store_root_returns_some_on_normal_host() {
        // On any realistic POSIX or Windows host either HOME or
        // USERPROFILE is set, so resolve_store_root is `Some(_)` even
        // without DOIGET_STORE_ROOT. We deliberately don't mutate env
        // (the test process is shared with other tests).
        if std::env::var_os("HOME").is_some() || std::env::var_os("USERPROFILE").is_some() {
            assert!(resolve_store_root().is_some());
        }
    }
}
