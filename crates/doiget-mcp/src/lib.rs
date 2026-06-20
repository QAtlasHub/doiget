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
//!   non-dry-run path (dispatches through
//!   [`doiget_core::orchestrator::metadata_only_to_store`], which also
//!   performs the `docs/MCP_TOOLS.md` §11 store-write SIDE EFFECT) are
//!   wired. The tool MUST NOT call `HttpClient::fetch_pdf` — that
//!   contract is enforced by the orchestrator and the posture-lint
//!   workflow.
//!
//! The other tools named in `docs/MCP_TOOLS.md` (`doiget_fetch_paper`,
//! `doiget_batch_fetch`, `doiget_info`, `doiget_search_local`,
//! `doiget_paper_search`, `doiget_list_recent`, `doiget_paper_pdf_path`, …)
//! are implemented below. The exact count is intentionally left unstated
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
use doiget_core::http::{
    fulltext_allowlist, oa_publisher_allowlist, tier_1_allowlist, tier_2_allowlist, HttpClient,
};
use doiget_core::orchestrator::{
    batch_fetch as core_batch_fetch, batch_fetch_plans, fetch_paper as core_fetch_paper,
    metadata_only_to_store, resolve_only as core_resolve_only, FetchPaperOutcome,
    MetadataOnlyOutcome, PdfLegStatus,
};
use doiget_core::provenance::{Capability, LogEvent, LogResult, ProvenanceLog, RowInput};
use doiget_core::rate_limiter::RateLimiter;
use doiget_core::source::{FetchContext, FetchError};
use doiget_core::sources::crossref::CrossrefSource;
use doiget_core::store::{EntryInfo, FsStore, Store};
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
    /// Per `docs/MCP_TOOLS.md` §7 ("Capability awareness", NORMATIVE),
    /// agents call this first to plan whether a TDM-class fetch will
    /// succeed. The spec'd output shape is
    /// `{ oa_enabled, metadata_sources: string[], tdm_enabled,
    /// tdm_elsevier, tdm_aps, tdm_springer, rate_limit_per_sec }`;
    /// `ok` and `tier_1/2/3` are emitted additively for back-compat.
    #[tool(
        description = "WHEN TO USE: Determine which sources the running doiget instance is allowed to use.\n\
                       INPUTS: none.\n\
                       OUTPUTS: { oa_enabled, metadata_sources, tdm_enabled, tdm_elsevier, tdm_aps, tdm_springer, rate_limit_per_sec } (plus additive ok, tier_1, tier_2, tier_3).\n\
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
    /// Both branches are wired:
    ///
    /// - `dry_run: true` → builds a [`FetchPlan`] preview and returns
    ///   it without touching the network, store, or provenance log
    ///   (ADR-0022 §2).
    /// - `dry_run: false` (default) → dispatches through
    ///   [`doiget_core::orchestrator::metadata_only_to_store`] (which
    ///   resolves the metadata **and** writes the `docs/MCP_TOOLS.md`
    ///   §11 metadata TOML to the store):
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
                    Some(&input.ref_),
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
        // `metadata_only_to_store` orchestrator (resolves metadata AND
        // writes the §11 metadata TOML). The orchestrator owns source
        // selection and per-leg politeness; we own the per-call
        // session boundary (SessionStart / SessionEnd bookend rows) and
        // the wire envelope shape (`docs/MCP_TOOLS.md` §11).
        let ctx = match build_fetch_context() {
            Ok(c) => c,
            Err(e) => {
                return Ok(CallToolResult::structured(metadata_only_error_envelope(
                    Some(&input.ref_),
                    ErrorCode::InternalError,
                    &format!("metadata-only context initialization failed: {e}"),
                )));
            }
        };

        // §11 SIDE EFFECT: persist the metadata TOML to the store
        // (#139). Resolve the store root + open the FsStore the same way
        // `doiget_fetch_paper` does — and crucially do this BEFORE the
        // `SessionStart` bookend, so a store-init failure cannot leave an
        // orphaned `SessionStart` with no `SessionEnd` in the fail-closed
        // provenance log. Mirrors `doiget_fetch_paper`'s ordering.
        // `StoreError` matches every other `FsStore::new` failure site
        // in this file.
        let store_root = match resolve_store_root() {
            Some(p) => p,
            None => {
                return Ok(CallToolResult::structured(metadata_only_error_envelope(
                    Some(&input.ref_),
                    ErrorCode::StoreError,
                    "store root could not be resolved (set DOIGET_STORE_ROOT or $HOME)",
                )));
            }
        };
        let store = match FsStore::new(store_root.clone()) {
            Ok(s) => s,
            Err(e) => {
                return Ok(CallToolResult::structured(metadata_only_error_envelope(
                    Some(&input.ref_),
                    ErrorCode::StoreError,
                    &format!("opening store at {store_root}: {e}"),
                )));
            }
        };

        // SessionStart bookend (mirrors the CLI orchestrator pattern in
        // `crates/doiget-cli/src/commands/fetch.rs::FetchHarness::log_session_start`).
        // A log-append failure here is fail-closed per
        // `docs/PROVENANCE_LOG.md` §5 — abort the call. Store init above
        // is the only fallible step before this point and it cannot have
        // emitted a row, so there is no orphaned-session window.
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
                Some(&input.ref_),
                ErrorCode::LogError,
                &format!("SessionStart append failed: {e}"),
            )));
        }

        let outcome = metadata_only_to_store(&ref_, &self.profile, &ctx, &store).await;

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

    /// `doiget_resolve_paper` — resolve a DOI / arXiv id to metadata with
    /// **no local persistence**.
    ///
    /// Per `docs/MCP_TOOLS.md` §1 (Phase 3 baseline tool list — Slice 7).
    ///
    /// Delegates to [`core_resolve_only`], whose binding contract is that
    /// no metadata TOML is ever written to the store. This holds
    /// **structurally**: `core_resolve_only` delegates to the *pure*
    /// `orchestrator::metadata_only`, while the §11 store-write lives in
    /// the separate `orchestrator::metadata_only_to_store` (which this
    /// path never calls). The divergence is enforced by construction,
    /// not by a future-slice convention (#139).
    ///
    /// Per-call boundary semantics mirror [`Self::doiget_metadata_only`]:
    /// the MCP server emits `SessionStart` / `SessionEnd` bookend rows,
    /// each consulted [`Source`](doiget_core::source::Source) emits its
    /// own `LogEvent::Fetch` row inside the orchestrator. No `StoreWrite`
    /// row is emitted (no store mutation).
    ///
    /// `dry_run` is **not** a supported input field per
    /// `docs/MCP_TOOLS.md` §10 (the spec lists `doiget_resolve_paper`
    /// in the "dry_run does not apply" set). The schema's
    /// `deny_unknown_fields` posture rejects an attempted `dry_run`
    /// field at deserialize time, which surfaces as the rmcp transport's
    /// own input-validation error rather than reaching the tool body.
    /// Agents that intend "preview the resolve" must call
    /// `doiget_metadata_only` with `dry_run: true` instead.
    #[tool(
        description = "WHEN TO USE: User wants metadata for a DOI / arXiv id with no local persistence (audit log row only).\n\
                       INPUTS: ref (DOI or arXiv id).\n\
                       OUTPUTS: { ok: true, ref, source, resolver_profile, license?, oa_url, metadata, schema_version } OR { ok:false, ref, error }.\n\
                       COSTS: 1-2 s metadata round-trip.\n\
                       SIDE EFFECTS: Appends one provenance row per consulted resolver. NEVER writes a metadata TOML to the store. NEVER fetches PDF.\n\
                       LIMITS: Subject to the same rate cap as metadata_only (5/sec). The OA URL is reported but never followed. dry_run is not supported; use metadata_only with dry_run for a preview."
    )]
    async fn doiget_resolve_paper(
        &self,
        Parameters(input): Parameters<ResolvePaperInput>,
    ) -> Result<CallToolResult, ErrorData> {
        // Step 1: parse the ref. Failures collapse to INVALID_REF per
        // docs/ERRORS.md §2 / docs/PUBLIC_API.md §4.
        let ref_ = match Ref::parse(&input.ref_) {
            Ok(r) => r,
            Err(e) => {
                return Ok(CallToolResult::structured(metadata_only_error_envelope(
                    Some(&input.ref_),
                    ErrorCode::InvalidRef,
                    &format!("invalid ref: {e}"),
                )));
            }
        };

        // Step 2: build the per-call context. Failures here surface as
        // INTERNAL_ERROR per the metadata_only pattern.
        let ctx = match build_fetch_context() {
            Ok(c) => c,
            Err(e) => {
                return Ok(CallToolResult::structured(metadata_only_error_envelope(
                    Some(&input.ref_),
                    ErrorCode::InternalError,
                    &format!("resolve-paper context initialization failed: {e}"),
                )));
            }
        };

        // SessionStart bookend (mirrors doiget_metadata_only). A
        // log-append failure here is fail-closed per
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
                Some(&input.ref_),
                ErrorCode::LogError,
                &format!("SessionStart append failed: {e}"),
            )));
        }

        let outcome = core_resolve_only(&ref_, &self.profile, &ctx).await;

        // SessionEnd bookend. Best-effort.
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

    /// `doiget_batch_from_bibliography` — read a bibliography file
    /// (CSL-JSON today; BibTeX in a follow-up slice) and fetch each
    /// resolvable entry. Per ADR-0030 D6.
    ///
    /// Mirrors `doiget_batch_fetch`'s per-entry semantics: each
    /// `ParsedEntry` becomes one element of `results[]`, success or
    /// per-entry failure independent of siblings. Each result also
    /// carries the source bibliography's citation key
    /// (`entry_key`) — the load-bearing field that lets a Zotero /
    /// Mendeley plugin bridge the fetched PDF back to the
    /// originating reference (ADR-0030 §6).
    ///
    /// `strict` controls per-entry parse-error policy:
    ///   - `strict: false` (default) — invalid entries surface as
    ///     `{ok:false, error:{code:INVALID_REF, ...}}` rows next to
    ///     successful siblings; the call as a whole still returns
    ///     `ok: true`.
    ///   - `strict: true` — the first per-entry parse error aborts
    ///     the whole call with `INVALID_REF`; successful upstream
    ///     entries are not flushed (the operator asked for
    ///     all-or-nothing).
    ///
    /// A whole-input decode failure (malformed CSL-JSON) ALWAYS
    /// aborts regardless of `strict` — the file structure is broken,
    /// not the data inside.
    #[tool(
        description = "WHEN TO USE: User has a Zotero / Mendeley CSL-JSON export and wants to fetch all OA-resolvable entries.\n\
                       INPUTS: path (absolute path to .bib / .csl / .json), format (\"auto\" | \"csl-json\" | \"bibtex\" | \"refs\", default \"auto\"), strict (bool, default false).\n\
                       OUTPUTS: { ok: true, summary:{total,ok,failed,parse_errors}, results: [{entry_key, ref, ok, ...}] } OR { ok:false, error }.\n\
                       COSTS: Same as batch_fetch — 1-3 s per entry, bounded by the 5/sec global rate cap.\n\
                       SIDE EFFECTS: Writes PDFs / metadata TOMLs to the store. Appends one provenance row per attempt.\n\
                       LIMITS: bibtex parsing is not yet shipped (re-export as CSL-JSON). Per-entry parse errors are reported in results unless strict=true."
    )]
    async fn doiget_batch_from_bibliography(
        &self,
        Parameters(input): Parameters<BatchFromBibliographyInput>,
    ) -> Result<CallToolResult, ErrorData> {
        // Step 1: resolve the format token.
        let format = match parse_bibliography_format(input.format.as_deref()) {
            Ok(f) => f,
            Err(e) => {
                return Ok(CallToolResult::structured(batch_fetch_error_envelope(
                    ErrorCode::InvalidRef,
                    &e,
                )));
            }
        };

        // Step 2: read the file. A missing / unreadable path aborts.
        let raw = match std::fs::read_to_string(&input.path) {
            Ok(s) => s,
            Err(e) => {
                return Ok(CallToolResult::structured(batch_fetch_error_envelope(
                    ErrorCode::InvalidRef,
                    &format!("reading bibliography file {:?}: {e}", input.path),
                )));
            }
        };

        // Step 3: parse via the ADR-0030 adapter.
        let path_utf8 = camino::Utf8Path::new(&input.path);
        let parsed = doiget_core::refs::parse_input(&raw, format, Some(path_utf8));

        // Pull whole-input failures out first — those always abort.
        for entry in &parsed {
            match entry {
                Err(doiget_core::refs::ParseError::Decode { format, message }) => {
                    return Ok(CallToolResult::structured(batch_fetch_error_envelope(
                        ErrorCode::InvalidRef,
                        &format!("input did not deserialise as {format}: {message}"),
                    )));
                }
                Err(doiget_core::refs::ParseError::UnsupportedFormat { format }) => {
                    return Ok(CallToolResult::structured(batch_fetch_error_envelope(
                        ErrorCode::InvalidRef,
                        &format!(
                            "{format} parsing is not yet implemented — re-export your library as CSL-JSON"
                        ),
                    )));
                }
                _ => {}
            }
        }

        // Step 4: classify per-entry outcomes into success-ready
        // `Ref`s + parse-error rows. In `strict` mode any per-entry
        // parse failure aborts the call.
        let mut parse_errors: Vec<Value> = Vec::new();
        let mut to_fetch: Vec<(Ref, Option<String>)> = Vec::new();
        for entry in parsed {
            match entry {
                Ok(p) => to_fetch.push((p.ref_, p.entry_key)),
                Err(doiget_core::refs::ParseError::InvalidRef {
                    raw,
                    entry_key,
                    source,
                }) => {
                    if input.strict {
                        return Ok(CallToolResult::structured(batch_fetch_error_envelope(
                            ErrorCode::InvalidRef,
                            &format!(
                                "entry {entry_key:?} identifier {raw:?} did not parse \
                                 (strict mode aborts): {source}"
                            ),
                        )));
                    }
                    parse_errors.push(json!({
                        "entry_key": entry_key,
                        "ref": raw,
                        "ok": false,
                        "error": {
                            "code":    ErrorCode::InvalidRef,
                            "message": source.to_string(),
                        },
                    }));
                }
                Err(doiget_core::refs::ParseError::NoIdentifier { entry_key }) => {
                    if input.strict {
                        return Ok(CallToolResult::structured(batch_fetch_error_envelope(
                            ErrorCode::InvalidRef,
                            &format!(
                                "entry {entry_key:?} has no DOI / arXiv id \
                                 (strict mode aborts)"
                            ),
                        )));
                    }
                    parse_errors.push(json!({
                        "entry_key": entry_key,
                        "ref":       Value::Null,
                        "ok":        false,
                        "error": {
                            "code":    ErrorCode::InvalidRef,
                            "message": "entry has no DOI / arXiv id",
                        },
                    }));
                }
                Err(_) => {
                    // Decode / UnsupportedFormat already drained in
                    // Step 3; this arm is defensive against future
                    // `ParseError` variants.
                    parse_errors.push(json!({
                        "entry_key": Value::Null,
                        "ref":       Value::Null,
                        "ok":        false,
                        "error": {
                            "code":    ErrorCode::InvalidRef,
                            "message": "unhandled bibliography parse error",
                        },
                    }));
                }
            }
        }

        // Enforce the same per-call ref cap `doiget_batch_fetch` does.
        if to_fetch.len() > MAX_BATCH_REFS {
            return Ok(CallToolResult::structured(batch_fetch_error_envelope(
                ErrorCode::InvalidRef,
                &format!(
                    "too many resolvable entries: got {}, max {} (TOO_MANY_REFS)",
                    to_fetch.len(),
                    MAX_BATCH_REFS
                ),
            )));
        }

        // Step 5: stand up the shared context + store (mirrors
        // `doiget_batch_fetch`). A context-init failure aborts the
        // whole call.
        let ctx = match build_fetch_context() {
            Ok(c) => c,
            Err(e) => {
                return Ok(CallToolResult::structured(batch_fetch_error_envelope(
                    ErrorCode::InternalError,
                    &format!("batch-from-bibliography context init failed: {e}"),
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
            canonical_digest: None,
        }) {
            return Ok(CallToolResult::structured(batch_fetch_error_envelope(
                ErrorCode::LogError,
                &format!("SessionStart append failed: {e}"),
            )));
        }

        // Step 6: fan out via the core orchestrator. The `Vec<Ref>`
        // alignment with `entry_keys` lets us thread per-entry keys
        // through to the result rows below.
        let refs: Vec<Ref> = to_fetch.iter().map(|(r, _)| r.clone()).collect();
        let entry_keys: Vec<Option<String>> = to_fetch.iter().map(|(_, k)| k.clone()).collect();
        let batch_outcome = core_batch_fetch(&refs, &self.profile, &ctx, &store, &store_root).await;

        let session_ok = batch_outcome
            .as_ref()
            .map(|b| b.results.iter().all(|r| r.outcome.is_ok()))
            .unwrap_or(false)
            && parse_errors.is_empty();

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
            canonical_digest: None,
        });

        // Step 7: build the per-entry success / error rows. Per-entry
        // parse-error rows from Step 4 come FIRST (the operator
        // reads the records top-down; surfacing the parse failures
        // before the fetch outcomes makes "which entries do I need
        // to fix in my library" easy to scan).
        let envelope = match batch_outcome {
            Ok(b) => build_bibliography_envelope(&b, &refs, &entry_keys, parse_errors),
            Err(e) => {
                return Ok(CallToolResult::structured(batch_fetch_error_envelope(
                    ErrorCode::from(e),
                    "batch-from-bibliography orchestrator failed",
                )));
            }
        };
        Ok(CallToolResult::structured(envelope))
    }

    /// `doiget_info` — read the metadata for a stored entry. Read-only.
    ///
    /// Per `docs/MCP_TOOLS.md` §1 (Phase 3 baseline). Mirrors the CLI's
    /// `doiget info <ref>` subcommand: opens the configured store, reads
    /// the metadata TOML for the supplied ref's safekey, and surfaces it
    /// as a JSON object in the success envelope. No network. No
    /// provenance row (this is a local-only inspection).
    #[tool(
        description = "WHEN TO USE: Inspect a stored entry's metadata locally; the entry must already have been fetched.\n\
                       INPUTS: ref (DOI or arXiv id).\n\
                       OUTPUTS: { ok: true, ref, safekey, metadata: <object>|null } OR { ok:false, ref, error }.\n\
                       COSTS: <10 ms local read.\n\
                       SIDE EFFECTS: none.\n\
                       LIMITS: A missing entry surfaces as { ok: true, metadata: null } — NOT an error envelope. Check `metadata !== null` to confirm presence; call doiget_fetch_paper first when `metadata` is null."
    )]
    async fn doiget_info(
        &self,
        Parameters(input): Parameters<InfoInput>,
    ) -> Result<CallToolResult, ErrorData> {
        let ref_ = match Ref::parse(&input.ref_) {
            Ok(r) => r,
            Err(e) => {
                return Ok(CallToolResult::structured(read_path_error_envelope(
                    Some(&input.ref_),
                    ErrorCode::InvalidRef,
                    &format!("invalid ref: {e}"),
                )));
            }
        };
        let safekey = ref_.safekey();
        let store_root = match resolve_store_root() {
            Some(p) => p,
            None => {
                return Ok(CallToolResult::structured(read_path_error_envelope(
                    Some(&input.ref_),
                    ErrorCode::InternalError,
                    "could not resolve store root (neither DOIGET_STORE_ROOT, HOME, nor USERPROFILE is set)",
                )));
            }
        };
        let store = match FsStore::new(store_root.clone()) {
            Ok(s) => s,
            Err(e) => {
                return Ok(CallToolResult::structured(read_path_error_envelope(
                    Some(&input.ref_),
                    ErrorCode::StoreError,
                    &format!("opening store at {store_root}: {e}"),
                )));
            }
        };
        match store.read(&safekey) {
            Ok(Some(m)) => {
                let payload = match serde_json::to_value(&m) {
                    Ok(v) => v,
                    Err(e) => {
                        return Ok(CallToolResult::structured(read_path_error_envelope(
                            Some(&input.ref_),
                            ErrorCode::InternalError,
                            &format!("metadata serialization failed: {e}"),
                        )));
                    }
                };
                Ok(CallToolResult::structured(json!({
                    "ok": true,
                    "ref": input.ref_,
                    "safekey": safekey.as_str(),
                    "metadata": payload,
                })))
            }
            Ok(None) => {
                // "Not in store" is NOT an error envelope — the closed
                // ErrorCode set has no NotFound variant. Surface as a
                // success envelope with `metadata: null` so agents can
                // pattern-match on the field without parsing an error.
                Ok(CallToolResult::structured(json!({
                    "ok": true,
                    "ref": input.ref_,
                    "safekey": safekey.as_str(),
                    "metadata": Value::Null,
                })))
            }
            Err(e) => Ok(CallToolResult::structured(read_path_error_envelope(
                Some(&input.ref_),
                ErrorCode::StoreError,
                &format!("store read failed: {e}"),
            ))),
        }
    }

    /// `doiget_search_local` — case-insensitive substring search over the
    /// local store's metadata. Read-only.
    ///
    /// Per `docs/MCP_TOOLS.md` §1. Backed by `Store::search`, which today
    /// is a linear scan of `<root>/.metadata/*.toml` (a Phase 2+ tantivy
    /// or sqlite-fts index will swap in transparently).
    #[tool(
        description = "WHEN TO USE: Find stored entries whose title / authors / venue / publisher contain a query substring (case-insensitive).\n\
                       INPUTS: query (string), limit (optional integer, default 50, max 200).\n\
                       OUTPUTS: { ok: true, scope: \"local\", query, count, results: [{ safekey, title, year, fetched_at }] } OR { ok:false, error }.\n\
                       COSTS: O(N) over the local store; <100 ms for a few thousand entries.\n\
                       SIDE EFFECTS: none.\n\
                       LIMITS: Returns at most `limit` entries (capped at 200)."
    )]
    async fn doiget_search_local(
        &self,
        Parameters(input): Parameters<SearchLocalInput>,
    ) -> Result<CallToolResult, ErrorData> {
        let limit = clamp_list_limit(input.limit);
        let store_root = match resolve_store_root() {
            Some(p) => p,
            None => {
                return Ok(CallToolResult::structured(read_path_error_envelope(
                    None,
                    ErrorCode::InternalError,
                    "could not resolve store root",
                )));
            }
        };
        let store = match FsStore::new(store_root.clone()) {
            Ok(s) => s,
            Err(e) => {
                return Ok(CallToolResult::structured(read_path_error_envelope(
                    None,
                    ErrorCode::StoreError,
                    &format!("opening store at {store_root}: {e}"),
                )));
            }
        };
        match store.search(&input.query, limit) {
            Ok(entries) => Ok(CallToolResult::structured(json!({
                "ok": true,
                "scope": "local",
                "query": input.query,
                "count": entries.len(),
                "results": entries.iter().map(entry_info_to_json).collect::<Vec<_>>(),
            }))),
            Err(e) => Ok(CallToolResult::structured(read_path_error_envelope(
                None,
                ErrorCode::StoreError,
                &format!("store search failed: {e}"),
            ))),
        }
    }

    /// `doiget_paper_search` — external literature discovery over OpenAlex
    /// (`/works?search=`). The MCP surface for the #281 discovery loop:
    /// turn a topic into ranked, abstract-bearing candidate papers for
    /// triage before any PDF fetch. Tier-1 OA metadata, always-on
    /// (ADR-0031 D1); metadata-only, never fetches a PDF.
    ///
    /// A supplied `author` / `venue` / `publisher` *name* is resolved to an
    /// OpenAlex id first; an ambiguous name returns `AMBIGUOUS` (with
    /// candidates in the message), a name matching nothing returns
    /// `NOT_FOUND`.
    #[tool(
        description = "WHEN TO USE: Discover papers on a topic via external OpenAlex search, abstract-first, before fetching any PDF.\n\
                       INPUTS: query (string); optional limit (1-200, default 25), from_year, to_year, oa_only (bool), min_citations, min_fwci (impact floor), min_percentile (0-100, top-X% in cohort), author, venue, publisher (names — resolved to OpenAlex ids), sort (relevance only — use min_fwci/min_percentile/from_year as filters, #290).\n\
                       OUTPUTS: { ok: true, scope: \"external\", query, total_results, count, results: [{ doi, openalex_id, arxiv, title, authors, year, venue, abstract, cited_by_count, oa_status, source }] } OR { ok:false, error }.\n\
                       COSTS: 1 OpenAlex request, plus 1 per supplied author/venue/publisher name to resolve.\n\
                       SIDE EFFECTS: Emits Metadata provenance rows. NEVER writes the store. NEVER fetches a PDF.\n\
                       LIMITS: Tier-1, always-on (no DOIGET_ENABLE_OPENALEX gate). An ambiguous author/venue/publisher name → AMBIGUOUS (candidates listed); no match → NOT_FOUND."
    )]
    async fn doiget_paper_search(
        &self,
        Parameters(input): Parameters<PaperSearchInput>,
    ) -> Result<CallToolResult, ErrorData> {
        // `sort` is a JsonSchema enum, so an unknown value is rejected at
        // deserialization (the schema advertises the choices to agents);
        // here it only needs lowering to the core enum (default: relevance).
        let sort = input
            .sort
            .map(doiget_core::discovery::SearchSort::from)
            .unwrap_or(doiget_core::discovery::SearchSort::Relevance);

        let q = doiget_core::discovery::PaperSearchQuery {
            query: input.query.clone(),
            limit: input
                .limit
                .map(|l| l as usize)
                .unwrap_or(doiget_core::discovery::DEFAULT_LIMIT),
            from_year: input.from_year,
            to_year: input.to_year,
            oa_only: input.oa_only.unwrap_or(false),
            min_citations: input.min_citations,
            min_fwci: input.min_fwci,
            min_percentile: input.min_percentile,
            author: input.author.clone(),
            venue: input.venue.clone(),
            publisher: input.publisher.clone(),
            sort,
        };
        // Boundary validation shared with the CLI (ADR-0031 D5).
        if let Err(msg) = q.validate() {
            return Ok(CallToolResult::structured(read_path_error_envelope(
                None,
                ErrorCode::InvalidRef,
                &msg,
            )));
        }

        let base = match openalex_base() {
            Ok(b) => b,
            Err(e) => {
                return Ok(CallToolResult::structured(read_path_error_envelope(
                    None,
                    ErrorCode::InternalError,
                    &e,
                )));
            }
        };
        // Omit `mailto` when no contact email is configured (never a
        // placeholder); the empty string is skipped downstream.
        let contact_email = std::env::var("DOIGET_CONTACT_EMAIL").unwrap_or_default();

        let ctx = match build_fetch_context() {
            Ok(c) => c,
            Err(e) => {
                return Ok(CallToolResult::structured(read_path_error_envelope(
                    None,
                    ErrorCode::InternalError,
                    &format!("paper-search context init failed: {e}"),
                )));
            }
        };

        if let Err(e) = ctx.log.append(RowInput {
            event: LogEvent::SessionStart,
            result: LogResult::Ok,
            capability: Capability::Metadata,
            ref_: None,
            source: None,
            error_code: None,
            size_bytes: None,
            license: None,
            store_path: None,
            canonical_digest: None,
        }) {
            return Ok(CallToolResult::structured(read_path_error_envelope(
                None,
                ErrorCode::LogError,
                &format!("SessionStart append failed: {e}"),
            )));
        }

        let outcome = doiget_core::discovery::paper_search(&base, &contact_email, &q, &ctx).await;
        let session_ok = outcome.is_ok();
        let _ = ctx.log.append(RowInput {
            event: LogEvent::SessionEnd,
            result: if session_ok {
                LogResult::Ok
            } else {
                LogResult::Err
            },
            capability: Capability::Metadata,
            ref_: None,
            source: None,
            error_code: None,
            size_bytes: None,
            license: None,
            store_path: None,
            canonical_digest: None,
        });

        match outcome {
            Ok(results) => Ok(CallToolResult::structured(json!({
                "ok": true,
                "scope": "external",
                "query": input.query,
                "total_results": results.total_results,
                "count": results.results.len(),
                "results": results.results,
            }))),
            // Canonical FetchError -> ErrorCode (AMBIGUOUS / NOT_FOUND /
            // NETWORK_ERROR / …) so an agent can branch on the code.
            Err(e) => Ok(CallToolResult::structured(read_path_error_envelope(
                None,
                ErrorCode::from(&e),
                &e.to_string(),
            ))),
        }
    }

    /// `doiget_paper_text` — extract a paper's full text from ar5iv (the
    /// #281 "read" step; ADR-0032). Fetches the ar5iv LaTeXML-XHTML
    /// rendering of an **arXiv** paper and returns it as sectioned plain
    /// text. The PDF blob is never opened (ADR-0032 D1). Tier-1 OA,
    /// always-on. A bare DOI returns `NO_OA_AVAILABLE` (DOI→arXiv linking
    /// is #281 item 5).
    #[tool(
        description = "WHEN TO USE: Read a paper's full text (arXiv only) without an external pdf-to-text tool — the 'read' step after discovery/fetch.\n\
                       INPUTS: ref (arXiv id, e.g. \"arxiv:2401.12345\"); optional max_chars (cap; omit for full text).\n\
                       OUTPUTS: { ok: true, arxiv_id, source: \"ar5iv\", title, sections: [{ heading, text }], char_count, truncated, retrieved_from } OR { ok:false, error }.\n\
                       COSTS: 1 ar5iv HTTP request (HTML), then parse; large papers can be sizeable — use max_chars to bound.\n\
                       SIDE EFFECTS: Emits an OA provenance row. NEVER opens the PDF blob; NEVER writes the store.\n\
                       LIMITS: arXiv only (a DOI → NO_OA_AVAILABLE; pass the arXiv id). A paper not converted by ar5iv → NOT_FOUND. Best-effort extraction (truncation flagged on `truncated`)."
    )]
    async fn doiget_paper_text(
        &self,
        Parameters(input): Parameters<PaperTextInput>,
    ) -> Result<CallToolResult, ErrorData> {
        // Validate the ref. A DOI has no full-text source in this slice;
        // report NO_OA_AVAILABLE rather than silently failing (ADR-0032 D5).
        let id = match doiget_core::Ref::parse(&input.ref_) {
            Ok(doiget_core::Ref::Arxiv(a)) => a,
            Ok(doiget_core::Ref::Doi(_)) => {
                return Ok(CallToolResult::structured(read_path_error_envelope(
                    Some(&input.ref_),
                    ErrorCode::NoOaAvailable,
                    "no full-text source for a DOI — pass the arXiv id if a preprint exists \
                     (DOI→arXiv linking is #281 item 5)",
                )));
            }
            Err(e) => {
                return Ok(CallToolResult::structured(read_path_error_envelope(
                    Some(&input.ref_),
                    ErrorCode::InvalidRef,
                    &e.to_string(),
                )));
            }
        };

        let base = match ar5iv_base() {
            Ok(b) => b,
            Err(e) => {
                return Ok(CallToolResult::structured(read_path_error_envelope(
                    Some(id.as_str()),
                    ErrorCode::InternalError,
                    &e,
                )));
            }
        };

        // The text cache (docs/CACHE.md) is left disabled on the MCP path
        // for now, mirroring `build_fetch_context`'s resolver-cache note;
        // enabling it here is a follow-up. Correctness is unaffected — a
        // cache miss just re-fetches.
        let ctx = match build_fetch_context() {
            Ok(c) => c,
            Err(e) => {
                return Ok(CallToolResult::structured(read_path_error_envelope(
                    Some(id.as_str()),
                    ErrorCode::InternalError,
                    &format!("paper-text context init failed: {e}"),
                )));
            }
        };

        if let Err(e) = ctx.log.append(RowInput {
            event: LogEvent::SessionStart,
            result: LogResult::Ok,
            capability: Capability::Oa,
            ref_: Some(id.as_str()),
            source: None,
            error_code: None,
            size_bytes: None,
            license: None,
            store_path: None,
            canonical_digest: None,
        }) {
            return Ok(CallToolResult::structured(read_path_error_envelope(
                Some(id.as_str()),
                ErrorCode::LogError,
                &format!("SessionStart append failed: {e}"),
            )));
        }

        let max_chars = input.max_chars.map(|m| m as usize);
        let outcome = doiget_core::paper_text::paper_text(&base, &id, max_chars, &ctx).await;
        let _ = ctx.log.append(RowInput {
            event: LogEvent::SessionEnd,
            result: if outcome.is_ok() {
                LogResult::Ok
            } else {
                LogResult::Err
            },
            capability: Capability::Oa,
            ref_: Some(id.as_str()),
            source: None,
            error_code: None,
            size_bytes: None,
            license: None,
            store_path: None,
            canonical_digest: None,
        });

        match outcome {
            Ok(t) => Ok(CallToolResult::structured(paper_text_success_envelope(&t))),
            Err(e) => Ok(CallToolResult::structured(read_path_error_envelope(
                Some(id.as_str()),
                ErrorCode::from(&e),
                &e.to_string(),
            ))),
        }
    }

    /// `doiget_paper_tex_source` — fetch the raw LaTeX source of an arXiv
    /// paper from the arXiv source API (`export.arxiv.org/src/<id>`).
    ///
    /// This is the structured-text complement to [`Self::doiget_paper_text`]
    /// (ar5iv HTML extraction). More reliable for papers that ar5iv has not
    /// yet processed through LaTeXML. LLMs handle LaTeX well — `\section{}`
    /// and equation environments provide explicit structure. PDF-only
    /// submissions return `TEXT_UNAVAILABLE`. Tier-1 OA, always-on.
    #[tool(
        description = "WHEN TO USE: Fetch the raw LaTeX source of an arXiv paper — more reliable than `doiget_paper_text` (ar5iv) for papers not yet processed by LaTeXML.\n\
                       INPUTS: ref (arXiv id, e.g. \"arxiv:2401.12345\"); optional max_chars (cap; omit for full source).\n\
                       OUTPUTS: { ok: true, arxiv_id, main_file, tex_source, char_count, truncated, retrieved_from } OR { ok:false, error }.\n\
                       COSTS: 1 arXiv source API request (gzip'd tar download); large papers can be sizeable — use max_chars to bound.\n\
                       SIDE EFFECTS: Emits an OA provenance row. NEVER writes the store; NEVER opens a PDF blob.\n\
                       LIMITS: arXiv only (a DOI → NO_OA_AVAILABLE; pass the arXiv id). PDF-only submissions → TEXT_UNAVAILABLE."
    )]
    async fn doiget_paper_tex_source(
        &self,
        Parameters(input): Parameters<PaperTexSourceInput>,
    ) -> Result<CallToolResult, ErrorData> {
        let id = match doiget_core::Ref::parse(&input.ref_) {
            Ok(doiget_core::Ref::Arxiv(a)) => a,
            Ok(doiget_core::Ref::Doi(_)) => {
                return Ok(CallToolResult::structured(read_path_error_envelope(
                    Some(&input.ref_),
                    ErrorCode::NoOaAvailable,
                    "no TeX source for a DOI — pass the arXiv id if a preprint exists",
                )));
            }
            Err(e) => {
                return Ok(CallToolResult::structured(read_path_error_envelope(
                    Some(&input.ref_),
                    ErrorCode::InvalidRef,
                    &e.to_string(),
                )));
            }
        };

        let base = match arxiv_src_base() {
            Ok(b) => b,
            Err(e) => {
                return Ok(CallToolResult::structured(read_path_error_envelope(
                    Some(id.as_str()),
                    ErrorCode::InternalError,
                    &e,
                )));
            }
        };

        let ctx = match build_fetch_context() {
            Ok(c) => c,
            Err(e) => {
                return Ok(CallToolResult::structured(read_path_error_envelope(
                    Some(id.as_str()),
                    ErrorCode::InternalError,
                    &format!("paper-tex-source context init failed: {e}"),
                )));
            }
        };

        if let Err(e) = ctx.log.append(RowInput {
            event: LogEvent::SessionStart,
            result: LogResult::Ok,
            capability: Capability::Oa,
            ref_: Some(id.as_str()),
            source: None,
            error_code: None,
            size_bytes: None,
            license: None,
            store_path: None,
            canonical_digest: None,
        }) {
            return Ok(CallToolResult::structured(read_path_error_envelope(
                Some(id.as_str()),
                ErrorCode::LogError,
                &format!("SessionStart append failed: {e}"),
            )));
        }

        let max_chars = input.max_chars.map(|m| m as usize);
        let outcome =
            doiget_core::paper_tex_source::paper_tex_source(&base, &id, max_chars, &ctx).await;

        if let Err(e) = ctx.log.append(RowInput {
            event: LogEvent::SessionEnd,
            result: if outcome.is_ok() {
                LogResult::Ok
            } else {
                LogResult::Err
            },
            capability: Capability::Oa,
            ref_: Some(id.as_str()),
            source: None,
            error_code: None,
            size_bytes: None,
            license: None,
            store_path: None,
            canonical_digest: None,
        }) {
            tracing::warn!(
                arxiv_id = %id.as_str(),
                error = %e,
                "SessionEnd append failed; session bookend missing from provenance log"
            );
        }

        match outcome {
            Ok(t) => Ok(CallToolResult::structured(json!({
                "ok": true,
                "arxiv_id": t.arxiv_id,
                "main_file": t.main_file,
                "tex_source": t.tex_source,
                "char_count": t.char_count,
                "truncated": t.truncated,
                "retrieved_from": t.retrieved_from,
            }))),
            Err(e) => Ok(CallToolResult::structured(read_path_error_envelope(
                Some(id.as_str()),
                ErrorCode::from(&e),
                &e.to_string(),
            ))),
        }
    }

    /// `doiget_link` — resolve a **DOI** to its arXiv preprint + identity
    /// cluster over OpenAlex (#281 item 5). Reports whether the same work
    /// has a free arXiv preprint so an agent can read the free full text or
    /// dedup a preprint against its journal version. Tier-1 OA, always-on;
    /// never fetches a PDF. arXiv → DOI (reverse) is a follow-up.
    #[tool(
        description = "WHEN TO USE: Given a published DOI, find whether the same work has a free arXiv preprint (to read it, or to dedup a preprint vs the journal version).\n\
                       INPUTS: ref (a DOI, e.g. \"10.1103/PhysRevB.1\").\n\
                       OUTPUTS: { ok: true, doi, arxiv, openalex_id, title } (arxiv is null when no preprint) OR { ok:false, error }.\n\
                       COSTS: 1 OpenAlex request.\n\
                       SIDE EFFECTS: Emits a Metadata provenance row. NEVER writes the store; NEVER fetches a PDF.\n\
                       LIMITS: DOI input only (arXiv → DOI is a follow-up; an arXiv/invalid ref → INVALID_REF). A DOI with no OpenAlex work → NOT_FOUND."
    )]
    async fn doiget_link(
        &self,
        Parameters(input): Parameters<LinkInput>,
    ) -> Result<CallToolResult, ErrorData> {
        // DOI input only this slice (arXiv → DOI is a follow-up). An arXiv
        // id parses fine but is the wrong direction; an unparsable ref is
        // malformed — both surface as INVALID_REF for this tool.
        let doi = match doiget_core::Ref::parse(&input.ref_) {
            Ok(doiget_core::Ref::Doi(d)) => d,
            Ok(doiget_core::Ref::Arxiv(_)) => {
                return Ok(CallToolResult::structured(read_path_error_envelope(
                    Some(&input.ref_),
                    ErrorCode::InvalidRef,
                    "doiget_link resolves a DOI to its arXiv preprint; pass a DOI \
                     (arXiv → DOI linking is #281 follow-up)",
                )));
            }
            Err(e) => {
                return Ok(CallToolResult::structured(read_path_error_envelope(
                    Some(&input.ref_),
                    ErrorCode::InvalidRef,
                    &e.to_string(),
                )));
            }
        };

        let base = match openalex_base() {
            Ok(b) => b,
            Err(e) => {
                return Ok(CallToolResult::structured(read_path_error_envelope(
                    Some(doi.as_str()),
                    ErrorCode::InternalError,
                    &e,
                )));
            }
        };
        let contact_email = std::env::var("DOIGET_CONTACT_EMAIL").unwrap_or_default();

        let ctx = match build_fetch_context() {
            Ok(c) => c,
            Err(e) => {
                return Ok(CallToolResult::structured(read_path_error_envelope(
                    Some(doi.as_str()),
                    ErrorCode::InternalError,
                    &format!("link context init failed: {e}"),
                )));
            }
        };

        if let Err(e) = ctx.log.append(RowInput {
            event: LogEvent::SessionStart,
            result: LogResult::Ok,
            capability: Capability::Metadata,
            ref_: Some(doi.as_str()),
            source: None,
            error_code: None,
            size_bytes: None,
            license: None,
            store_path: None,
            canonical_digest: None,
        }) {
            return Ok(CallToolResult::structured(read_path_error_envelope(
                Some(doi.as_str()),
                ErrorCode::LogError,
                &format!("SessionStart append failed: {e}"),
            )));
        }

        let outcome = doiget_core::discovery::resolve_links_for_doi(
            &base,
            &contact_email,
            doi.as_str(),
            &ctx,
        )
        .await;
        let _ = ctx.log.append(RowInput {
            event: LogEvent::SessionEnd,
            result: if outcome.is_ok() {
                LogResult::Ok
            } else {
                LogResult::Err
            },
            capability: Capability::Metadata,
            ref_: Some(doi.as_str()),
            source: None,
            error_code: None,
            size_bytes: None,
            license: None,
            store_path: None,
            canonical_digest: None,
        });

        match outcome {
            Ok(links) => Ok(CallToolResult::structured(json!({
                "ok": true,
                "doi": links.doi,
                "arxiv": links.arxiv,
                "openalex_id": links.openalex_id,
                "title": links.title,
            }))),
            Err(e) => Ok(CallToolResult::structured(read_path_error_envelope(
                Some(doi.as_str()),
                ErrorCode::from(&e),
                &e.to_string(),
            ))),
        }
    }

    /// `doiget_list_recent` — most-recent stored entries by
    /// `[doiget].fetched_at`. Read-only.
    ///
    /// Per `docs/MCP_TOOLS.md` §1. Backed by `Store::list_recent`.
    #[tool(
        description = "WHEN TO USE: List the most-recently fetched entries in the local store.\n\
                       INPUTS: limit (optional integer, default 50, max 200).\n\
                       OUTPUTS: { ok: true, count, entries: [{ safekey, title, year, fetched_at }] } OR { ok:false, error }.\n\
                       COSTS: <100 ms for a few thousand entries.\n\
                       SIDE EFFECTS: none.\n\
                       LIMITS: Returns at most `limit` entries (capped at 200)."
    )]
    async fn doiget_list_recent(
        &self,
        Parameters(input): Parameters<ListRecentInput>,
    ) -> Result<CallToolResult, ErrorData> {
        let limit = clamp_list_limit(input.limit);
        let store_root = match resolve_store_root() {
            Some(p) => p,
            None => {
                return Ok(CallToolResult::structured(read_path_error_envelope(
                    None,
                    ErrorCode::InternalError,
                    "could not resolve store root",
                )));
            }
        };
        let store = match FsStore::new(store_root.clone()) {
            Ok(s) => s,
            Err(e) => {
                return Ok(CallToolResult::structured(read_path_error_envelope(
                    None,
                    ErrorCode::StoreError,
                    &format!("opening store at {store_root}: {e}"),
                )));
            }
        };
        match store.list_recent(limit) {
            Ok(entries) => Ok(CallToolResult::structured(json!({
                "ok": true,
                "count": entries.len(),
                "entries": entries.iter().map(entry_info_to_json).collect::<Vec<_>>(),
            }))),
            Err(e) => Ok(CallToolResult::structured(read_path_error_envelope(
                None,
                ErrorCode::StoreError,
                &format!("store list_recent failed: {e}"),
            ))),
        }
    }

    /// `doiget_paper_pdf_path` — return the absolute path of a cached PDF.
    /// **Does not read, parse, or transmit the PDF content.**
    ///
    /// Per `docs/MCP_TOOLS.md` §1 — the spec emphasizes that this tool
    /// returns a path *string*, not bytes. The tool verifies the entry
    /// exists via `Store::read`, then returns the projected
    /// `<root>/<safekey>.pdf` path. If the metadata entry exists but no
    /// PDF was fetched (e.g. the metadata-only fallback path), `path`
    /// is `null` and `pdf_exists` is `false`.
    #[tool(
        description = "WHEN TO USE: Locate the local PDF file for a stored entry (returns a path, NOT the PDF bytes).\n\
                       INPUTS: ref (DOI or arXiv id).\n\
                       OUTPUTS: { ok: true, ref, safekey, path: string|null, pdf_exists: bool } OR { ok:false, ref, error }.\n\
                       COSTS: <10 ms local read.\n\
                       SIDE EFFECTS: none. NEVER reads or transmits PDF bytes.\n\
                       LIMITS: Both 'no metadata entry' and 'metadata exists but PDF file missing' surface as { ok: true, path: null, pdf_exists: false } — call doiget_info to distinguish the two cases. Returns an ok:false envelope only on invalid ref / store-open failure."
    )]
    async fn doiget_paper_pdf_path(
        &self,
        Parameters(input): Parameters<PaperPdfPathInput>,
    ) -> Result<CallToolResult, ErrorData> {
        let ref_ = match Ref::parse(&input.ref_) {
            Ok(r) => r,
            Err(e) => {
                return Ok(CallToolResult::structured(read_path_error_envelope(
                    Some(&input.ref_),
                    ErrorCode::InvalidRef,
                    &format!("invalid ref: {e}"),
                )));
            }
        };
        let safekey = ref_.safekey();
        let store_root = match resolve_store_root() {
            Some(p) => p,
            None => {
                return Ok(CallToolResult::structured(read_path_error_envelope(
                    Some(&input.ref_),
                    ErrorCode::InternalError,
                    "could not resolve store root",
                )));
            }
        };
        let store = match FsStore::new(store_root.clone()) {
            Ok(s) => s,
            Err(e) => {
                return Ok(CallToolResult::structured(read_path_error_envelope(
                    Some(&input.ref_),
                    ErrorCode::StoreError,
                    &format!("opening store at {store_root}: {e}"),
                )));
            }
        };
        match store.read(&safekey) {
            Ok(Some(_)) => {
                let pdf_path = store_root.join(format!("{}.pdf", safekey.as_str()));
                let exists = pdf_path.exists();
                Ok(CallToolResult::structured(json!({
                    "ok": true,
                    "ref": input.ref_,
                    "safekey": safekey.as_str(),
                    "path": if exists { Value::String(pdf_path.to_string()) } else { Value::Null },
                    "pdf_exists": exists,
                })))
            }
            Ok(None) => Ok(CallToolResult::structured(json!({
                "ok": true,
                "ref": input.ref_,
                "safekey": safekey.as_str(),
                "path": Value::Null,
                "pdf_exists": false,
            }))),
            Err(e) => Ok(CallToolResult::structured(read_path_error_envelope(
                Some(&input.ref_),
                ErrorCode::StoreError,
                &format!("store read failed: {e}"),
            ))),
        }
    }

    /// `doiget_expand_citation_graph` — BFS citation walk via OpenAlex.
    ///
    /// Per `docs/MCP_TOOLS.md` §1 (Phase 4 baseline). Compile-gated by
    /// the `citation` Cargo feature; default release binaries ship
    /// without this tool.
    ///
    /// Wraps `doiget_core::citation_graph::expand`:
    /// - The seed `ref` (DOI only) is resolved through `OpenalexSource`
    ///   for the audit trail.
    /// - Subsequent Works are walked via `ctx.http` under the
    ///   `openalex` source key from `tier_2_allowlist()`.
    /// - ADR-0010 hard caps (depth=3, total=100, per-paper=20) apply
    ///   regardless of caller input. `truncated: true` surfaces when
    ///   any cap is hit.
    #[tool(
        description = "WHEN TO USE: Expand a DOI's citation neighborhood via OpenAlex BFS.\n\
                       INPUTS: ref (DOI), depth (optional 1-3, default 3), total (optional 1-100, default 100), per_paper (optional 1-20, default 20).\n\
                       OUTPUTS: { ok: true, ref, seed_work_id, nodes, edges, truncated, total_visited } OR { ok:false, ref, error }.\n\
                       COSTS: O(total) OpenAlex requests; expect 1-30 s for a depth-3 walk.\n\
                       SIDE EFFECTS: Emits one provenance row per consulted Work under Capability::Metadata. NEVER writes to the store. NEVER fetches PDF.\n\
                       LIMITS: ADR-0010 hard caps applied regardless of inputs: depth<=3, total<=100, per_paper<=20. Requires DOIGET_ENABLE_OPENALEX in env. Returns NOT_IMPLEMENTED when this binary was built without the `citation` Cargo feature."
    )]
    async fn doiget_expand_citation_graph(
        &self,
        Parameters(input): Parameters<ExpandCitationGraphInput>,
    ) -> Result<CallToolResult, ErrorData> {
        // The body is conditionally compiled. Default release binaries
        // are built WITHOUT `--features citation` and return
        // NOT_IMPLEMENTED for this tool so the wire surface stays
        // stable across feature configurations. With the feature on,
        // the call dispatches into `citation_graph::expand`.
        #[cfg(not(feature = "citation"))]
        {
            let _ = input;
            return Ok(CallToolResult::structured(json!({
                "ok": false,
                "error": {
                    "code": ErrorCode::NotImplemented,
                    "message": "doiget_expand_citation_graph requires the `citation` Cargo feature; this binary was built without it",
                },
            })));
        }
        #[cfg(feature = "citation")]
        {
            let ref_ = match Ref::parse(&input.ref_) {
                Ok(r) => r,
                Err(e) => {
                    return Ok(CallToolResult::structured(read_path_error_envelope(
                        Some(&input.ref_),
                        ErrorCode::InvalidRef,
                        &format!("invalid ref: {e}"),
                    )));
                }
            };
            let doi = match &ref_ {
                Ref::Doi(d) => d.clone(),
                Ref::Arxiv(_) => {
                    return Ok(CallToolResult::structured(read_path_error_envelope(
                        Some(&input.ref_),
                        ErrorCode::InvalidRef,
                        "expand_citation_graph requires a DOI seed (arXiv ids are not supported)",
                    )));
                }
            };

            let ctx = match build_fetch_context() {
                Ok(c) => c,
                Err(e) => {
                    return Ok(CallToolResult::structured(read_path_error_envelope(
                        Some(&input.ref_),
                        ErrorCode::InternalError,
                        &format!("expand-graph context init failed: {e}"),
                    )));
                }
            };

            let contact_email = std::env::var("DOIGET_CONTACT_EMAIL")
                .unwrap_or_else(|_| "doiget@localhost".to_string());
            let source = if let Ok(base) = std::env::var("DOIGET_OPENALEX_BASE") {
                if let Ok(url) = url::Url::parse(&base) {
                    doiget_core::sources::openalex::OpenalexSource::with_base(url, contact_email)
                } else {
                    doiget_core::sources::openalex::OpenalexSource::new(contact_email)
                }
            } else {
                doiget_core::sources::openalex::OpenalexSource::new(contact_email)
            };

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
                canonical_digest: None,
            }) {
                return Ok(CallToolResult::structured(read_path_error_envelope(
                    Some(&input.ref_),
                    ErrorCode::LogError,
                    &format!("SessionStart append failed: {e}"),
                )));
            }

            let caps = doiget_core::citation_graph::GraphCaps {
                depth: input
                    .depth
                    .map(|d| d as usize)
                    .unwrap_or(doiget_core::citation_graph::GraphCaps::MAX_DEPTH),
                total: input
                    .total
                    .map(|t| t as usize)
                    .unwrap_or(doiget_core::citation_graph::GraphCaps::MAX_TOTAL),
                per_paper: input
                    .per_paper
                    .map(|p| p as usize)
                    .unwrap_or(doiget_core::citation_graph::GraphCaps::MAX_PER_PAPER),
            };

            let outcome =
                doiget_core::citation_graph::expand(&doi, caps, &source, &self.profile, &ctx).await;

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
                canonical_digest: None,
            });

            match outcome {
                Ok(graph) => Ok(CallToolResult::structured(json!({
                    "ok": true,
                    "ref": input.ref_,
                    "seed_work_id": graph.seed_work_id,
                    "nodes": graph.nodes,
                    "edges": graph.edges,
                    "truncated": graph.truncated,
                    "total_visited": graph.total_visited,
                }))),
                Err(e) => Ok(CallToolResult::structured(read_path_error_envelope(
                    Some(&input.ref_),
                    match &e {
                        doiget_core::citation_graph::GraphError::CapabilityDenied => {
                            ErrorCode::CapabilityDenied
                        }
                        doiget_core::citation_graph::GraphError::SeedNotIndexed => {
                            ErrorCode::NoOaAvailable
                        }
                        doiget_core::citation_graph::GraphError::Log(_) => ErrorCode::LogError,
                        doiget_core::citation_graph::GraphError::Source(_) => {
                            ErrorCode::NetworkError
                        }
                        _ => ErrorCode::InternalError,
                    },
                    &format!("citation graph expansion failed: {e}"),
                ))),
            }
        }
    }

    /// `doiget_bibtex_export` — render stored entries as BibTeX.
    ///
    /// Per `docs/MCP_TOOLS.md` §1. Read-only, no network, no
    /// provenance row (local inspection, same posture as
    /// `doiget_info`). Accepts one or many refs; each is resolved
    /// independently so a bad ref in the batch does not fail the
    /// others.
    #[tool(
        description = "WHEN TO USE: Export already-fetched entries as BibTeX (one or many).\n\
                       INPUTS: refs (array of DOI / arXiv id strings, 1..=200).\n\
                       OUTPUTS: { ok: true, entries: [{ ref, safekey, bibtex }] } — bibtex is null when the entry is not in the store; a per-ref { ref, error } element is emitted for an invalid ref or a store read error. OR { ok:false, error } for a store-open failure.\n\
                       COSTS: <10 ms per entry, local read.\n\
                       SIDE EFFECTS: none.\n\
                       LIMITS: Entry must already have been fetched (bibtex:null otherwise — NOT an error). At most 200 refs per call."
    )]
    async fn doiget_bibtex_export(
        &self,
        Parameters(input): Parameters<BibtexExportInput>,
    ) -> Result<CallToolResult, ErrorData> {
        Ok(self.export_citations(&input.refs, CiteFmt::Bibtex))
    }

    /// `doiget_csl_export` — render stored entries as CSL JSON 1.0.
    ///
    /// Per `docs/MCP_TOOLS.md` §1. Read-only, no network, no
    /// provenance row. Each found entry's payload is a single-element
    /// CSL JSON 1.0 array (drop-in for citeproc-js / pandoc).
    #[tool(
        description = "WHEN TO USE: Export already-fetched entries as CSL JSON 1.0 (one or many).\n\
                       INPUTS: refs (array of DOI / arXiv id strings, 1..=200).\n\
                       OUTPUTS: { ok: true, entries: [{ ref, safekey, csl }] } — csl is a 1-element CSL JSON array, or null when the entry is not in the store; a per-ref { ref, error } element is emitted for an invalid ref or a store read error. OR { ok:false, error } for a store-open failure.\n\
                       COSTS: <10 ms per entry, local read.\n\
                       SIDE EFFECTS: none.\n\
                       LIMITS: Entry must already have been fetched (csl:null otherwise — NOT an error). At most 200 refs per call."
    )]
    async fn doiget_csl_export(
        &self,
        Parameters(input): Parameters<CslExportInput>,
    ) -> Result<CallToolResult, ErrorData> {
        Ok(self.export_citations(&input.refs, CiteFmt::Csl))
    }

    /// `doiget_resolve_citation` — resolve a free-form bibliographic citation string to ranked DOI candidates.
    #[tool(
        description = "WHEN TO USE: Resolve a free-form bibliographic citation string (e.g. 'Onsager 1944') to ranked DOI candidates.\n\
                       INPUTS: query (bibliographic citation query string), limit (maximum number of candidates to return, default: 5).\n\
                       OUTPUTS: { ok: true, query, candidates: [ { doi, title, author, year, score, source } ] } OR { ok: false, error }.\n\
                       COSTS: 1-2 s round-trip.\n\
                       SIDE EFFECTS: none.\n\
                       LIMITS: Returns candidates with similarity score >= 0.5."
    )]
    async fn doiget_resolve_citation(
        &self,
        Parameters(input): Parameters<ResolveCitationInput>,
    ) -> Result<CallToolResult, ErrorData> {
        let ctx = match build_fetch_context() {
            Ok(c) => c,
            Err(e) => {
                return Ok(CallToolResult::structured(serde_json::json!({
                    "ok": false,
                    "error": format!("context initialization failed: {e}"),
                })));
            }
        };

        let source = match crossref_source_from_env() {
            Ok(s) => s,
            Err(e) => {
                return Ok(CallToolResult::structured(serde_json::json!({
                    "ok": false,
                    "error": e,
                })));
            }
        };

        match source
            .resolve_citation(&input.query, input.limit, &ctx)
            .await
        {
            Ok(candidates) => Ok(CallToolResult::structured(serde_json::json!({
                "ok": true,
                "query": input.query,
                "candidates": candidates,
            }))),
            Err(e) => Ok(CallToolResult::structured(serde_json::json!({
                "ok": false,
                "error": format!("resolve failed: {e}"),
            }))),
        }
    }

    /// `doiget_batch_resolve_citations` — resolve multiple free-form bibliographic citation strings to ranked DOI candidates.
    #[tool(
        description = "WHEN TO USE: Resolve multiple free-form bibliographic citation strings in batch.\n\
                       INPUTS: queries (array of query strings), limit (maximum number of candidates per query, default: 5).\n\
                       OUTPUTS: { ok: true, results: [ { query, candidates: [ { doi, title, author, year, score, source } ] } ] } OR { ok: false, error }.\n\
                       COSTS: 1-2 s round-trip per query.\n\
                       SIDE EFFECTS: none.\n\
                       LIMITS: Returns candidates with similarity score >= 0.5. At most 50 queries per call."
    )]
    async fn doiget_batch_resolve_citations(
        &self,
        Parameters(input): Parameters<BatchResolveCitationsInput>,
    ) -> Result<CallToolResult, ErrorData> {
        if input.queries.len() > 50 {
            return Ok(CallToolResult::structured(serde_json::json!({
                "ok": false,
                "error": "At most 50 queries per call.",
            })));
        }

        let ctx = match build_fetch_context() {
            Ok(c) => c,
            Err(e) => {
                return Ok(CallToolResult::structured(serde_json::json!({
                    "ok": false,
                    "error": format!("context initialization failed: {e}"),
                })));
            }
        };

        let source = match crossref_source_from_env() {
            Ok(s) => s,
            Err(e) => {
                return Ok(CallToolResult::structured(serde_json::json!({
                    "ok": false,
                    "error": e,
                })));
            }
        };

        let mut results = Vec::new();
        for query in &input.queries {
            match source.resolve_citation(query, input.limit, &ctx).await {
                Ok(candidates) => {
                    results.push(serde_json::json!({
                        "query": query,
                        "candidates": candidates,
                    }));
                }
                Err(e) => {
                    results.push(serde_json::json!({
                        "query": query,
                        "error": format!("resolve failed: {e}"),
                    }));
                }
            }
        }

        Ok(CallToolResult::structured(serde_json::json!({
            "ok": true,
            "results": results,
        })))
    }

    /// `doiget_tag` — add / remove tags and collection membership on a stored
    /// entry. Mutates `[doiget].tags` and `[doiget].collections` in the
    /// metadata TOML (issue #294). All mutations are idempotent.
    ///
    /// The entry must already be in the store (fetched via `doiget_fetch_paper`
    /// or equivalent). No network I/O is performed.
    #[tool(
        description = "WHEN TO USE: Add or remove tags / collections on a stored paper entry for local knowledge-base organisation (#294).\n\
                       INPUTS: ref (DOI or arXiv id); add (array of tags to add, optional); remove (array of tags to remove, optional); collection_add (array of collections to join, optional); collection_remove (array of collections to leave, optional).\n\
                       OUTPUTS: { ok: true, ref, tags, collections } OR { ok: false, error }.\n\
                       COSTS: 0 network requests; one store read + write.\n\
                       SIDE EFFECTS: Overwrites [doiget].tags / [doiget].collections in <store>/.metadata/<safekey>.toml.\n\
                       LIMITS: Entry must already exist in the store. Tags are case-sensitive; idempotent add/remove."
    )]
    async fn doiget_tag(
        &self,
        Parameters(input): Parameters<TagInput>,
    ) -> Result<CallToolResult, ErrorData> {
        let ref_ = match Ref::parse(&input.ref_) {
            Ok(r) => r,
            Err(e) => {
                return Ok(CallToolResult::structured(read_path_error_envelope(
                    Some(&input.ref_),
                    ErrorCode::InvalidRef,
                    &format!("invalid ref: {e}"),
                )));
            }
        };
        let safekey = ref_.safekey();

        let store_root = match resolve_store_root() {
            Some(p) => p,
            None => {
                return Ok(CallToolResult::structured(read_path_error_envelope(
                    Some(&input.ref_),
                    ErrorCode::InternalError,
                    "could not resolve store root",
                )));
            }
        };
        let store = match FsStore::new(store_root.clone()) {
            Ok(s) => s,
            Err(e) => {
                return Ok(CallToolResult::structured(read_path_error_envelope(
                    Some(&input.ref_),
                    ErrorCode::StoreError,
                    &format!("opening store at {store_root}: {e}"),
                )));
            }
        };

        let mut metadata = match store.read(&safekey) {
            Ok(Some(m)) => m,
            Ok(None) => {
                return Ok(CallToolResult::structured(json!({
                    "ok": false,
                    "error": format!("no store entry for {}; fetch the paper first", input.ref_),
                })));
            }
            Err(e) => {
                return Ok(CallToolResult::structured(read_path_error_envelope(
                    Some(&input.ref_),
                    ErrorCode::StoreError,
                    &format!("store read failed: {e}"),
                )));
            }
        };

        let ext = match metadata.doiget.as_mut() {
            Some(d) => d,
            None => {
                return Ok(CallToolResult::structured(json!({
                    "ok": false,
                    "error": format!("entry {} has no [doiget] table; fetch it first", input.ref_),
                })));
            }
        };

        for t in &input.add {
            if !ext.tags.contains(t) {
                ext.tags.push(t.clone());
            }
        }
        for t in &input.remove {
            ext.tags.retain(|x| x != t);
        }
        for c in &input.collection_add {
            if !ext.collections.contains(c) {
                ext.collections.push(c.clone());
            }
        }
        for c in &input.collection_remove {
            ext.collections.retain(|x| x != c);
        }

        let tags = ext.tags.clone();
        let collections = ext.collections.clone();

        match store.write(&safekey, &metadata, None) {
            Ok(()) => Ok(CallToolResult::structured(json!({
                "ok": true,
                "ref": input.ref_,
                "tags": tags,
                "collections": collections,
            }))),
            Err(e) => Ok(CallToolResult::structured(read_path_error_envelope(
                Some(&input.ref_),
                ErrorCode::StoreError,
                &format!("store write failed: {e}"),
            ))),
        }
    }

    /// `doiget_annotate` — set or clear the freeform annotation on a stored
    /// paper entry. Mutates `[doiget].annotation` in the metadata TOML
    /// (issue #294).
    ///
    /// The entry must already be in the store. No network I/O is performed.
    #[tool(
        description = "WHEN TO USE: Attach or clear a freeform annotation / note on a stored paper for local knowledge-base organisation (#294).\n\
                       INPUTS: ref (DOI or arXiv id); text (string — the annotation; omit or set to null to clear); clear (bool, default false — explicitly clear the annotation).\n\
                       OUTPUTS: { ok: true, ref, annotation } OR { ok: false, error }.\n\
                       COSTS: 0 network requests; one store read + write.\n\
                       SIDE EFFECTS: Overwrites [doiget].annotation in <store>/.metadata/<safekey>.toml.\n\
                       LIMITS: Entry must already exist in the store. Setting text overrides any existing annotation."
    )]
    async fn doiget_annotate(
        &self,
        Parameters(input): Parameters<AnnotateInput>,
    ) -> Result<CallToolResult, ErrorData> {
        let ref_ = match Ref::parse(&input.ref_) {
            Ok(r) => r,
            Err(e) => {
                return Ok(CallToolResult::structured(read_path_error_envelope(
                    Some(&input.ref_),
                    ErrorCode::InvalidRef,
                    &format!("invalid ref: {e}"),
                )));
            }
        };
        let safekey = ref_.safekey();

        let store_root = match resolve_store_root() {
            Some(p) => p,
            None => {
                return Ok(CallToolResult::structured(read_path_error_envelope(
                    Some(&input.ref_),
                    ErrorCode::InternalError,
                    "could not resolve store root",
                )));
            }
        };
        let store = match FsStore::new(store_root.clone()) {
            Ok(s) => s,
            Err(e) => {
                return Ok(CallToolResult::structured(read_path_error_envelope(
                    Some(&input.ref_),
                    ErrorCode::StoreError,
                    &format!("opening store at {store_root}: {e}"),
                )));
            }
        };

        let mut metadata = match store.read(&safekey) {
            Ok(Some(m)) => m,
            Ok(None) => {
                return Ok(CallToolResult::structured(json!({
                    "ok": false,
                    "error": format!("no store entry for {}; fetch the paper first", input.ref_),
                })));
            }
            Err(e) => {
                return Ok(CallToolResult::structured(read_path_error_envelope(
                    Some(&input.ref_),
                    ErrorCode::StoreError,
                    &format!("store read failed: {e}"),
                )));
            }
        };

        let ext = match metadata.doiget.as_mut() {
            Some(d) => d,
            None => {
                return Ok(CallToolResult::structured(json!({
                    "ok": false,
                    "error": format!("entry {} has no [doiget] table; fetch it first", input.ref_),
                })));
            }
        };

        if input.clear.unwrap_or(false) {
            ext.annotation = None;
        } else if let Some(ref text) = input.text {
            if text.is_empty() {
                return Ok(CallToolResult::structured(json!({
                    "ok": false,
                    "error": "annotation text must not be empty; set clear:true to remove it",
                })));
            }
            ext.annotation = Some(text.clone());
        } else {
            return Ok(CallToolResult::structured(json!({
                "ok": false,
                "error": "provide 'text' to set an annotation, or 'clear: true' to remove it",
            })));
        }

        let annotation = ext.annotation.clone();

        match store.write(&safekey, &metadata, None) {
            Ok(()) => Ok(CallToolResult::structured(json!({
                "ok": true,
                "ref": input.ref_,
                "annotation": annotation,
            }))),
            Err(e) => Ok(CallToolResult::structured(read_path_error_envelope(
                Some(&input.ref_),
                ErrorCode::StoreError,
                &format!("store write failed: {e}"),
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
#[serde(deny_unknown_fields)]
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

/// JSON-schema-derived input for the `doiget_resolve_paper` MCP tool.
///
/// Mirrors `docs/MCP_TOOLS.md` §1 (the tool name and shape) and §10
/// (`dry_run` does **not** apply to `doiget_resolve_paper`).
/// `deny_unknown_fields` rejects an attempted `dry_run` field as a
/// schema violation at the rmcp transport boundary, so the tool body
/// never observes it. Agents that need a preview must call
/// `doiget_metadata_only` with `dry_run: true` instead.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct ResolvePaperInput {
    /// DOI or arXiv id. Validated via `Ref::parse`; failures surface as
    /// `INVALID_REF` per `docs/ERRORS.md`.
    #[serde(rename = "ref")]
    #[schemars(rename = "ref")]
    pub ref_: String,
}

// ---------------------------------------------------------------------------
// Slice 8 read-path inputs + helpers
// ---------------------------------------------------------------------------

/// JSON-schema-derived input for the `doiget_info` MCP tool.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct InfoInput {
    /// DOI or arXiv id. Validated via `Ref::parse`; failures surface as
    /// `INVALID_REF`.
    #[serde(rename = "ref")]
    #[schemars(rename = "ref")]
    pub ref_: String,
}

/// JSON-schema-derived input for the `doiget_search_local` MCP tool.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct SearchLocalInput {
    /// Case-insensitive substring matched against title / authors /
    /// venue / publisher.
    pub query: String,
    /// Maximum number of results to return. `None` means default (50);
    /// values >200 are clamped to 200.
    #[serde(default)]
    pub limit: Option<u32>,
}

/// `sort` choices for `doiget_paper_search`. Modelled as a JsonSchema enum
/// (not a free string) so the valid values appear in the tool's input
/// schema — an agent picks a valid one rather than guessing a token, and
/// an unknown value is rejected at deserialization (ADR-0031 D5).
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[schemars(rename_all = "lowercase")]
pub enum SortInput {
    /// Best textual match first (`relevance_score:desc`). The only sort:
    /// `cited` / `recent` were removed (#290) because over OpenAlex's loose
    /// free-text match they surface off-topic papers; use `min_fwci` /
    /// `min_percentile` / `from_year` to express "important / recent" as
    /// filters instead.
    Relevance,
}

impl From<SortInput> for doiget_core::discovery::SearchSort {
    fn from(s: SortInput) -> Self {
        match s {
            SortInput::Relevance => Self::Relevance,
        }
    }
}

/// JSON-schema-derived input for the `doiget_paper_search` MCP tool
/// (external OpenAlex discovery; ADR-0031).
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct PaperSearchInput {
    /// Free-text topic query (e.g. "tropical tensor networks for spin glasses").
    pub query: String,
    /// Maximum results (1..=200; default 25). Out-of-range is rejected.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Inclusive lower publication-year bound.
    #[serde(default)]
    pub from_year: Option<i32>,
    /// Inclusive upper publication-year bound.
    #[serde(default)]
    pub to_year: Option<i32>,
    /// Restrict to open-access works.
    #[serde(default)]
    pub oa_only: Option<bool>,
    /// Only works cited strictly more than this many times.
    #[serde(default)]
    pub min_citations: Option<u64>,
    /// Minimum field-and-year-normalized impact (FWCI) — a quality filter
    /// (#290).
    #[serde(default)]
    pub min_fwci: Option<f64>,
    /// Minimum within-cohort citation percentile (0–100): top-X% among
    /// same-year works; combine with `from_year` for "recent and standing
    /// out" (#290).
    #[serde(default)]
    pub min_percentile: Option<u8>,
    /// Author name (resolved to an OpenAlex author id).
    #[serde(default)]
    pub author: Option<String>,
    /// Venue / journal name (resolved to an OpenAlex source id).
    #[serde(default)]
    pub venue: Option<String>,
    /// Publisher name (resolved to an OpenAlex publisher id).
    #[serde(default)]
    pub publisher: Option<String>,
    /// Result ordering: `relevance` only (the default; `cited` / `recent`
    /// were removed — see `min_fwci` / `min_percentile` / `from_year`, #290).
    #[serde(default)]
    pub sort: Option<SortInput>,
}

/// JSON-schema-derived input for the `doiget_paper_text` MCP tool
/// (full-text extraction from ar5iv; ADR-0032).
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct PaperTextInput {
    /// arXiv id (e.g. "arxiv:2401.12345"), validated via `Ref::parse`. A
    /// bare DOI returns `NO_OA_AVAILABLE` (pass the arXiv id;
    /// DOI→arXiv linking is #281 item 5).
    #[serde(rename = "ref")]
    #[schemars(rename = "ref")]
    pub ref_: String,
    /// Cap the returned text to this many characters (truncation is
    /// flagged on `truncated`). Omit for the full text.
    #[serde(default)]
    pub max_chars: Option<u32>,
}

/// JSON-schema-derived input for the `doiget_paper_tex_source` MCP tool.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct PaperTexSourceInput {
    /// arXiv id (e.g. "arxiv:2401.12345"), validated via `Ref::parse`. A
    /// bare DOI returns `NO_OA_AVAILABLE` (pass the arXiv id).
    #[serde(rename = "ref")]
    #[schemars(rename = "ref")]
    pub ref_: String,
    /// Cap the returned LaTeX source to this many characters (truncation is
    /// flagged on `truncated`). Omit for the full source.
    #[serde(default)]
    pub max_chars: Option<u32>,
}

/// JSON-schema-derived input for the `doiget_link` MCP tool (DOI → arXiv
/// preprint linking; #281 item 5).
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct LinkInput {
    /// A **DOI** (e.g. "10.1103/PhysRevB.1"), validated via `Ref::parse`.
    /// An arXiv id or unparsable ref returns `INVALID_REF` (arXiv → DOI is
    /// a follow-up).
    #[serde(rename = "ref")]
    #[schemars(rename = "ref")]
    pub ref_: String,
}

/// JSON-schema-derived input for the `doiget_list_recent` MCP tool.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct ListRecentInput {
    /// Maximum number of results to return. `None` means default (50);
    /// values >200 are clamped to 200.
    #[serde(default)]
    pub limit: Option<u32>,
}

/// JSON-schema-derived input for the `doiget_paper_pdf_path` MCP tool.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct PaperPdfPathInput {
    /// DOI or arXiv id. Validated via `Ref::parse`; failures surface as
    /// `INVALID_REF`.
    #[serde(rename = "ref")]
    #[schemars(rename = "ref")]
    pub ref_: String,
}

/// JSON-schema-derived input for the `doiget_expand_citation_graph`
/// MCP tool. **Always present** in the type system — the
/// `#[tool_router]` macro references this type unconditionally and a
/// cfg-gated `pub struct` would cause an `unresolved type` error in
/// the default build. The feature-gate is applied only to the tool
/// body, which returns `NOT_IMPLEMENTED` when built without
/// `--features citation`. ADR-0010 hard caps (depth<=3, total<=100,
/// per_paper<=20) are applied inside the tool body via
/// `GraphCaps::clamped` — the `Option<u32>` fields below are caller
/// hints, not authoritative.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct ExpandCitationGraphInput {
    /// DOI seed. arXiv ids are rejected with `INVALID_REF`.
    #[serde(rename = "ref")]
    #[schemars(rename = "ref")]
    pub ref_: String,
    /// Max BFS depth (1..=3). Default is the ADR-0010 maximum (3).
    #[serde(default)]
    pub depth: Option<u32>,
    /// Max total nodes (1..=100). Default is the ADR-0010 maximum
    /// (100). `truncated: true` is set on the response when this
    /// cap is hit.
    #[serde(default)]
    pub total: Option<u32>,
    /// Max children expanded per parent (1..=20). Default is the
    /// ADR-0010 maximum (20).
    #[serde(default)]
    pub per_paper: Option<u32>,
}

/// JSON-schema-derived input for the `doiget_bibtex_export` MCP tool.
/// One or many refs (`docs/MCP_TOOLS.md` §1 row `doiget_bibtex_export`);
/// each is resolved independently.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct BibtexExportInput {
    /// DOIs / arXiv ids to render. 1..=200; each validated via
    /// `Ref::parse` with per-ref error reporting.
    pub refs: Vec<String>,
}

/// JSON-schema-derived input for the `doiget_csl_export` MCP tool.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct CslExportInput {
    /// DOIs / arXiv ids to render. 1..=200; each validated via
    /// `Ref::parse` with per-ref error reporting.
    pub refs: Vec<String>,
}

/// JSON-schema-derived input for the `doiget_resolve_citation` MCP tool.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct ResolveCitationInput {
    /// Bibliographic citation query string (e.g. "Onsager 1944").
    pub query: String,
    /// Maximum number of candidates to return (default: 5).
    #[serde(default = "default_resolve_limit")]
    pub limit: u8,
}

/// JSON-schema-derived input for the `doiget_batch_resolve_citations` MCP tool.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct BatchResolveCitationsInput {
    /// Bibliographic citation query strings.
    pub queries: Vec<String>,
    /// Maximum number of candidates to return per query (default: 5).
    #[serde(default = "default_resolve_limit")]
    pub limit: u8,
}

fn default_resolve_limit() -> u8 {
    5
}

/// JSON-schema-derived input for the `doiget_tag` MCP tool.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct TagInput {
    /// DOI or arXiv id. Validated via `Ref::parse`; failures surface as
    /// `INVALID_REF`.
    #[serde(rename = "ref")]
    #[schemars(rename = "ref")]
    pub ref_: String,
    /// Tags to add (idempotent, case-sensitive).
    #[serde(default)]
    pub add: Vec<String>,
    /// Tags to remove.
    #[serde(default)]
    pub remove: Vec<String>,
    /// Collections to join (idempotent, case-sensitive).
    #[serde(default)]
    pub collection_add: Vec<String>,
    /// Collections to leave.
    #[serde(default)]
    pub collection_remove: Vec<String>,
}

/// JSON-schema-derived input for the `doiget_annotate` MCP tool.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct AnnotateInput {
    /// DOI or arXiv id. Validated via `Ref::parse`; failures surface as
    /// `INVALID_REF`.
    #[serde(rename = "ref")]
    #[schemars(rename = "ref")]
    pub ref_: String,
    /// Annotation text. Replaces any existing annotation. Omit or set to
    /// null together with `clear: true` to remove the annotation.
    #[serde(default)]
    pub text: Option<String>,
    /// Set to `true` to clear (remove) the annotation instead of setting it.
    #[serde(default)]
    pub clear: Option<bool>,
}

/// Hard cap on refs accepted by a single `doiget_bibtex_export` /
/// `doiget_csl_export` call. Mirrors the 200-entry ceiling used by the
/// read-path list tools.
const MAX_EXPORT_REFS: usize = 200;

/// Which citation format [`Server::export_citations`] renders.
#[derive(Debug, Clone, Copy)]
enum CiteFmt {
    Bibtex,
    Csl,
}

impl Server {
    /// Shared body for `doiget_bibtex_export` / `doiget_csl_export`.
    ///
    /// Opens the store once, then resolves each ref independently:
    /// a found entry yields `{ ref, safekey, <payload> }`, a missing
    /// one `{ ref, safekey, <payload>: null }` (NOT an error — same
    /// convention as `doiget_info`), an unparsable ref or a store
    /// read failure a per-ref `{ ref, error }` element. A failure to
    /// resolve / open the store at all is a single `ok:false`
    /// envelope. Read-only; emits no provenance row.
    fn export_citations(&self, refs: &[String], fmt: CiteFmt) -> CallToolResult {
        if refs.len() > MAX_EXPORT_REFS {
            return CallToolResult::structured(read_path_error_envelope(
                None,
                ErrorCode::InvalidRef,
                &format!("too many refs: got {}, max {MAX_EXPORT_REFS}", refs.len()),
            ));
        }
        let store_root = match resolve_store_root() {
            Some(p) => p,
            None => {
                return CallToolResult::structured(read_path_error_envelope(
                    None,
                    ErrorCode::InternalError,
                    "could not resolve store root (neither DOIGET_STORE_ROOT, HOME, nor USERPROFILE is set)",
                ));
            }
        };
        let store = match FsStore::new(store_root.clone()) {
            Ok(s) => s,
            Err(e) => {
                return CallToolResult::structured(read_path_error_envelope(
                    None,
                    ErrorCode::StoreError,
                    &format!("opening store at {store_root}: {e}"),
                ));
            }
        };

        let payload_key = match fmt {
            CiteFmt::Bibtex => "bibtex",
            CiteFmt::Csl => "csl",
        };
        let mut entries: Vec<Value> = Vec::with_capacity(refs.len());
        for r in refs {
            let ref_ = match Ref::parse(r) {
                Ok(v) => v,
                Err(e) => {
                    entries.push(json!({
                        "ref": r,
                        "error": { "code": ErrorCode::InvalidRef, "message": format!("invalid ref: {e}") },
                    }));
                    continue;
                }
            };
            let safekey = ref_.safekey();
            match store.read(&safekey) {
                Ok(Some(m)) => {
                    let payload = match fmt {
                        CiteFmt::Bibtex => {
                            Value::from(doiget_core::store::render::to_bibtex(safekey.as_str(), &m))
                        }
                        CiteFmt::Csl => {
                            doiget_core::store::render::to_csl_array(safekey.as_str(), &m)
                        }
                    };
                    // `json!` requires literal keys; the payload key is
                    // format-dependent, so build the object explicitly.
                    let mut obj = serde_json::Map::with_capacity(3);
                    obj.insert("ref".to_string(), Value::from(r.clone()));
                    obj.insert("safekey".to_string(), Value::from(safekey.as_str()));
                    obj.insert(payload_key.to_string(), payload);
                    entries.push(Value::Object(obj));
                }
                Ok(None) => {
                    // Not in store is NOT an error (closed ErrorCode set
                    // has no NotFound). Surface null payload, same as
                    // doiget_info's `metadata: null`.
                    let mut obj = serde_json::Map::with_capacity(3);
                    obj.insert("ref".to_string(), Value::from(r.clone()));
                    obj.insert("safekey".to_string(), Value::from(safekey.as_str()));
                    obj.insert(payload_key.to_string(), Value::Null);
                    entries.push(Value::Object(obj));
                }
                Err(e) => {
                    entries.push(json!({
                        "ref": r,
                        "error": { "code": ErrorCode::StoreError, "message": format!("store read failed: {e}") },
                    }));
                }
            }
        }

        CallToolResult::structured(json!({ "ok": true, "entries": entries }))
    }
}

/// Default + max clamp for the `limit` field on `doiget_search_local`
/// and `doiget_list_recent`. The cap mirrors the
/// `crates/doiget-cli/src/commands/search.rs::DEFAULT_LIMIT` ceiling.
///
/// `Some(0)` is treated as "use default" rather than literal zero — a
/// caller passing `limit: 0` would otherwise get an empty array that
/// is indistinguishable from "store is empty / no matches", which is
/// a silent-failure trap. Callers that genuinely want "return nothing"
/// should not call the tool at all.
fn clamp_list_limit(limit: Option<u32>) -> usize {
    const DEFAULT: u32 = 50;
    const MAX: u32 = 200;
    let v = match limit {
        None | Some(0) => DEFAULT,
        Some(n) => n,
    };
    let clamped = v.min(MAX);
    clamped as usize
}

/// Project an [`EntryInfo`] summary into the JSON envelope shape used by
/// `doiget_search_local` and `doiget_list_recent`.
///
/// `fetched_at` is rendered as RFC3339 UTC (`%Y-%m-%dT%H:%M:%SZ`) to
/// match the `docs/STORE.md` §2 on-disk wire format. `null` when the
/// stored entry pre-dates the `fetched_at` field.
fn entry_info_to_json(entry: &EntryInfo) -> Value {
    json!({
        "safekey": entry.safekey.as_str(),
        "title": entry.title,
        "year": entry.year,
        "fetched_at": entry.fetched_at.map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string()),
    })
}

/// Build the `{ok:false, error:{code, message}, ref}` envelope used
/// by all four Slice-8 read-path tools. Mirrors
/// `metadata_only_error_envelope` but additionally surfaces `ref` so
/// the envelope is self-describing without inspecting the request.
///
/// `ref` is **always** emitted — `null` when the caller had no ref to
/// surface (e.g., a `doiget_search_local` store-open failure with no
/// per-ref context). This shape-symmetry with the success envelopes
/// (which always carry `"ref"`) means consumers can pattern-match
/// uniformly across `ok:true` / `ok:false` envelopes.
fn read_path_error_envelope(ref_str: Option<&str>, code: ErrorCode, message: &str) -> Value {
    json!({
        "ok": false,
        "ref": ref_str.map(Value::from).unwrap_or(Value::Null),
        "error": {
            "code": code,
            "message": message,
        },
    })
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
fn metadata_only_error_envelope(ref_str: Option<&str>, code: ErrorCode, message: &str) -> Value {
    json!({
        "ok": false,
        // Issue #123: docs/MCP_TOOLS.md §5 mandates `ref` on every
        // ok:false envelope. Emitted always (null when there is no
        // ref to surface) for shape-symmetry with the success
        // envelopes and `read_path_error_envelope`.
        "ref": ref_str.map(Value::from).unwrap_or(Value::Null),
        "error": {
            "code": code,
            "message": message,
            // denial_context is intentionally absent for these envelope
            // shapes (parse-error / not-implemented); ADR-0023 §1 says
            // the field is optional and consumers MUST tolerate it
            // being absent (§3 covers the per-subfield optionality
            // rules that apply when denial_context IS present).
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
        // OA transparency (#281 item 4): gold/green/hybrid/bronze/closed,
        // or null when not determined (e.g. the Crossref-first path).
        "oa_status": outcome.oa_status,
        "metadata": outcome.metadata,
        "schema_version": SCHEMA_VERSION,
    })
}

/// Serialize a [`DenialContext`] to its wire [`Value`], logging a
/// `tracing::warn!` on the (today unreachable) serialization-failure
/// branch instead of silently substituting `null` (#154).
///
/// `DenialContext` is a typed `Serialize` struct with only optional
/// scalar/string/array fields, so `serde_json::to_value` cannot fail in
/// practice. The fallback exists purely so a future non-serializable
/// field cannot panic the server — but a silent `null` would strip the
/// structured recovery payload an agent depends on, with zero trace.
/// `tracing` writes to stderr only (stdout is the JSON-RPC channel and
/// `clippy::print_stdout` is denied crate-wide), so this is safe to log.
fn denial_context_to_value(dc: &DenialContext, surface: &str) -> Value {
    match serde_json::to_value(dc) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                surface,
                error = %e,
                "denial_context serialization failed; emitting null and losing the \
                 structured ADR-0023 recovery payload for this {surface} error envelope"
            );
            Value::Null
        }
    }
}

/// Build the `{ok:false, error:{code, message, denial_context?}}`
/// envelope for orchestrator failures. Maps the [`FetchError`] to the
/// closed [`ErrorCode`] set via the existing
/// `From<FetchError> for ErrorCode` impl, and produces the optional
/// structured `denial_context` channel via
/// `From<&FetchError> for Option<DenialContext>` (ADR-0023 §4).
fn metadata_only_fetch_error_envelope(err: &FetchError, ref_str: &str) -> Value {
    // Use the canonical `From<&FetchError> for ErrorCode` (borrow form, so
    // no clone of the non-`Clone` transport error). This keeps the MCP
    // surface in lock-step with the core mapping — notably `NotFound` and
    // `Ambiguous`, which a hand-rolled wildcard here would incorrectly map
    // to `INTERNAL_ERROR`.
    let code: ErrorCode = ErrorCode::from(err);
    let denial: Option<DenialContext> = err.into();
    let message = err.to_string();

    let mut error_obj = serde_json::Map::new();
    error_obj.insert("code".into(), json!(code));
    error_obj.insert("message".into(), json!(message));
    if let Some(dc) = denial {
        // `DenialContext` is `Serialize` (`#[serde(deny_unknown_fields)]`,
        // optional fields) and `serde_json::to_value` cannot fail on a
        // typed struct today. The fallback to `null` is defensive only
        // (a future non-serializable field), but a silent swallow loses
        // the structured recovery payload with no trace — so log it on
        // stderr (#154). stdout is the JSON-RPC channel; `tracing` is
        // stderr-only.
        error_obj.insert(
            "denial_context".into(),
            denial_context_to_value(&dc, "metadata_only"),
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
#[serde(deny_unknown_fields)]
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
/// Render [`PdfLegStatus`] for the wire. `Blocked` carries the
/// closed-set `code`, a human `message`, and (when the failure was an
/// allowlist / scheme denial) the structured ADR-0023 `denial_context`
/// so an agent can act on WHY the PDF was not retrieved instead of
/// seeing an indistinguishable "metadata-only" success (issue #118).
fn pdf_leg_json(leg: &PdfLegStatus) -> Value {
    match leg {
        PdfLegStatus::Fetched => json!({ "status": "fetched" }),
        PdfLegStatus::NoOaUrl => json!({ "status": "no_oa_url" }),
        PdfLegStatus::Blocked {
            code,
            message,
            denial,
            suggested_arxiv_id,
        } => {
            let mut o = serde_json::Map::new();
            o.insert("status".into(), json!("blocked"));
            o.insert("code".into(), json!(code));
            o.insert("message".into(), json!(message));
            if let Some(dc) = denial {
                // Route through the logged helper (#154): a bare
                // `json!(dc)` here would silently coerce a future
                // serialization failure to `null` inside the
                // `fetch_paper` SUCCESS envelope with no trace —
                // exactly the silent-swallow class #154 eliminates.
                // The other three denial-context sites already use
                // this helper; keep this consistent. `tracing` is
                // stderr-only (stdout is the JSON-RPC channel).
                o.insert(
                    "denial_context".into(),
                    denial_context_to_value(dc, "fetch_paper_pdf_leg"),
                );
            }
            if let Some(arxiv_id) = suggested_arxiv_id {
                o.insert("suggested_arxiv_id".into(), json!(arxiv_id));
            }
            Value::Object(o)
        }
        // Issue #325: publisher blocked but arXiv preprint auto-fetched.
        PdfLegStatus::PreprintFallback {
            arxiv_id,
            original_block,
        } => json!({
            "status": "preprint_fallback",
            "arxiv_id": arxiv_id,
            "original_block": original_block,
        }),
        // `PdfLegStatus` is `#[non_exhaustive]`; a future variant
        // surfaces as a forward-compatible neutral status rather than
        // failing the build in this downstream crate.
        _ => json!({ "status": "unknown" }),
    }
}

fn fetch_paper_success_envelope(outcome: &FetchPaperOutcome, ref_str: &str) -> Value {
    json!({
        "ok": true,
        "ref": ref_str,
        "source": outcome.source,
        // ADR-0021 §4 / ADR-0024: the audit-identity resolver under
        // which the canonical-digest for this fetch was minted.
        "resolver_profile": outcome.resolver_profile,
        "license": outcome.license,
        // OA transparency (#281 item 4): gold/green/hybrid/bronze/closed,
        // or null when not determined. Combined with `pdf.status`, lets an
        // agent tell a paywalled work (`closed` + `no_oa_url`) from one it
        // simply could not reach.
        "oa_status": outcome.oa_status,
        "path": outcome.path,
        "size_bytes": outcome.size_bytes,
        "schema_version": outcome.schema_version,
        // Issue #118: never a silent metadata-only success.
        "pdf": pdf_leg_json(&outcome.pdf_leg),
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
            denial_context_to_value(&dc, "fetch_paper"),
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

/// JSON-schema-derived input for `doiget_batch_from_bibliography` per
/// ADR-0030 D6.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct BatchFromBibliographyInput {
    /// Absolute path to a bibliography file. CSL-JSON (.json / .csl)
    /// is supported in slice 1; BibTeX (.bib) returns a structured
    /// "not yet implemented" error pending the biblatex-crate slice.
    pub path: String,
    /// Optional format override. Accepted tokens (case-insensitive):
    /// `auto` (default — extension + content fingerprint),
    /// `csl-json`, `bibtex`, `refs`. Anything else surfaces as
    /// `INVALID_REF`.
    #[serde(default)]
    pub format: Option<String>,
    /// `true` aborts on the first per-entry parse error (no flushed
    /// successes); `false` (default) emits parse errors as
    /// `{ok:false, error:{code:INVALID_REF, ...}}` rows next to
    /// successful siblings.
    #[serde(default)]
    pub strict: bool,
}

/// Resolve a `--format` token to a [`doiget_core::refs::Format`]. The
/// tokens match the wire strings `Format::as_wire` emits so the MCP
/// input schema and the CLI flag share the same vocabulary
/// (ADR-0030 D4).
fn parse_bibliography_format(token: Option<&str>) -> Result<doiget_core::refs::Format, String> {
    use doiget_core::refs::Format;
    match token.unwrap_or("auto").to_ascii_lowercase().as_str() {
        "auto" => Ok(Format::Auto),
        "refs" => Ok(Format::Refs),
        "csl-json" | "csl_json" | "cslJson" => Ok(Format::CslJson),
        "bibtex" | "biblatex" => Ok(Format::Bibtex),
        other => Err(format!(
            "unknown format {:?}; accepted: auto / refs / csl-json / bibtex",
            other
        )),
    }
}

/// Build the bibliography-tool envelope: a `summary` block + a
/// `results[]` array carrying parse-error rows first, then the
/// fetch outcomes. Each fetch row mirrors `doiget_batch_fetch`'s
/// per-entry shape with an extra `entry_key` field.
fn build_bibliography_envelope(
    batch: &doiget_core::orchestrator::BatchOutcome,
    refs: &[Ref],
    entry_keys: &[Option<String>],
    parse_errors: Vec<Value>,
) -> Value {
    let fetch_ok = batch.results.iter().filter(|r| r.outcome.is_ok()).count();
    let fetch_err = batch.results.len() - fetch_ok;
    let summary = json!({
        "total":        batch.results.len() + parse_errors.len(),
        "ok":           fetch_ok,
        "failed":       fetch_err,
        "parse_errors": parse_errors.len(),
    });

    let mut results: Vec<Value> = parse_errors;
    for ((entry, ref_), key) in batch.results.iter().zip(refs.iter()).zip(entry_keys.iter()) {
        let mut obj = match &entry.outcome {
            Ok(outcome) => serde_json::json!({
                "entry_key":        key,
                "ref":              ref_.as_input_str(),
                "ok":               true,
                "source":           outcome.source,
                "resolver_profile": outcome.resolver_profile,
                "license":          outcome.license,
                "path":             outcome.path,
                "size_bytes":       outcome.size_bytes,
                "schema_version":   outcome.schema_version,
                "pdf":              pdf_leg_json(&outcome.pdf_leg),
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
                        denial_context_to_value(&dc, "batch_from_bibliography"),
                    );
                } else {
                    error_obj.insert("denial_context".into(), Value::Null);
                }
                json!({
                    "entry_key": key,
                    "ref":       ref_.as_input_str(),
                    "ok":        false,
                    "error":     error_obj,
                })
            }
        };
        // Drop `entry_key: null` so the wire is minimal when the
        // source bibliography had no key (e.g. plain-refs input).
        if let Some(map) = obj.as_object_mut() {
            if matches!(map.get("entry_key"), Some(Value::Null)) {
                map.remove("entry_key");
            }
        }
        results.push(obj);
    }

    json!({
        "ok": true,
        "summary": summary,
        "results": results,
    })
}

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
                "pdf": pdf_leg_json(&outcome.pdf_leg),
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
                        denial_context_to_value(&dc, "batch_fetch"),
                    );
                } else {
                    // Per the Slice 2 spec: transport (NETWORK_ERROR)
                    // entries carry `denial_context: null` so an agent
                    // can pattern-match on the field's presence rather
                    // than tolerating absence. This deliberately
                    // differs from the single-paper envelopes
                    // (`doiget_fetch_paper` / `doiget_metadata_only`)
                    // where the field is OMITTED entirely when `None`.
                    // The asymmetry is intentional and documented in
                    // `docs/MCP_TOOLS.md` §5; `tests/fetch_paper_e2e.rs`
                    // pins the explicit-null batch shape (#154).
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

/// Resolve the OpenAlex base URL for `doiget_paper_search`: the
/// `DOIGET_OPENALEX_BASE` override (tests) or the production default.
fn openalex_base() -> Result<url::Url, String> {
    let raw = std::env::var("DOIGET_OPENALEX_BASE")
        .unwrap_or_else(|_| "https://api.openalex.org".to_string());
    url::Url::parse(&raw).map_err(|e| format!("DOIGET_OPENALEX_BASE is not a URL: {e}"))
}

/// Resolve the ar5iv base URL for `doiget_paper_text`: the
/// `DOIGET_AR5IV_BASE` override (tests) or the production default
/// (ADR-0032 D3).
fn ar5iv_base() -> Result<url::Url, String> {
    let raw = std::env::var("DOIGET_AR5IV_BASE")
        .unwrap_or_else(|_| doiget_core::paper_text::AR5IV_DEFAULT_BASE.to_string());
    url::Url::parse(&raw).map_err(|e| format!("DOIGET_AR5IV_BASE is not a URL: {e}"))
}

/// Resolve the arXiv source API base URL for `doiget_paper_tex_source`:
/// the `DOIGET_ARXIV_SRC_BASE` override (tests) or the production default.
fn arxiv_src_base() -> Result<url::Url, String> {
    let raw = std::env::var("DOIGET_ARXIV_SRC_BASE")
        .unwrap_or_else(|_| doiget_core::paper_tex_source::ARXIV_SRC_DEFAULT_BASE.to_string());
    url::Url::parse(&raw).map_err(|e| format!("DOIGET_ARXIV_SRC_BASE is not a URL: {e}"))
}

/// Build the `{ ok:true, arxiv_id, source, title, sections, char_count,
/// truncated, retrieved_from }` success envelope for `doiget_paper_text`
/// (the `PaperText` shape plus the `ok` discriminant).
fn paper_text_success_envelope(t: &doiget_core::paper_text::PaperText) -> Value {
    json!({
        "ok": true,
        "arxiv_id": t.arxiv_id,
        "source": t.source,
        "title": t.title,
        "sections": t.sections,
        "char_count": t.char_count,
        "truncated": t.truncated,
        "retrieved_from": t.retrieved_from,
    })
}

/// Build a [`FetchContext`] for the non-dry-run `doiget_metadata_only`
/// path.
///
/// Mirrors `crates/doiget-cli/src/commands/fetch.rs::FetchHarness::from_env`
/// minus the on-disk store: the `FetchContext` carries no store handle
/// because the store is opened per-call in `doiget_metadata_only` and
/// passed explicitly to `orchestrator::metadata_only_to_store` (the §11
/// store-write entry point), keeping the context store-agnostic:
///
/// - `HttpClient` — production allowlist (Tier 1 ∪ OA publisher ∪ Tier 2 ∪
///   full-text (ar5iv)), or the test-mode multi-source allowlist when any
///   `DOIGET_*_BASE` env var is set.
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
        // Resolver cache disabled on the MCP path for now; the resolve
        // cache (docs/CACHE.md) is wired through `doiget verify` first.
        // Enabling it here for metadata_only / resolve_paper is a follow-up.
        cache_root: None,
    })
}

/// Build a [`CrossrefSource`] from environment variables
/// (`DOIGET_CROSSREF_BASE`, `DOIGET_CONTACT_EMAIL`).
///
/// Returns `Err(String)` — callers convert it into a structured tool error.
fn crossref_source_from_env() -> Result<CrossrefSource, String> {
    let contact_email =
        std::env::var("DOIGET_CONTACT_EMAIL").unwrap_or_else(|_| "doiget@localhost".to_string());
    match std::env::var("DOIGET_CROSSREF_BASE").ok() {
        Some(base_str) => {
            let base = url::Url::parse(&base_str)
                .map_err(|e| format!("invalid DOIGET_CROSSREF_BASE: {e}"))?;
            Ok(CrossrefSource::with_base(base, contact_email))
        }
        None => Ok(CrossrefSource::new(contact_email)),
    }
}

/// HTTP client construction with the same `DOIGET_*_BASE` test-override
/// surface that `doiget-cli` honors (`build_http_client` in
/// `crates/doiget-cli/src/commands/fetch.rs`). When no overrides are
/// set, returns the production allowlist (Tier 1 ∪ OA publisher ∪ Tier 2 ∪
/// full-text (ar5iv)).
fn build_http_client_for_fetch() -> anyhow::Result<HttpClient> {
    let arxiv = std::env::var("DOIGET_ARXIV_BASE").ok();
    let crossref = std::env::var("DOIGET_CROSSREF_BASE").ok();
    let unpaywall = std::env::var("DOIGET_UNPAYWALL_BASE").ok();
    let oa_publisher = std::env::var("DOIGET_OA_PUBLISHER_BASE").ok();

    let openalex_base = std::env::var("DOIGET_OPENALEX_BASE").ok();
    // ADR-0032: ar5iv full-text base override (test wiremock origin).
    let ar5iv_base = std::env::var("DOIGET_AR5IV_BASE").ok();
    // `doiget_paper_tex_source` uses the `"arxiv"` HTTP source key (same key
    // as `DOIGET_ARXIV_BASE`). MCP integration tests that override only the
    // source API can set `DOIGET_ARXIV_SRC_BASE`; in the test-mode path below
    // it is treated as a fallback for the `"arxiv"` source entry when
    // `DOIGET_ARXIV_BASE` is absent.
    let arxiv_src = std::env::var("DOIGET_ARXIV_SRC_BASE").ok();

    if arxiv.is_none()
        && arxiv_src.is_none()
        && crossref.is_none()
        && unpaywall.is_none()
        && oa_publisher.is_none()
        && openalex_base.is_none()
        && ar5iv_base.is_none()
    {
        let mut allowlists = tier_1_allowlist();
        allowlists.extend(oa_publisher_allowlist());
        // Slice 15: Tier 2 allowlist is unioned in unconditionally —
        // the runtime `metadata.openalex` / `.semantic_scholar` /
        // `.doaj` capability flags gate whether the source impls
        // even call `HttpClient::fetch_bytes` under these keys, so
        // including the hosts here cannot widen the network surface
        // beyond what the CapabilityProfile already permits.
        allowlists.extend(tier_2_allowlist());
        // ADR-0032: full-text extraction (`doiget_paper_text`) is Tier-1
        // OA, always-on. Register `ar5iv.labs.arxiv.org` under the
        // `"ar5iv"` source key so `paper_text::paper_text` can reach it.
        allowlists.extend(fulltext_allowlist());

        // ADR-0028 D2: merge user-extension hosts from
        // `<config_dir>/doiget/config.toml`. Mirrors the CLI path in
        // `crates/doiget-cli/src/commands/fetch.rs::build_http_client`
        // so the MCP server sees the same user-curated allowlist
        // additions. Failure handling matches the CLI:
        //   - missing config (file not found) is silent (Ok-empty);
        //   - malformed config emits `tracing::warn!` and continues
        //     with the curated allowlist;
        //   - unresolvable config dir emits `tracing::debug!`.
        match config_dir_utf8() {
            Ok(cfg_dir) => {
                let path = cfg_dir.join("doiget").join("config.toml");
                match doiget_core::user_extension::load(&path) {
                    Ok(cfg) => {
                        let mut hosts = cfg.additional_hosts;
                        if cfg.trust_academic_repos {
                            hosts.extend(doiget_core::user_extension::academic_repo_hosts());
                        }
                        if !hosts.is_empty() {
                            tracing::info!(
                                count = hosts.len(),
                                trust_academic_repos = cfg.trust_academic_repos,
                                path = %path,
                                "merging user-extension allowlist hosts (ADR-0028 D2)"
                            );
                            doiget_core::user_extension::merge_into_allowlists(
                                &mut allowlists,
                                &hosts,
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            path = %path,
                            "failed to load user-extension allowlist; \
                             falling back to curated set only"
                        );
                    }
                }
            }
            Err(e) => {
                tracing::debug!(
                    error = %e,
                    "config dir unresolvable; \
                     user-extension allowlist disabled (curated set only)"
                );
            }
        }

        return HttpClient::new(allowlists)
            .map_err(|e| anyhow::anyhow!("building production HTTP client: {e}"));
    }

    let mut owned: Vec<(String, String)> = Vec::new();
    // When DOIGET_ARXIV_BASE is set use it; otherwise fall back to
    // DOIGET_ARXIV_SRC_BASE (they share the "arxiv" HTTP source key).
    let arxiv_entry = arxiv.as_deref().or(arxiv_src.as_deref());
    for (source, base) in [
        ("arxiv", arxiv_entry),
        ("crossref", crossref.as_deref()),
        ("unpaywall", unpaywall.as_deref()),
        ("oa-publisher", oa_publisher.as_deref()),
        ("openalex", openalex_base.as_deref()),
        ("ar5iv", ar5iv_base.as_deref()),
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

/// Best-effort config-dir resolution. Honors `XDG_CONFIG_HOME` first
/// (POSIX), then `APPDATA` (Windows), then falls back to
/// `$HOME/.config` (or `%USERPROFILE%\.config` on Windows). Mirrors
/// `crates/doiget-cli/src/commands/fetch.rs::config_dir_utf8` so the
/// MCP server reads `<config_dir>/doiget/config.toml` from the same
/// location the CLI writes it.
///
/// Returns `Err` only when none of `XDG_CONFIG_HOME` / `APPDATA` /
/// `HOME` / `USERPROFILE` are set (or all set to empty), in which
/// case the caller downgrades to "user extension disabled" rather
/// than failing the whole request.
fn config_dir_utf8() -> anyhow::Result<Utf8PathBuf> {
    if let Ok(s) = std::env::var("XDG_CONFIG_HOME") {
        if !s.is_empty() {
            return Ok(Utf8PathBuf::from(s));
        }
    }
    if let Ok(s) = std::env::var("APPDATA") {
        if !s.is_empty() {
            return Ok(Utf8PathBuf::from(s));
        }
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_err(|_| anyhow::anyhow!("neither HOME nor USERPROFILE is set"))?;
    Ok(Utf8PathBuf::from(home).join(".config"))
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
/// `docs/MCP_TOOLS.md` §7 (NORMATIVE).
///
/// The §7 contract requires
/// `{ oa_enabled, metadata_sources: string[], tdm_enabled, tdm_elsevier,
/// tdm_aps, tdm_springer, rate_limit_per_sec }`. We emit exactly those
/// fields and additionally retain `ok` and `tier_1/2/3` as **additive**
/// fields — the e2e handshake test and pre-#141 agents rely on them and
/// §7 (a TypeScript object type, structurally open) does not forbid
/// extra keys. `metadata_sources` is the spec-canonical view of the
/// enabled Tier-2 metadata sources.
///
/// - `metadata_sources` reflects the `MetadataAccess` booleans (the
///   enabled Tier-2 source names; always-on Tier-1 sources are reported
///   separately via the additive `tier_1` field).
/// - Tier 1 is always `["arxiv", "crossref", "unpaywall"]` (sorted for
///   deterministic output).
/// - Tier 3 reflects which `tdm_*` slots are `Some(...)`.
fn capability_profile_to_json(profile: &CapabilityProfile) -> Value {
    let tier_1 = vec!["arxiv", "crossref", "unpaywall"];

    // `metadata_sources` (spec §7) == the enabled Tier-2 metadata
    // sources. Order is deterministic (declaration order).
    let mut metadata_sources: Vec<&str> = Vec::new();
    if profile.metadata.openalex {
        metadata_sources.push("openalex");
    }
    if profile.metadata.semantic_scholar {
        metadata_sources.push("semantic_scholar");
    }
    if profile.metadata.doaj {
        metadata_sources.push("doaj");
    }
    // Additive alias kept for back-compat with pre-#141 consumers.
    let tier_2 = metadata_sources.clone();

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
        // -- NORMATIVE §7 contract fields --
        "oa_enabled": true,
        "metadata_sources": metadata_sources,
        "tdm_enabled": tdm_enabled,
        "tdm_elsevier": profile.tdm_elsevier.is_some(),
        "tdm_aps": profile.tdm_aps.is_some(),
        "tdm_springer": profile.tdm_springer.is_some(),
        "rate_limit_per_sec": profile.rate_limits.max_fetches_per_second(),
        // -- additive (back-compat; not part of the §7 contract) --
        "ok": true,
        "tier_1": tier_1,
        "tier_2": tier_2,
        "tier_3": tier_3,
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
        // NORMATIVE §7 contract fields.
        assert_eq!(v["oa_enabled"], true);
        assert!(
            v["metadata_sources"].is_array(),
            "§7 requires metadata_sources: string[]; got: {v:?}"
        );
        assert_eq!(v["tdm_enabled"], json!(false));
        assert_eq!(v["tdm_elsevier"], json!(false));
        assert_eq!(v["tdm_aps"], json!(false));
        assert_eq!(v["tdm_springer"], json!(false));
        assert_eq!(v["rate_limit_per_sec"], 5.0);
        // Additive back-compat fields.
        assert_eq!(v["ok"], true);
        assert_eq!(v["tier_1"], json!(["arxiv", "crossref", "unpaywall"]));
    }

    // ---- ADR-0030 D6: doiget_batch_from_bibliography helpers ------

    #[test]
    fn parse_bibliography_format_auto_when_missing_or_empty() {
        assert_eq!(
            parse_bibliography_format(None).unwrap(),
            doiget_core::refs::Format::Auto
        );
    }

    #[test]
    fn parse_bibliography_format_accepts_canonical_tokens() {
        assert_eq!(
            parse_bibliography_format(Some("auto")).unwrap(),
            doiget_core::refs::Format::Auto
        );
        assert_eq!(
            parse_bibliography_format(Some("refs")).unwrap(),
            doiget_core::refs::Format::Refs
        );
        assert_eq!(
            parse_bibliography_format(Some("csl-json")).unwrap(),
            doiget_core::refs::Format::CslJson
        );
        assert_eq!(
            parse_bibliography_format(Some("bibtex")).unwrap(),
            doiget_core::refs::Format::Bibtex
        );
    }

    #[test]
    fn parse_bibliography_format_is_case_insensitive() {
        assert_eq!(
            parse_bibliography_format(Some("CSL-JSON")).unwrap(),
            doiget_core::refs::Format::CslJson
        );
        assert_eq!(
            parse_bibliography_format(Some("BibTeX")).unwrap(),
            doiget_core::refs::Format::Bibtex
        );
    }

    #[test]
    fn parse_bibliography_format_accepts_underscore_variant() {
        // Some MCP clients prefer underscore tokens; honor both.
        assert_eq!(
            parse_bibliography_format(Some("csl_json")).unwrap(),
            doiget_core::refs::Format::CslJson
        );
        assert_eq!(
            parse_bibliography_format(Some("biblatex")).unwrap(),
            doiget_core::refs::Format::Bibtex
        );
    }

    #[test]
    fn parse_bibliography_format_rejects_unknown_token() {
        let err = parse_bibliography_format(Some("rdf")).unwrap_err();
        assert!(
            err.contains("rdf") && err.contains("auto"),
            "error must name the offending token AND the accepted set: {err}"
        );
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

    /// ADR-0028 D2: `build_http_client_for_fetch` MUST merge user-
    /// extension allowlist hosts from `<config_dir>/doiget/config.toml`
    /// into the oa-publisher allowlist before returning, mirroring the
    /// CLI's `commands::fetch::build_http_client`. Drift here would
    /// silently disable user-curated allowlist additions for the MCP
    /// server while the CLI honors them.
    ///
    /// We can't read the HttpClient's internal allowlists directly
    /// (the field is private), so we drive an end-to-end probe: write
    /// a config.toml that adds `host = "ruj.uj.edu.pl"` to the user
    /// extension, build the client, and call
    /// `oa_publisher_allowlist_hosts` — the same helper the CLI test
    /// uses (review pass M3). The function exposes the merged host
    /// list without leaking client internals.
    #[test]
    #[serial_test::serial]
    fn build_http_client_for_fetch_merges_user_extension_hosts() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let cfg_root = camino::Utf8Path::from_path(tmp.path()).expect("utf8 tempdir");
        let doiget_dir = cfg_root.join("doiget");
        std::fs::create_dir_all(doiget_dir.as_std_path()).expect("mk dir");
        std::fs::write(
            doiget_dir.join("config.toml").as_std_path(),
            "[[network.additional_hosts]]\nhost = \"ruj.uj.edu.pl\"\n",
        )
        .expect("write config.toml");

        let _guards = scoped_env_for_user_extension(cfg_root.as_str());

        // No DOIGET_*_BASE overrides → takes the production branch
        // that performs the user-extension merge.
        let client = build_http_client_for_fetch().expect("build http client");
        let oa = client
            .source_allowlist("oa-publisher")
            .expect("oa-publisher source must be registered");
        assert!(
            oa.redirect_hosts.iter().any(|h| h == "ruj.uj.edu.pl"),
            "user-extension host must appear in the oa-publisher allowlist; \
             got: {:?}",
            oa.redirect_hosts
        );
    }

    /// Set XDG_CONFIG_HOME / APPDATA / HOME / USERPROFILE so
    /// `config_dir_utf8()` resolves to the supplied directory on
    /// every supported test host. Returns guards that restore prior
    /// values on drop.
    fn scoped_env_for_user_extension(dir: &str) -> Vec<EnvGuard> {
        vec![
            EnvGuard::set("XDG_CONFIG_HOME", dir),
            EnvGuard::set("APPDATA", dir),
            EnvGuard::set("HOME", dir),
            EnvGuard::set("USERPROFILE", dir),
            EnvGuard::unset("DOIGET_ARXIV_BASE"),
            EnvGuard::unset("DOIGET_CROSSREF_BASE"),
            EnvGuard::unset("DOIGET_UNPAYWALL_BASE"),
            EnvGuard::unset("DOIGET_OA_PUBLISHER_BASE"),
            EnvGuard::unset("DOIGET_OPENALEX_BASE"),
        ]
    }

    /// RAII env guard local to this tests module.
    struct EnvGuard {
        var: &'static str,
        prior: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(var: &'static str, value: &str) -> Self {
            let prior = std::env::var_os(var);
            std::env::set_var(var, value);
            EnvGuard { var, prior }
        }
        fn unset(var: &'static str) -> Self {
            let prior = std::env::var_os(var);
            std::env::remove_var(var);
            EnvGuard { var, prior }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.prior {
                Some(v) => std::env::set_var(self.var, v),
                None => std::env::remove_var(self.var),
            }
        }
    }
}
