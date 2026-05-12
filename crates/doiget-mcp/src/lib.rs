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
//! - `doiget_metadata_only` — DOI / arXiv id metadata resolution. Both
//!   the `dry_run: true` preview path (ADR-0022) and the live
//!   non-dry-run path (Slice 1: dispatches through
//!   [`doiget_core::orchestrator::metadata_only`]) are wired. The
//!   tool MUST NOT call `HttpClient::fetch_pdf` — that contract is
//!   enforced by the orchestrator and the posture-lint workflow.
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

use std::sync::Arc;

use camino::Utf8PathBuf;

use doiget_core::dry_run::{
    build_dry_run_envelope, build_fetch_plan, rate_limit_budget as core_rate_limit_budget,
};
use doiget_core::http::{oa_publisher_allowlist, tier_1_allowlist, HttpClient};
use doiget_core::orchestrator::{
    batch_fetch as core_batch_fetch, batch_fetch_plans, fetch_paper as core_fetch_paper,
    metadata_only, FetchPaperOutcome, MetadataOnlyOutcome,
};
use doiget_core::provenance::{Capability, LogEvent, LogResult, ProvenanceLog, RowInput};
use doiget_core::rate_limiter::RateLimiter;
use doiget_core::source::{FetchContext, FetchError};
use doiget_core::store::FsStore;
use doiget_core::{
    CapabilityProfile, DenialContext, ErrorCode, RateLimits, Ref, MAX_BATCH_REFS, SCHEMA_VERSION,
    VERSION,
};
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
    /// Slice 1 wired both branches:
    ///
    /// - `dry_run: true` → builds a [`FetchPlan`] preview and returns
    ///   it without touching the network, store, or provenance log
    ///   (ADR-0022 §2).
    /// - `dry_run: false` (default) → dispatches through
    ///   [`doiget_core::orchestrator::metadata_only`]:
    ///   - DOI → Crossref (`message` metadata + OA URL via
    ///     `message.link[]`), with Unpaywall as a fallback when
    ///     Crossref fails.
    ///   - arXiv → Atom feed at
    ///     `https://export.arxiv.org/api/query?id_list=<id>` via
    ///     [`doiget_core::sources::arxiv::ArxivSource::fetch_metadata_only`].
    ///
    /// In neither branch does this tool call
    /// `HttpClient::fetch_pdf` — the spec contract for
    /// `doiget_metadata_only` (`docs/MCP_TOOLS.md` §11) is
    /// posture-lint-enforced.
    ///
    /// [`FetchPlan`]: doiget_core::dry_run::FetchPlan
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
                    ErrorCode::InvalidRef,
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

        // Step 3: non-dry-run path. Dispatch through the
        // `metadata_only` orchestrator (Slice 1). The orchestrator owns
        // source selection and per-leg politeness; we own the per-call
        // session boundary (SessionStart / SessionEnd bookend rows) and
        // the wire envelope shape (`docs/MCP_TOOLS.md` §11).
        let ctx = match build_fetch_context() {
            Ok(c) => c,
            Err(e) => {
                return Ok(CallToolResult::structured(metadata_only_error_envelope(
                    ErrorCode::InternalError,
                    &format!("metadata-only context initialization failed: {e}"),
                )));
            }
        };

        // SessionStart bookend (mirrors the CLI orchestrator pattern in
        // `crates/doiget-cli/src/commands/fetch.rs::FetchHarness::log_session_start`).
        // A log-append failure here is fail-closed per
        // `docs/PROVENANCE_LOG.md` §5 — abort the call.
        if let Err(e) = ctx.log.append(RowInput {
            event: LogEvent::SessionStart,
            result: LogResult::Ok,
            capability: Capability::Metadata,
            ref_: Some(input.ref_.as_str()),
            source: None,
            error_code: None,
            size_bytes: None,
            license: None,
            store_path: None,
            // Session bookend rows carry no audit identity — they
            // bracket the call, they do not mint a CanonicalRef
            // (ADR-0021 §1; ADR-0024).
            canonical_digest: None,
        }) {
            return Ok(CallToolResult::structured(metadata_only_error_envelope(
                ErrorCode::LogError,
                &format!("SessionStart append failed: {e}"),
            )));
        }

        let outcome = metadata_only(&ref_, &self.profile, &ctx).await;

        // SessionEnd bookend. Best-effort: if this append fails we still
        // surface the orchestrator's outcome (a fresh log error here
        // would mask the more informative orchestrator error).
        let session_ok = outcome.is_ok();
        let _ = ctx.log.append(RowInput {
            event: LogEvent::SessionEnd,
            result: if session_ok {
                LogResult::Ok
            } else {
                LogResult::Err
            },
            capability: Capability::Metadata,
            ref_: Some(input.ref_.as_str()),
            source: None,
            error_code: None,
            size_bytes: None,
            license: None,
            store_path: None,
            // Session bookend rows carry no audit identity — they
            // bracket the call, they do not mint a CanonicalRef
            // (ADR-0021 §1; ADR-0024).
            canonical_digest: None,
        });

        match outcome {
            Ok(o) => Ok(CallToolResult::structured(metadata_only_success_envelope(
                &o,
                &input.ref_,
            ))),
            Err(e) => Ok(CallToolResult::structured(
                metadata_only_fetch_error_envelope(&e, &input.ref_),
            )),
        }
    }

    /// `doiget_fetch_paper` — resolve and download a single PDF.
    ///
    /// Per `docs/MCP_TOOLS.md` §4 (NORMATIVE). Slice 2 wires this to
    /// the live [`core_fetch_paper`] orchestrator and the dry-run
    /// preview path (ADR-0022 §2).
    ///
    /// Per-call boundary semantics mirror the
    /// [`doiget_metadata_only`](Self::doiget_metadata_only) tool: the
    /// MCP server owns the SessionStart / SessionEnd bookend rows; each
    /// consulted [`Source`](doiget_core::source::Source) impl emits its
    /// own `LogEvent::Fetch` row inside the orchestrator. A successful
    /// `StoreWrite` row is also emitted by the orchestrator.
    #[tool(
        description = "WHEN TO USE: User wants to download a paper PDF given a DOI or arXiv id.\n\
                       INPUTS: ref (DOI or arXiv id), dry_run (optional bool).\n\
                       OUTPUTS: { ok: true, ref, source, path, license, size_bytes, schema_version } OR { ok: true, dry_run: true, ref, plan, rate_limit_budget } OR { ok:false, ref, error }.\n\
                       COSTS: 1-3 s network call (or 0 when dry_run). May fail if not Open Access.\n\
                       SIDE EFFECTS: Writes PDF (or metadata-only TOML) to the store. Appends a row to the provenance log (unless dry_run).\n\
                       LIMITS: Max 5 fetches/sec (global). Use doiget_batch_fetch for >5 refs."
    )]
    async fn doiget_fetch_paper(
        &self,
        Parameters(input): Parameters<FetchPaperInput>,
    ) -> Result<CallToolResult, ErrorData> {
        // Step 1: parse the ref. Failures collapse to INVALID_REF.
        let ref_ = match Ref::parse(&input.ref_) {
            Ok(r) => r,
            Err(e) => {
                return Ok(CallToolResult::structured(fetch_paper_error_envelope(
                    Some(&input.ref_),
                    ErrorCode::InvalidRef,
                    &format!("invalid ref: {e}"),
                )));
            }
        };

        // Step 2: dry-run branch (ADR-0022 §2).
        if input.dry_run {
            let store_root = resolve_store_root().unwrap_or_else(|| Utf8PathBuf::from("./papers"));
            let plan = build_fetch_plan(&ref_, &store_root);
            return Ok(CallToolResult::structured(build_dry_run_envelope(
                &ref_, &plan,
            )));
        }

        // Step 3: non-dry-run path. Build foundation modules + open
        // FsStore + dispatch through core orchestrator.
        let ctx = match build_fetch_context() {
            Ok(c) => c,
            Err(e) => {
                return Ok(CallToolResult::structured(fetch_paper_error_envelope(
                    Some(&input.ref_),
                    ErrorCode::InternalError,
                    &format!("fetch-paper context initialization failed: {e}"),
                )));
            }
        };
        let store_root = match resolve_store_root() {
            Some(p) => p,
            None => {
                return Ok(CallToolResult::structured(fetch_paper_error_envelope(
                    Some(&input.ref_),
                    ErrorCode::InternalError,
                    "could not resolve store root (neither DOIGET_STORE_ROOT, HOME, nor USERPROFILE is set)",
                )));
            }
        };
        let store = match FsStore::new(store_root.clone()) {
            Ok(s) => s,
            Err(e) => {
                return Ok(CallToolResult::structured(fetch_paper_error_envelope(
                    Some(&input.ref_),
                    ErrorCode::StoreError,
                    &format!("opening store at {store_root}: {e}"),
                )));
            }
        };

        // SessionStart bookend (mirrors `doiget_metadata_only`).
        if let Err(e) = ctx.log.append(RowInput {
            event: LogEvent::SessionStart,
            result: LogResult::Ok,
            capability: Capability::Oa,
            ref_: Some(input.ref_.as_str()),
            source: None,
            error_code: None,
            size_bytes: None,
            license: None,
            store_path: None,
            // Session bookend rows carry no audit identity — they
            // bracket the call, they do not mint a CanonicalRef
            // (ADR-0021 §1; ADR-0024).
            canonical_digest: None,
        }) {
            return Ok(CallToolResult::structured(fetch_paper_error_envelope(
                Some(&input.ref_),
                ErrorCode::LogError,
                &format!("SessionStart append failed: {e}"),
            )));
        }

        let outcome = core_fetch_paper(&ref_, &self.profile, &ctx, &store, &store_root).await;

        let session_ok = outcome.is_ok();
        let _ = ctx.log.append(RowInput {
            event: LogEvent::SessionEnd,
            result: if session_ok {
                LogResult::Ok
            } else {
                LogResult::Err
            },
            capability: Capability::Oa,
            ref_: Some(input.ref_.as_str()),
            source: None,
            error_code: None,
            size_bytes: None,
            license: None,
            store_path: None,
            // Session bookend rows carry no audit identity — they
            // bracket the call, they do not mint a CanonicalRef
            // (ADR-0021 §1; ADR-0024).
            canonical_digest: None,
        });

        match outcome {
            Ok(o) => Ok(CallToolResult::structured(fetch_paper_success_envelope(
                &o,
                &input.ref_,
            ))),
            Err(e) => Ok(CallToolResult::structured(
                fetch_paper_fetch_error_envelope(&e, &input.ref_),
            )),
        }
    }

    /// `doiget_batch_fetch` — fetch up to [`MAX_BATCH_REFS`] refs in
    /// one call.
    ///
    /// Per `docs/MCP_TOOLS.md` §1 (NORMATIVE Phase 3 baseline).
    ///
    /// Per-ref outcomes are independent — a failure on one ref does NOT
    /// abort sibling refs. The wire envelope is
    /// `{ok:true, results: [...]}` where each entry has `{ref, ok, ...}`.
    /// Only whole-call failures (INVALID_REF on a malformed input, the
    /// over-cap [`FetchError::TooManyRefs`] / TOO_MANY_REFS surfaced as
    /// INVALID_REF, or context initialization) emit the
    /// `{ok:false, error:{...}}` envelope.
    #[tool(
        description = "WHEN TO USE: User wants to fetch many papers in one call (up to 100).\n\
                       INPUTS: refs (array of up to 100 DOIs / arXiv ids), dry_run (optional bool).\n\
                       OUTPUTS: { ok: true, results: [{ref, ok, ...}] } OR { ok: true, dry_run: true, plans: [{ref, plan, rate_limit_budget}] } OR { ok:false, error }.\n\
                       COSTS: 1-3 s per ref, bounded by the 5/sec global rate cap.\n\
                       SIDE EFFECTS: Writes PDFs / metadata TOMLs to the store (unless dry_run). Appends one provenance row per attempt.\n\
                       LIMITS: Max 100 refs per call (TOO_MANY_REFS otherwise). Per-ref errors are reported in `results` and do NOT fail the whole call."
    )]
    async fn doiget_batch_fetch(
        &self,
        Parameters(input): Parameters<BatchFetchInput>,
    ) -> Result<CallToolResult, ErrorData> {
        // Step 1: enforce the cap before any parsing.
        if input.refs.len() > MAX_BATCH_REFS {
            return Ok(CallToolResult::structured(batch_fetch_error_envelope(
                ErrorCode::InvalidRef,
                &format!(
                    "too many refs: got {}, max {} (TOO_MANY_REFS)",
                    input.refs.len(),
                    MAX_BATCH_REFS
                ),
            )));
        }

        // Step 2: parse every ref up-front. Any malformed ref aborts
        // the whole call with INVALID_REF (per the spec: bulk parse is
        // all-or-nothing).
        let mut parsed: Vec<Ref> = Vec::with_capacity(input.refs.len());
        for raw in &input.refs {
            match Ref::parse(raw) {
                Ok(r) => parsed.push(r),
                Err(e) => {
                    return Ok(CallToolResult::structured(batch_fetch_error_envelope(
                        ErrorCode::InvalidRef,
                        &format!("invalid ref {raw:?}: {e}"),
                    )));
                }
            }
        }

        // Step 3: dry-run branch — one plan per ref.
        if input.dry_run {
            let store_root = resolve_store_root().unwrap_or_else(|| Utf8PathBuf::from("./papers"));
            let plans = match batch_fetch_plans(&parsed, &store_root) {
                Ok(p) => p,
                Err(e) => {
                    return Ok(CallToolResult::structured(batch_fetch_error_envelope(
                        ErrorCode::from(e),
                        "batch dry-run plan build failed",
                    )));
                }
            };
            let envelope = build_batch_dry_run_envelope(&plans);
            return Ok(CallToolResult::structured(envelope));
        }

        // Step 4: non-dry-run — stand up the shared FetchContext +
        // store, emit a single SessionStart row, fan out via the core
        // orchestrator.
        let ctx = match build_fetch_context() {
            Ok(c) => c,
            Err(e) => {
                return Ok(CallToolResult::structured(batch_fetch_error_envelope(
                    ErrorCode::InternalError,
                    &format!("batch-fetch context initialization failed: {e}"),
                )));
            }
        };
        let store_root = match resolve_store_root() {
            Some(p) => p,
            None => {
                return Ok(CallToolResult::structured(batch_fetch_error_envelope(
                    ErrorCode::InternalError,
                    "could not resolve store root (neither DOIGET_STORE_ROOT, HOME, nor USERPROFILE is set)",
                )));
            }
        };
        let store = match FsStore::new(store_root.clone()) {
            Ok(s) => s,
            Err(e) => {
                return Ok(CallToolResult::structured(batch_fetch_error_envelope(
                    ErrorCode::StoreError,
                    &format!("opening store at {store_root}: {e}"),
                )));
            }
        };

        if let Err(e) = ctx.log.append(RowInput {
            event: LogEvent::SessionStart,
            result: LogResult::Ok,
            capability: Capability::Oa,
            ref_: None,
            source: None,
            error_code: None,
            size_bytes: None,
            license: None,
            store_path: None,
            // Session bookend rows carry no audit identity — they
            // bracket the call, they do not mint a CanonicalRef
            // (ADR-0021 §1; ADR-0024).
            canonical_digest: None,
        }) {
            return Ok(CallToolResult::structured(batch_fetch_error_envelope(
                ErrorCode::LogError,
                &format!("SessionStart append failed: {e}"),
            )));
        }

        let batch_outcome =
            core_batch_fetch(&parsed, &self.profile, &ctx, &store, &store_root).await;

        let session_ok = batch_outcome
            .as_ref()
            .map(|b| b.results.iter().all(|r| r.outcome.is_ok()))
            .unwrap_or(false);

        let _ = ctx.log.append(RowInput {
            event: LogEvent::SessionEnd,
            result: if session_ok {
                LogResult::Ok
            } else {
                LogResult::Err
            },
            capability: Capability::Oa,
            ref_: None,
            source: None,
            error_code: None,
            size_bytes: None,
            license: None,
            store_path: None,
            // Session bookend rows carry no audit identity — they
            // bracket the call, they do not mint a CanonicalRef
            // (ADR-0021 §1; ADR-0024).
            canonical_digest: None,
        });

        match batch_outcome {
            Ok(b) => Ok(CallToolResult::structured(batch_fetch_success_envelope(
                &b,
                &input.refs,
            ))),
            Err(e) => Ok(CallToolResult::structured(batch_fetch_error_envelope(
                ErrorCode::from(e),
                "batch-fetch orchestrator failed",
            ))),
        }
    }
}

// ---------------------------------------------------------------------------
// doiget_metadata_only — input schema
// ---------------------------------------------------------------------------

/// JSON-schema-derived input for the `doiget_metadata_only` MCP tool.
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
/// `doiget_metadata_only` for input-shape failures (e.g. `INVALID_REF`
/// from `Ref::parse`) and internal initialization failures (e.g.
/// `INTERNAL_ERROR` when the foundation modules cannot be constructed).
/// Mirrors the wire shape from `docs/MCP_TOOLS.md` §5; `denial_context`
/// is omitted (these failure modes do not produce one — see
/// `docs/ERRORS.md` §3.1).
///
/// The `code` parameter is typed as [`ErrorCode`] (not `&str`) so the
/// closed enum is the single source of truth for the wire token — the
/// I6 lesson from PR #84's multi-agent review: free-form string codes
/// can drift from `ErrorCode`'s SCREAMING_SNAKE_CASE rendering without
/// the compiler noticing.
fn metadata_only_error_envelope(code: ErrorCode, message: &str) -> Value {
    json!({
        "ok": false,
        "error": {
            "code": code,
            "message": message,
            // denial_context is intentionally absent for these envelope
            // shapes (parse-error / not-implemented); ADR-0023 §3 says
            // consumers MUST tolerate the field being absent.
        },
    })
}

/// Build the `{ok:true, ...}` success envelope per `docs/MCP_TOOLS.md`
/// §11 (the `MetadataOnlyResult` type alias). `oa_url` is always
/// emitted (as `null` when the resolver did not surface one) so agents
/// can pattern-match on the field without checking for absence.
fn metadata_only_success_envelope(outcome: &MetadataOnlyOutcome, ref_str: &str) -> Value {
    json!({
        "ok": true,
        "ref": ref_str,
        "source": outcome.source,
        // ADR-0021 §4: surface the resolver_profile under which the
        // canonical-digest was minted. In Slice 4 this equals `source`
        // verbatim; the field is kept distinct so future slices can
        // decouple the two when overlapping resolvers ship.
        "resolver_profile": outcome.resolver_profile,
        "license": outcome.license,
        "oa_url": outcome.oa_url,
        "metadata": outcome.metadata,
        "schema_version": SCHEMA_VERSION,
    })
}

/// Build the `{ok:false, error:{code, message, denial_context?}}`
/// envelope for orchestrator failures. Maps the [`FetchError`] to the
/// closed [`ErrorCode`] set via the existing
/// `From<FetchError> for ErrorCode` impl, and produces the optional
/// structured `denial_context` channel via
/// `From<&FetchError> for Option<DenialContext>` (ADR-0023 §4).
fn metadata_only_fetch_error_envelope(err: &FetchError, ref_str: &str) -> Value {
    // `FetchError` is not `Clone`; we can't move out of `&err`. Use
    // the existing `From` boundary indirectly by re-mapping via a
    // match on `&FetchError`. This duplicates a few lines of the
    // `From<FetchError> for ErrorCode` impl but avoids cloning the
    // underlying transport error.
    let code: ErrorCode = match err {
        FetchError::NotEligible { .. } => ErrorCode::CapabilityDenied,
        FetchError::NoOaAvailable => ErrorCode::NoOaAvailable,
        FetchError::Http(_) => ErrorCode::NetworkError,
        FetchError::Log(_) => ErrorCode::LogError,
        FetchError::InvalidRef(_) => ErrorCode::InvalidRef,
        FetchError::SourceSchema { .. } => ErrorCode::InternalError,
        // `FetchError` is `#[non_exhaustive]`; this wildcard pins
        // future variants to `INTERNAL_ERROR` until they get an
        // explicit mapping. Mirrors the conservative default in
        // `doiget-core`'s `From<FetchError> for ErrorCode` impl.
        _ => ErrorCode::InternalError,
    };
    let denial: Option<DenialContext> = err.into();
    let message = err.to_string();

    let mut error_obj = serde_json::Map::new();
    error_obj.insert("code".into(), json!(code));
    error_obj.insert("message".into(), json!(message));
    if let Some(dc) = denial {
        // `DenialContext` is `Serialize` (`#[serde(deny_unknown_fields)]`,
        // optional fields). `serde_json::to_value` cannot fail on a
        // typed struct; the `unwrap_or` defensively returns `null` if a
        // future field becomes non-serializable (defensive only).
        error_obj.insert(
            "denial_context".into(),
            serde_json::to_value(&dc).unwrap_or(Value::Null),
        );
    }
    json!({
        "ok": false,
        "ref": ref_str,
        "error": error_obj,
    })
}

// ---------------------------------------------------------------------------
// doiget_fetch_paper — input schema + envelopes (Slice 2)
// ---------------------------------------------------------------------------

/// JSON-schema-derived input for `doiget_fetch_paper`. Same wire shape
/// as `MetadataOnlyInput` — just a different orchestrator target.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct FetchPaperInput {
    /// DOI or arXiv id; validated via `Ref::parse`.
    #[serde(rename = "ref")]
    #[schemars(rename = "ref")]
    pub ref_: String,
    /// When `true`, returns a [`FetchPlan`](doiget_core::dry_run::FetchPlan)
    /// preview without touching the network or writing anything
    /// (ADR-0022). Defaults to `false`.
    #[serde(default)]
    pub dry_run: bool,
}

/// Build the `{ok:true, ref, source, path, ...}` success envelope per
/// `docs/MCP_TOOLS.md` §5 `FetchResult`.
fn fetch_paper_success_envelope(outcome: &FetchPaperOutcome, ref_str: &str) -> Value {
    json!({
        "ok": true,
        "ref": ref_str,
        "source": outcome.source,
        // ADR-0021 §4 / ADR-0024: the audit-identity resolver under
        // which the canonical-digest for this fetch was minted.
        "resolver_profile": outcome.resolver_profile,
        "license": outcome.license,
        "path": outcome.path,
        "size_bytes": outcome.size_bytes,
        "schema_version": outcome.schema_version,
    })
}

/// Build the `{ok:false, ref, error:{code, message}}` envelope for
/// input-shape / context-init failures in `doiget_fetch_paper`.
fn fetch_paper_error_envelope(ref_str: Option<&str>, code: ErrorCode, message: &str) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("ok".into(), json!(false));
    if let Some(r) = ref_str {
        obj.insert("ref".into(), json!(r));
    }
    obj.insert(
        "error".into(),
        json!({
            "code": code,
            "message": message,
        }),
    );
    Value::Object(obj)
}

/// Build the `{ok:false, error:{code, message, denial_context?}}`
/// envelope for orchestrator failures in `doiget_fetch_paper`.
fn fetch_paper_fetch_error_envelope(err: &FetchError, ref_str: &str) -> Value {
    let code: ErrorCode = match err {
        FetchError::NotEligible { .. } => ErrorCode::CapabilityDenied,
        FetchError::NoOaAvailable => ErrorCode::NoOaAvailable,
        FetchError::Http(_) => ErrorCode::NetworkError,
        FetchError::Log(_) => ErrorCode::LogError,
        FetchError::InvalidRef(_) => ErrorCode::InvalidRef,
        FetchError::SourceSchema { .. } => ErrorCode::InternalError,
        FetchError::TooManyRefs { .. } => ErrorCode::InvalidRef,
        _ => ErrorCode::InternalError,
    };
    let denial: Option<DenialContext> = err.into();
    let mut error_obj = serde_json::Map::new();
    error_obj.insert("code".into(), json!(code));
    error_obj.insert("message".into(), json!(err.to_string()));
    if let Some(dc) = denial {
        error_obj.insert(
            "denial_context".into(),
            serde_json::to_value(&dc).unwrap_or(Value::Null),
        );
    }
    json!({
        "ok": false,
        "ref": ref_str,
        "error": error_obj,
    })
}

// ---------------------------------------------------------------------------
// doiget_batch_fetch — input schema + envelopes (Slice 2)
// ---------------------------------------------------------------------------

/// JSON-schema-derived input for `doiget_batch_fetch`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct BatchFetchInput {
    /// List of DOI / arXiv id refs. Max [`MAX_BATCH_REFS`] entries; the
    /// cap is enforced inside the tool handler so a 101st ref surfaces
    /// as an `INVALID_REF` envelope (rather than a JSON schema reject).
    pub refs: Vec<String>,
    /// When `true`, return one [`FetchPlan`](doiget_core::dry_run::FetchPlan)
    /// per ref without touching the network or store.
    #[serde(default)]
    pub dry_run: bool,
}

/// Build the `{ok:true, results: [...]}` envelope for a successful
/// batch call. Each per-ref outcome is independent: a network failure
/// on one ref shows up as `{ref, ok:false, error}` next to siblings'
/// `{ref, ok:true, ...}`. Mirrors the per-ref shape of
/// [`fetch_paper_success_envelope`] / [`fetch_paper_fetch_error_envelope`].
fn batch_fetch_success_envelope(
    batch: &doiget_core::orchestrator::BatchOutcome,
    raw_refs: &[String],
) -> Value {
    let results: Vec<Value> = batch
        .results
        .iter()
        .zip(raw_refs.iter())
        .map(|(entry, raw)| match &entry.outcome {
            Ok(outcome) => json!({
                "ref": raw,
                "ok": true,
                "source": outcome.source,
                "resolver_profile": outcome.resolver_profile,
                "license": outcome.license,
                "path": outcome.path,
                "size_bytes": outcome.size_bytes,
                "schema_version": outcome.schema_version,
            }),
            Err(err) => {
                let code: ErrorCode = match err {
                    FetchError::NotEligible { .. } => ErrorCode::CapabilityDenied,
                    FetchError::NoOaAvailable => ErrorCode::NoOaAvailable,
                    FetchError::Http(_) => ErrorCode::NetworkError,
                    FetchError::Log(_) => ErrorCode::LogError,
                    FetchError::InvalidRef(_) => ErrorCode::InvalidRef,
                    FetchError::SourceSchema { .. } => ErrorCode::InternalError,
                    FetchError::TooManyRefs { .. } => ErrorCode::InvalidRef,
                    _ => ErrorCode::InternalError,
                };
                let denial: Option<DenialContext> = err.into();
                let mut error_obj = serde_json::Map::new();
                error_obj.insert("code".into(), json!(code));
                error_obj.insert("message".into(), json!(err.to_string()));
                if let Some(dc) = denial {
                    error_obj.insert(
                        "denial_context".into(),
                        serde_json::to_value(&dc).unwrap_or(Value::Null),
                    );
                } else {
                    // Per the Slice 2 spec: transport (NETWORK_ERROR)
                    // entries carry `denial_context: null` so an agent
                    // can pattern-match on the field's presence rather
                    // than tolerating absence. This deliberately
                    // differs from the metadata-only envelope where
                    // the field is omitted entirely; the batch surface
                    // makes the null explicit because per-ref entries
                    // are uniform table rows in the agent's view.
                    error_obj.insert("denial_context".into(), Value::Null);
                }
                json!({
                    "ref": raw,
                    "ok": false,
                    "error": error_obj,
                })
            }
        })
        .collect();
    json!({
        "ok": true,
        "results": results,
    })
}

/// Build the dry-run envelope for `doiget_batch_fetch` —
/// `{ok:true, dry_run:true, plans:[{ref, plan, rate_limit_budget}]}`.
fn build_batch_dry_run_envelope(plans: &[(Ref, doiget_core::dry_run::FetchPlan)]) -> Value {
    let budget = core_rate_limit_budget();
    let plan_items: Vec<Value> = plans
        .iter()
        .map(|(ref_, plan)| {
            let envelope = build_dry_run_envelope(ref_, plan);
            // `build_dry_run_envelope` returns the single-ref shape; we
            // unpack it into per-row entries for the batch envelope.
            json!({
                "ref": envelope.get("ref").cloned().unwrap_or(Value::Null),
                "plan": envelope.get("plan").cloned().unwrap_or(Value::Null),
                "rate_limit_budget": envelope
                    .get("rate_limit_budget")
                    .cloned()
                    .unwrap_or(serde_json::to_value(budget).unwrap_or(Value::Null)),
            })
        })
        .collect();
    json!({
        "ok": true,
        "dry_run": true,
        "plans": plan_items,
    })
}

/// Build the `{ok:false, error:{code, message}}` envelope for whole-
/// call failures in `doiget_batch_fetch` (TOO_MANY_REFS, INVALID_REF on
/// bulk parse, context-init).
fn batch_fetch_error_envelope(code: ErrorCode, message: &str) -> Value {
    json!({
        "ok": false,
        "error": {
            "code": code,
            "message": message,
        },
    })
}

/// Build a [`FetchContext`] for the non-dry-run `doiget_metadata_only`
/// path.
///
/// Mirrors `crates/doiget-cli/src/commands/fetch.rs::FetchHarness::from_env`
/// minus the on-disk store (which the orchestrator does not touch in
/// Slice 1 — see the `TODO Phase 2.x` in
/// `crates/doiget-core/src/orchestrator.rs::metadata_only`):
///
/// - `HttpClient` — production allowlist (Tier 1 ∪ OA publisher), or
///   the test-mode multi-source allowlist when any `DOIGET_*_BASE` env
///   var is set.
/// - `RateLimiter` — process-wide hard-coded politeness
///   ([`RateLimits::HARD_CODED`]).
/// - `ProvenanceLog` — opened at `$DOIGET_LOG_PATH` or
///   `<config>/doiget/access.jsonl`; the parent directory is created
///   if missing.
/// - `session_id` — fresh 26-char ULID per call (one tool call = one
///   logical session, per `docs/PROVENANCE_LOG.md` §3).
fn build_fetch_context() -> anyhow::Result<FetchContext> {
    let log_path = resolve_log_path()?;
    if let Some(parent) = log_path.parent() {
        if !parent.as_str().is_empty() {
            std::fs::create_dir_all(parent.as_std_path())
                .map_err(|e| anyhow::anyhow!("creating log dir {parent}: {e}"))?;
        }
    }
    let session_id = ulid::Ulid::new().to_string();
    let log = Arc::new(
        ProvenanceLog::open(log_path, session_id.clone())
            .map_err(|e| anyhow::anyhow!("opening provenance log: {e}"))?,
    );
    let http = Arc::new(build_http_client_for_fetch()?);
    let rate_limiter = Arc::new(RateLimiter::new(RateLimits::HARD_CODED));
    Ok(FetchContext {
        http,
        rate_limiter,
        log,
        session_id,
    })
}

/// HTTP client construction with the same `DOIGET_*_BASE` test-override
/// surface that `doiget-cli` honors (`build_http_client` in
/// `crates/doiget-cli/src/commands/fetch.rs`). When no overrides are
/// set, returns the production allowlist (Tier 1 ∪ OA publisher).
fn build_http_client_for_fetch() -> anyhow::Result<HttpClient> {
    let arxiv = std::env::var("DOIGET_ARXIV_BASE").ok();
    let crossref = std::env::var("DOIGET_CROSSREF_BASE").ok();
    let unpaywall = std::env::var("DOIGET_UNPAYWALL_BASE").ok();
    let oa_publisher = std::env::var("DOIGET_OA_PUBLISHER_BASE").ok();

    if arxiv.is_none() && crossref.is_none() && unpaywall.is_none() && oa_publisher.is_none() {
        let mut allowlists = tier_1_allowlist();
        allowlists.extend(oa_publisher_allowlist());
        return HttpClient::new(allowlists)
            .map_err(|e| anyhow::anyhow!("building production HTTP client: {e}"));
    }

    let mut owned: Vec<(String, String)> = Vec::new();
    for (source, base) in [
        ("arxiv", arxiv.as_deref()),
        ("crossref", crossref.as_deref()),
        ("unpaywall", unpaywall.as_deref()),
        ("oa-publisher", oa_publisher.as_deref()),
    ] {
        if let Some(b) = base {
            let url = url::Url::parse(b)
                .map_err(|e| anyhow::anyhow!("DOIGET_*_BASE for {source} not a URL: {b}: {e}"))?;
            let host = url
                .host_str()
                .ok_or_else(|| anyhow::anyhow!("base URL has no host: {b}"))?;
            owned.push((source.to_string(), host.to_string()));
        }
    }
    let entries: Vec<(&str, &str)> = owned
        .iter()
        .map(|(s, h)| (s.as_str(), h.as_str()))
        .collect();
    Ok(HttpClient::new_for_tests_allow_http_multi(&entries))
}

/// Resolve the provenance log path. Mirrors the CLI's precedence
/// (`crates/doiget-cli/src/commands/fetch.rs::resolve_log_path`):
/// 1. `DOIGET_LOG_PATH` env var.
/// 2. `<config>/doiget/access.jsonl` where `<config>` is
///    `XDG_CONFIG_HOME` / `APPDATA` / `$HOME/.config`.
fn resolve_log_path() -> anyhow::Result<Utf8PathBuf> {
    if let Ok(s) = std::env::var("DOIGET_LOG_PATH") {
        if !s.is_empty() {
            return Ok(Utf8PathBuf::from(s));
        }
    }
    if let Ok(s) = std::env::var("XDG_CONFIG_HOME") {
        if !s.is_empty() {
            return Ok(Utf8PathBuf::from(s).join("doiget").join("access.jsonl"));
        }
    }
    if let Ok(s) = std::env::var("APPDATA") {
        if !s.is_empty() {
            return Ok(Utf8PathBuf::from(s).join("doiget").join("access.jsonl"));
        }
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_err(|_| anyhow::anyhow!("neither HOME nor USERPROFILE is set"))?;
    Ok(Utf8PathBuf::from(home)
        .join(".config")
        .join("doiget")
        .join("access.jsonl"))
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
