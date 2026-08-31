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
    fulltext_allowlist, oa_publisher_allowlist, tier_1_allowlist, tier_2_allowlist,
    tier_3_allowlists, HttpClient,
};
use doiget_core::orchestrator::{
    batch_fetch as core_batch_fetch, batch_fetch_plans, fetch_paper as core_fetch_paper,
    metadata_only_to_store_with_options, resolve_only_with_options as core_resolve_only,
    FetchPaperOutcome, MetadataOnlyOptions, MetadataOnlyOutcome, PdfLegStatus,
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
    /// on the inherent impl block below and then trimmed per build
    /// features in [`Server::new`].
    ///
    /// `#[tool_handler(router = self.tool_router)]` reads THIS field rather
    /// than the associated fn `Self::tool_router()`, which is what makes
    /// the per-instance trimming visible to `tools/list` and `tools/call`
    /// (issue #379). Do not switch the handler back to the associated fn.
    tool_router: ToolRouter<Server>,
}

#[tool_router]
impl Server {
    /// Construct a server with the given runtime capability profile.
    pub fn new(profile: CapabilityProfile) -> Self {
        let mut tool_router = Self::tool_router();
        // Issue #379 / #373(b): a tool that can only ever answer
        // NOT_IMPLEMENTED is worse than an absent one — an agent will
        // plan around it, call it, and get a dead end it cannot act on.
        // `doiget_expand_citation_graph` needs the `citation` Cargo
        // feature (ADR-0010), which the default `cargo install` build
        // does not enable, so drop it from the router in that build and
        // it disappears from `tools/list` and `tools/call` alike.
        //
        // The `#[tool]` method itself stays unconditional: rmcp's
        // `#[tool_router]` macro generates registration code that names
        // every `#[tool]` method, so `#[cfg]`-gating the method out does
        // not compile (E0599 on the generated `..._tool_attr`). Removing
        // the route at construction is the supported way to express this
        // — `ToolRouter::remove_route` landed in rmcp 2.x, which is why
        // #379 was blocked when the repo was on 1.7.
        if !cfg!(feature = "citation") {
            tool_router.remove_route("doiget_expand_citation_graph");
        }
        Self {
            profile,
            tool_router,
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
    /// `store_writable` is a best-effort probe of the nearest existing
    /// ancestor of the store root — see [`probe_store_writable`]. It
    /// creates nothing, which is what lets this tool honour its
    /// `read_only_hint = true` annotation (issue #406).
    #[tool(
        description = "WHEN TO USE: Operational sanity check for the doiget MCP server.\n\
                       INPUTS: none.\n\
                       OUTPUTS: { ok: true, version, schema_version, store_writable }.\n\
                       COSTS: <1 ms.\n\
                       SIDE EFFECTS: none.\n\
                       LIMITS: store_writable is a best-effort probe of the nearest existing ancestor; it never creates the store.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
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
    /// `ok`, `tier_1/2/3` and `tdm_ieee` (#430) are emitted additively.
    #[tool(
        description = "WHEN TO USE: Determine which sources the running doiget instance is allowed to use.\n\
                       INPUTS: none.\n\
                       OUTPUTS: { oa_enabled, metadata_sources, tdm_enabled, tdm_elsevier, tdm_aps, tdm_springer, rate_limit_per_sec } (plus additive ok, tier_1, tier_2, tier_3, tdm_ieee).\n\
                       COSTS: <1 ms.\n\
                       SIDE EFFECTS: none.\n\
                       LIMITS: none.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
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
                       INPUTS: ref (DOI or arXiv id), dry_run (optional bool), include_oa_location (optional bool).\n\
                       OUTPUTS: { ok: true, ref, source, license?, oa_url, oa_status, metadata } OR { ok: true, dry_run: true, ref, plan, rate_limit_budget } OR { ok:false, error }.\n\
                       COSTS: 1-2 s metadata round-trip (or 0 when dry_run; roughly doubled when include_oa_location).\n\
                       SIDE EFFECTS: Appends a 'metadata-only' provenance row (unless dry_run). Writes the metadata TOML to the store. Never fetches PDF.\n\
                       LIMITS: Subject to the same rate cap as fetch_paper (5/sec). The OA URL is reported but never followed. Crossref alone cannot supply an oa_url, because its link[] entries are scoped to a licensed programme (Similarity Check / TDM / syndication) rather than being general-purpose. A null oa_url is therefore NOT evidence that the work is closed. oa_url is null on the default path whenever Crossref answered, which is nearly every DOI. It can be non-null WITHOUT the flag in one case: Crossref failed and Unpaywall answered instead, and then source is unpaywall rather than crossref - so source tells you which happened. With include_oa_location set, oa_status says which answer you got: closed means the lookup completed and found no OA location, null means the lookup did not complete.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
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
            // fetch would write to. When the cwd can't be resolved (the
            // `None` case — e.g. a non-UTF-8 working directory), fall back to
            // `./papers` so the preview still has a complete shape — the
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
                    "store root could not be resolved (set DOIGET_STORE_ROOT, or run from a directory with a valid UTF-8 path)",
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

        let opts = MetadataOnlyOptions::default().with_oa_location(input.include_oa_location);
        let outcome =
            metadata_only_to_store_with_options(&ref_, &self.profile, &ctx, &store, opts).await;

        // SessionEnd bookend. Best-effort: if this append fails we still
        // surface the orchestrator's outcome (a fresh log error here
        // would mask the more informative orchestrator error).
        let session_ok = outcome.is_ok();
        // #507: the bookend recorded THAT the call failed and not WHAT it
        // failed with, so the provenance log could not answer "what did this
        // session tell the caller about this ref?" -- which is the question
        // repeat suppression has to ask before it can suppress anything.
        let session_err = outcome.as_ref().err().map(|e| ErrorCode::from(e).as_wire());
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
            error_code: session_err,
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
                       INPUTS: ref (DOI or arXiv id), include_oa_location (optional bool).\n\
                       OUTPUTS: { ok: true, ref, source, resolver_profile, license?, oa_url, oa_status, metadata, schema_version } OR { ok:false, ref, error }.\n\
                       COSTS: 1-2 s metadata round-trip (roughly doubled when include_oa_location).\n\
                       SIDE EFFECTS: Appends one provenance row per consulted resolver. NEVER writes a metadata TOML to the store. NEVER fetches PDF.\n\
                       LIMITS: Subject to the same rate cap as metadata_only (5/sec). The OA URL is reported but never followed. Crossref alone cannot supply an oa_url, because its link[] entries are scoped to a licensed programme (Similarity Check / TDM / syndication) rather than being general-purpose. A null oa_url is therefore NOT evidence that the work is closed. oa_url is null on the default path whenever Crossref answered, which is nearly every DOI. It can be non-null WITHOUT the flag in one case: Crossref failed and Unpaywall answered instead, and then source is unpaywall rather than crossref - so source tells you which happened. With include_oa_location set, oa_status says which answer you got: closed means the lookup completed and found no OA location, null means the lookup did not complete. dry_run is not supported; use metadata_only with dry_run for a preview.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
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

        let opts = MetadataOnlyOptions::default().with_oa_location(input.include_oa_location);
        let outcome = core_resolve_only(&ref_, &self.profile, &ctx, opts).await;

        // SessionEnd bookend. Best-effort.
        let session_ok = outcome.is_ok();
        // #507: the bookend recorded THAT the call failed and not WHAT it
        // failed with, so the provenance log could not answer "what did this
        // session tell the caller about this ref?" -- which is the question
        // repeat suppression has to ask before it can suppress anything.
        let session_err = outcome.as_ref().err().map(|e| ErrorCode::from(e).as_wire());
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
            error_code: session_err,
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
                       OUTPUTS: { ok: true, ref, source, path, license, size_bytes, schema_version, pdf, attempts } OR { ok: true, dry_run: true, ref, plan, rate_limit_budget } OR { ok:false, ref, error }.\n\
                       READ `pdf.status` — `ok: true` does NOT mean a PDF landed. `fetched` = PDF on disk; `no_oa_url` = metadata only, no free copy exists; `blocked` = a free copy EXISTS but was refused.\n\
                       ON `blocked`: do not report the paper as unavailable. `pdf.remediation` lists the config changes that would lift it, narrowest first — `additional_host` entries go under [[network.additional_hosts]] in the config file, a `trust_flag` is a [network] boolean. Show them to the user and let them choose; both widen the trusted download surface.\n\
                       `attempts` says which other sources were consulted and which were never asked (`consulted: false` + `detail` naming the env var to set). Use it before concluding a paper is not indexed anywhere.\n\
                       COSTS: 1-3 s network call (or 0 when dry_run). May fail if not Open Access.\n\
                       SIDE EFFECTS: Writes PDF (or metadata-only TOML) to the store. Appends a row to the provenance log (unless dry_run).\n\
                       LIMITS: Max 5 fetches/sec (global). Use doiget_batch_fetch for >5 refs.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
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
                    "could not resolve store root (set DOIGET_STORE_ROOT, or run from a directory with a valid UTF-8 path)",
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
        // #507, second surface. `core_fetch_paper` returns `Ok` with a FAILED
        // leg when an OA URL was found and refused, so `outcome.err()` is
        // `None` and the row would say the call succeeded with no error code
        // -- for the one outcome an agent is most likely to retry. The CLI
        // sibling special-cases exactly this (`commands/fetch.rs`); the MCP
        // path did not, so the same fix landed on one surface and not the
        // other. Found by review of this change.
        let blocked_code = match outcome.as_ref() {
            Ok(o) => match &o.pdf_leg {
                doiget_core::orchestrator::PdfLegStatus::Blocked { code, .. } => {
                    Some(code.as_wire())
                }
                _ => None,
            },
            Err(_) => None,
        };
        // #507: the bookend recorded THAT the call failed and not WHAT it
        // failed with, so the provenance log could not answer "what did this
        // session tell the caller about this ref?" -- which is the question
        // repeat suppression has to ask before it can suppress anything.
        let session_err = outcome
            .as_ref()
            .err()
            .map(|e| ErrorCode::from(e).as_wire())
            .or(blocked_code);
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
            error_code: session_err,
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
                       LIMITS: Max 100 refs per call (TOO_MANY_REFS otherwise). Per-ref errors are reported in `results` and do NOT fail the whole call.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
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
                    "could not resolve store root (set DOIGET_STORE_ROOT, or run from a directory with a valid UTF-8 path)",
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
                       LIMITS: bibtex parsing is not yet shipped (re-export as CSL-JSON). Per-entry parse errors are reported in results unless strict=true.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
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
                        "error": error_object(ErrorCode::InvalidRef, source.to_string()),
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
                        "error": error_object(ErrorCode::InvalidRef, "entry has no DOI / arXiv id"),
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
                        "error": error_object(ErrorCode::InvalidRef, "unhandled bibliography parse error"),
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
                    "could not resolve store root (set DOIGET_STORE_ROOT, or run from a directory with a valid UTF-8 path)",
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
                       LIMITS: A missing entry surfaces as { ok: true, metadata: null } — NOT an error envelope. Check `metadata !== null` to confirm presence; call doiget_fetch_paper first when `metadata` is null.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
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
                    "could not resolve store root (set DOIGET_STORE_ROOT, or run from a directory with a valid UTF-8 path)",
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
                       LIMITS: Returns at most `limit` entries (capped at 200).",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
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
                       LIMITS: Tier-1, always-on (no DOIGET_ENABLE_OPENALEX gate). An ambiguous author/venue/publisher name → AMBIGUOUS (candidates listed); no match → NOT_FOUND.\n\
                       ZERO RESULTS ARE NOT EVIDENCE OF ABSENCE: OpenAlex free-text matching degrades sharply past roughly 8 terms and returns nothing rather than a partial match. On total_results: 0, retry with 3-5 distinctive terms before concluding the work is not indexed; the envelope carries a `hint` saying so.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
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
        //
        // Goes through the core ladder so `[network] contact_email` in
        // `config.toml` counts here too — reading the env var directly meant
        // #504's fix stopped at `doiget fetch` and never reached the MCP
        // tools, which is the interface doiget leads with.
        let contact_email =
            doiget_core::orchestrator::configured_contact_email().unwrap_or_default();

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
        // #507: the bookend recorded THAT the call failed and not WHAT it
        // failed with, so the provenance log could not answer "what did this
        // session tell the caller about this ref?" -- which is the question
        // repeat suppression has to ask before it can suppress anything.
        let session_err = outcome.as_ref().err().map(|e| ErrorCode::from(e).as_wire());
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
            error_code: session_err,
            size_bytes: None,
            license: None,
            store_path: None,
            canonical_digest: None,
        });

        match outcome {
            Ok(results) => {
                let mut envelope = json!({
                    "ok": true,
                    "scope": "external",
                    "query": input.query,
                    "total_results": results.total_results,
                    "count": results.results.len(),
                    "results": results.results,
                });
                // A zero-result search is a SUCCESS envelope, so #506's work on
                // error dispositions does not reach it. An agent reads
                // `ok: true` with an empty array as a fact about the world --
                // the paper is not indexed -- and stops looking. That happened:
                // eleven consecutive searches returned 0 for papers a shorter
                // query then found on the first try (#534).
                //
                // The cause is query length, not absence, so the envelope says
                // so at the exact point an agent would otherwise conclude
                // absence.
                if results.results.is_empty() {
                    if let Some(hint) = zero_result_hint(&input.query) {
                        envelope["hint"] = json!(hint);
                    }
                }
                Ok(CallToolResult::structured(envelope))
            }
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
                       LIMITS: arXiv only (a DOI → NO_OA_AVAILABLE; pass the arXiv id). A paper not converted by ar5iv → NOT_FOUND. Best-effort extraction (truncation flagged on `truncated`).",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
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
                       LIMITS: arXiv only (a DOI → NO_OA_AVAILABLE; pass the arXiv id). PDF-only submissions → TEXT_UNAVAILABLE.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
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
                       LIMITS: DOI input only (arXiv → DOI is a follow-up; an arXiv/invalid ref → INVALID_REF). A DOI with no OpenAlex work → NOT_FOUND.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
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
        let contact_email =
            doiget_core::orchestrator::configured_contact_email().unwrap_or_default();

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
                       LIMITS: Returns at most `limit` entries (capped at 200).",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
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
                       LIMITS: Both 'no metadata entry' and 'metadata exists but PDF file missing' surface as { ok: true, path: null, pdf_exists: false } — call doiget_info to distinguish the two cases. Returns an ok:false envelope only on invalid ref / store-open failure.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
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
                       LIMITS: ADR-0010 hard caps applied regardless of inputs: depth<=3, total<=100, per_paper<=20. Requires DOIGET_ENABLE_OPENALEX in env. Returns NOT_IMPLEMENTED when this binary was built without the `citation` Cargo feature.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
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
                "error": error_object(
                    ErrorCode::NotImplemented,
                    "doiget_expand_citation_graph requires the `citation` Cargo feature; this binary was built without it",
                ),
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

            let contact_email = doiget_core::orchestrator::contact_email_or_placeholder();
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
            // #507: the bookend recorded THAT the call failed and not WHAT it
            // failed with, so the provenance log could not answer "what did this
            // session tell the caller about this ref?" -- which is the question
            // repeat suppression has to ask before it can suppress anything.
            let session_err = outcome.as_ref().err().map(|e| ErrorCode::from(e).as_wire());
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
                error_code: session_err,
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
                       LIMITS: Entry must already have been fetched (bibtex:null otherwise — NOT an error). At most 200 refs per call.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
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
                       LIMITS: Entry must already have been fetched (csl:null otherwise — NOT an error). At most 200 refs per call.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
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
                       OUTPUTS: { ok: true, query, candidates: [ { doi, title, author, year, score, confidence, matched, source } ] } OR { ok: false, error }.\n\
                       COSTS: 1-2 s round-trip.\n\
                       SIDE EFFECTS: none.\n\
                       LIMITS: Returns candidates with similarity score >= 0.5. BRANCH ON confidence, NOT score: the score is token overlap against your query string and 0.5 is the FLOOR, so the worst candidate this tool can emit still looks like a positive number. confidence is exact (every query token matched), probable (four in five), or weak (cleared the floor and no more) - for a known-item lookup a weak candidate is a near-miss, not a match, so verify it with doiget_resolve_paper before citing. matched lists which of your tokens were found, which is how you see whether the author and the journal were among them.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
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
                       OUTPUTS: { ok: true, results: [ { query, candidates: [ { doi, title, author, year, score, confidence, matched, source } ] } ] } OR { ok: false, error }.\n\
                       COSTS: 1-2 s round-trip per query.\n\
                       SIDE EFFECTS: none.\n\
                       LIMITS: Returns candidates with similarity score >= 0.5. At most 50 queries per call. BRANCH ON confidence, NOT score: the score is token overlap against your query string and 0.5 is the FLOOR, so the worst candidate this tool can emit still looks like a positive number. confidence is exact (every query token matched), probable (four in five), or weak (cleared the floor and no more) - for a known-item lookup a weak candidate is a near-miss, not a match, so verify it with doiget_resolve_paper before citing. matched lists which of your tokens were found, which is how you see whether the author and the journal were among them.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
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
                       LIMITS: Entry must already exist in the store. Tags are case-sensitive; idempotent add/remove.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
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
                       LIMITS: Entry must already exist in the store. Setting text overrides any existing annotation.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
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
    /// Consult Unpaywall for a real OA location, filling `oa_url`,
    /// `oa_status` and `license`. Costs one extra metadata round-trip.
    ///
    /// Defaults to `false`. On the default path `oa_url` is `null` whenever
    /// Crossref answered -- which is nearly every DOI -- because Crossref's
    /// `link[]` is a programme-scoped channel (Similarity Check, TDM,
    /// syndication), never a general-purpose OA URL (#517). Before #539 the
    /// field was advertised without that caveat, so an agent could read a
    /// permanent `null` as "this work has no OA location".
    ///
    /// It is NOT unconditionally null without the flag, and an earlier
    /// version of this doc said it was. `metadata_only_doi` keeps a
    /// pre-existing fallback: when Crossref FAILS, Unpaywall is consulted
    /// regardless of this flag, and `oa_url` / `oa_status` come from that
    /// record. `source` distinguishes the two -- `crossref` means the
    /// default path answered, `unpaywall` means the fallback did.
    ///
    /// Set it and `oa_status` tells you which answer you got: `"closed"`
    /// means the lookup completed and there is no OA location, while
    /// `null` means the lookup itself did not complete.
    ///
    /// Plain `bool` rather than `Option<bool>` for the same reason as
    /// `dry_run`: a wire `null` should be rejected at deserialize time
    /// rather than silently meaning "no".
    #[serde(default)]
    pub include_oa_location: bool,
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
    /// Consult Unpaywall for a real OA location, filling `oa_url`,
    /// `oa_status` and `license`. Costs one extra metadata round-trip.
    ///
    /// Defaults to `false`. On the default path `oa_url` is `null` whenever
    /// Crossref answered -- which is nearly every DOI -- because Crossref's
    /// `link[]` is a programme-scoped channel (Similarity Check, TDM,
    /// syndication), never a general-purpose OA URL (#517). Before #539 the
    /// field was advertised without that caveat, so an agent could read a
    /// permanent `null` as "this work has no OA location".
    ///
    /// It is NOT unconditionally null without the flag, and an earlier
    /// version of this doc said it was. `metadata_only_doi` keeps a
    /// pre-existing fallback: when Crossref FAILS, Unpaywall is consulted
    /// regardless of this flag, and `oa_url` / `oa_status` come from that
    /// record. `source` distinguishes the two -- `crossref` means the
    /// default path answered, `unpaywall` means the fallback did.
    ///
    /// Set it and `oa_status` tells you which answer you got: `"closed"`
    /// means the lookup completed and there is no OA location, while
    /// `null` means the lookup itself did not complete.
    ///
    /// Plain `bool` rather than `Option<bool>` for the same reason as
    /// `dry_run`: a wire `null` should be rejected at deserialize time
    /// rather than silently meaning "no".
    #[serde(default)]
    pub include_oa_location: bool,
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
    #[serde(default, deserialize_with = "de_lenient_opt_num")]
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
    #[serde(default, deserialize_with = "de_lenient_opt_num")]
    pub limit: Option<u32>,
    /// Inclusive lower publication-year bound.
    #[serde(default, deserialize_with = "de_lenient_opt_num")]
    pub from_year: Option<i32>,
    /// Inclusive upper publication-year bound.
    #[serde(default, deserialize_with = "de_lenient_opt_num")]
    pub to_year: Option<i32>,
    /// Restrict to open-access works.
    #[serde(default)]
    pub oa_only: Option<bool>,
    /// Only works cited strictly more than this many times.
    #[serde(default, deserialize_with = "de_lenient_opt_num")]
    pub min_citations: Option<u64>,
    /// Minimum field-and-year-normalized impact (FWCI) — a quality filter
    /// (#290).
    #[serde(default, deserialize_with = "de_lenient_opt_num")]
    pub min_fwci: Option<f64>,
    /// Minimum within-cohort citation percentile (0–100): top-X% among
    /// same-year works; combine with `from_year` for "recent and standing
    /// out" (#290).
    #[serde(default, deserialize_with = "de_lenient_opt_num")]
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
    #[serde(default, deserialize_with = "de_lenient_opt_num")]
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
    #[serde(default, deserialize_with = "de_lenient_opt_num")]
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
    #[serde(default, deserialize_with = "de_lenient_opt_num")]
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
    #[serde(default, deserialize_with = "de_lenient_opt_num")]
    pub depth: Option<u32>,
    /// Max total nodes (1..=100). Default is the ADR-0010 maximum
    /// (100). `truncated: true` is set on the response when this
    /// cap is hit.
    #[serde(default, deserialize_with = "de_lenient_opt_num")]
    pub total: Option<u32>,
    /// Max children expanded per parent (1..=20). Default is the
    /// ADR-0010 maximum (20).
    #[serde(default, deserialize_with = "de_lenient_opt_num")]
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
    #[serde(default = "default_resolve_limit", deserialize_with = "de_lenient_num")]
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
    #[serde(default = "default_resolve_limit", deserialize_with = "de_lenient_num")]
    pub limit: u8,
}

fn default_resolve_limit() -> u8 {
    5
}

/// A numeric value that may arrive as a JSON number (`10`) or a JSON string
/// (`"10"`). Some MCP clients / LLMs stringify numeric arguments; doiget
/// accepts either form. Private helper for the lenient numeric deserializers.
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum NumOrStr<T> {
    Num(T),
    Str(String),
}

/// Deserialize a required number leniently: accepts a JSON number or a
/// stringified number (`"10"`); a non-numeric string is rejected. The
/// published input schema is unchanged — schemars derives it from the field's
/// concrete type, so the tool still advertises `integer` / `number`; only the
/// runtime is lenient (Postel's law, #370).
fn de_lenient_num<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: std::str::FromStr + serde::Deserialize<'de>,
    <T as std::str::FromStr>::Err: std::fmt::Display,
{
    match NumOrStr::<T>::deserialize(deserializer)? {
        NumOrStr::Num(n) => Ok(n),
        NumOrStr::Str(s) => s.trim().parse::<T>().map_err(serde::de::Error::custom),
    }
}

/// `Option` variant of [`de_lenient_num`]: accepts a number, a stringified
/// number, JSON `null`, or (with `#[serde(default)]`) an absent field. An
/// empty / whitespace-only string is treated as absent (`None`).
fn de_lenient_opt_num<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: std::str::FromStr + serde::Deserialize<'de>,
    <T as std::str::FromStr>::Err: std::fmt::Display,
{
    match Option::<NumOrStr<T>>::deserialize(deserializer)? {
        None => Ok(None),
        Some(NumOrStr::Num(n)) => Ok(Some(n)),
        Some(NumOrStr::Str(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                trimmed
                    .parse::<T>()
                    .map(Some)
                    .map_err(serde::de::Error::custom)
            }
        }
    }
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
                    "could not resolve store root (set DOIGET_STORE_ROOT, or run from a directory with a valid UTF-8 path)",
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
                        "error": error_object(ErrorCode::InvalidRef, format!("invalid ref: {e}")),
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
                        "error": error_object(ErrorCode::StoreError, format!("store read failed: {e}")),
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
/// The `error` object every failure envelope carries (#506).
///
/// One builder rather than ten literals, because the point of `disposition`
/// is that an agent can rely on it being there. A field present on some
/// failures and absent on others is worse than no field: it teaches the reader
/// to fall back to guessing from the code's name, which is the habit this
/// exists to replace.
///
/// `disposition` is derived by [`doiget_core::ErrorCode::disposition`], which
/// is the same function `docs/ERRORS.md` §2's Disposition column is asserted
/// against.
fn error_object(code: ErrorCode, message: impl Into<Value>) -> Value {
    json!({
        "code": code,
        "message": message.into(),
        "disposition": code.disposition().as_wire(),
    })
}

fn read_path_error_envelope(ref_str: Option<&str>, code: ErrorCode, message: &str) -> Value {
    json!({
        "ok": false,
        "ref": ref_str.map(Value::from).unwrap_or(Value::Null),
        "error": error_object(code, message),
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
        // denial_context is intentionally absent for these envelope shapes
        // (parse-error / not-implemented); ADR-0023 §1 says the field is
        // optional and consumers MUST tolerate it being absent (§3 covers the
        // per-subfield optionality rules that apply when it IS present).
        "error": error_object(code, message),
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
    // #506: these five objects are assembled by hand rather than through
    // `error_object`, so the first pass at the disposition missed them --
    // including the two most common failures an agent sees. A field that is
    // present on some failures and absent on others is worse than none.
    error_obj.insert("disposition".into(), json!(code.disposition().as_wire()));
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
        // #506: `denial_context` says what was refused; this says what to do
        // about it. `docs/ERRORS.md` §3 used to state outright that
        // remediation "belongs to the ok:true envelope" -- so the one field
        // naming the fix was present when the call succeeded with a blocked
        // leg and absent when the call actually failed. Same core function
        // the blocked leg and the CLI `= help:` block use.
        let r = doiget_core::remediation::for_denial(&dc);
        if !r.is_empty() {
            error_obj.insert("remediation".into(), json!(r));
        }
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
        // #458. Explicit, because the `_` arm at the bottom renders
        // `{"status": "unknown"}` -- an agent would be told nothing about
        // a PDF that exists, came from the publisher rather than an open
        // host, and carries the terms of a TDM agreement.
        PdfLegStatus::TdmFetched {
            source,
            original_block,
        } => json!({
            "status": "tdm_fetched",
            "source": source,
            "original_block": original_block,
        }),
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
            // #506: these five objects are assembled by hand rather than through
            // `error_object`, so the first pass at the disposition missed them --
            // including the two most common failures an agent sees. A field that is
            // present on some failures and absent on others is worse than none.
            o.insert("disposition".into(), json!(code.disposition().as_wire()));
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
                // #459: `denial_context` says what was refused. This says
                // what to do about it — the same suggestions the CLI's
                // `= help:` block prints, from the same core function.
                //
                // Without it an agent sees a refusal and 24 patterns that
                // are not the host, and reports "not available" for a
                // paper that one line of config would have fetched.
                let r = doiget_core::remediation::for_denial(dc);
                if !r.is_empty() {
                    o.insert("remediation".into(), json!(r));
                }
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
        // #344 identity confirmation: title / authors / year mirrored from the
        // stored metadata so an agent can verify the RIGHT paper in one call,
        // without a follow-up `doiget_info`.
        "title": outcome.title,
        "authors": outcome.authors,
        "year": outcome.year,
        // Issue #118: never a silent metadata-only success.
        "pdf": pdf_leg_json(&outcome.pdf_leg),
        // #459: the #438 resolution trace. "We asked and it had nothing"
        // and "we never asked" need different actions, and until now only
        // the CLI was told which had happened. `null` rather than `[]`
        // when there is no trace, so "unavailable" stays distinguishable
        // from "empty".
        "attempts": attempts_json(&outcome.attempts),
    })
}

/// Serialise the resolution trace, or `null` when there is none.
fn attempts_json(attempts: &[doiget_core::orchestrator::SourceAttempt]) -> Value {
    if attempts.is_empty() {
        Value::Null
    } else {
        doiget_core::orchestrator::attempts_to_value(attempts)
    }
}

/// Build the `{ok:false, ref, error:{code, message}}` envelope for
/// input-shape / context-init failures in `doiget_fetch_paper`.
fn fetch_paper_error_envelope(ref_str: Option<&str>, code: ErrorCode, message: &str) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("ok".into(), json!(false));
    if let Some(r) = ref_str {
        obj.insert("ref".into(), json!(r));
    }
    obj.insert("error".into(), error_object(code, message));
    Value::Object(obj)
}

/// Build the `{ok:false, error:{code, message, denial_context?}}`
/// envelope for orchestrator failures in `doiget_fetch_paper`.
fn fetch_paper_fetch_error_envelope(err: &FetchError, ref_str: &str) -> Value {
    // Delegate, do not re-implement. This was a hand-rolled copy of
    // `From<&FetchError> for ErrorCode` ending in `_ => InternalError`, so
    // every variant it had not enumerated -- including `NotFound`, which a
    // mistyped DOI produces on the shipped build -- was reported to the caller
    // as an internal error. The canonical mapping is exhaustive and is what
    // the rest of this file already calls.
    let code: ErrorCode = ErrorCode::from(err);
    let denial: Option<DenialContext> = err.into();
    let mut error_obj = serde_json::Map::new();
    error_obj.insert("code".into(), json!(code));
    error_obj.insert("message".into(), json!(err.to_string()));
    // #506: these five objects are assembled by hand rather than through
    // `error_object`, so the first pass at the disposition missed them --
    // including the two most common failures an agent sees. A field that is
    // present on some failures and absent on others is worse than none.
    error_obj.insert("disposition".into(), json!(code.disposition().as_wire()));
    if let Some(dc) = denial {
        error_obj.insert(
            "denial_context".into(),
            denial_context_to_value(&dc, "fetch_paper"),
        );
        // #506: `denial_context` says what was refused; this says what to do
        // about it. `docs/ERRORS.md` §3 used to state outright that
        // remediation "belongs to the ok:true envelope" -- so the one field
        // naming the fix was present when the call succeeded with a blocked
        // leg and absent when the call actually failed. Same core function
        // the blocked leg and the CLI `= help:` block use.
        let r = doiget_core::remediation::for_denial(&dc);
        if !r.is_empty() {
            error_obj.insert("remediation".into(), json!(r));
        }
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
                // #506: these five objects are assembled by hand rather than through
                // `error_object`, so the first pass at the disposition missed them --
                // including the two most common failures an agent sees. A field that is
                // present on some failures and absent on others is worse than none.
                error_obj.insert("disposition".into(), json!(code.disposition().as_wire()));
                if let Some(dc) = denial {
                    error_obj.insert(
                        "denial_context".into(),
                        denial_context_to_value(&dc, "batch_from_bibliography"),
                    );
                    // #506: `denial_context` says what was refused; this says what to do
                    // about it. `docs/ERRORS.md` §3 used to state outright that
                    // remediation "belongs to the ok:true envelope" -- so the one field
                    // naming the fix was present when the call succeeded with a blocked
                    // leg and absent when the call actually failed. Same core function
                    // the blocked leg and the CLI `= help:` block use.
                    let r = doiget_core::remediation::for_denial(&dc);
                    if !r.is_empty() {
                        error_obj.insert("remediation".into(), json!(r));
                    }
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
                // #506: these five objects are assembled by hand rather than through
                // `error_object`, so the first pass at the disposition missed them --
                // including the two most common failures an agent sees. A field that is
                // present on some failures and absent on others is worse than none.
                error_obj.insert("disposition".into(), json!(code.disposition().as_wire()));
                if let Some(dc) = denial {
                    error_obj.insert(
                        "denial_context".into(),
                        denial_context_to_value(&dc, "batch_fetch"),
                    );
                    // #506: `denial_context` says what was refused; this says what to do
                    // about it. `docs/ERRORS.md` §3 used to state outright that
                    // remediation "belongs to the ok:true envelope" -- so the one field
                    // naming the fix was present when the call succeeded with a blocked
                    // leg and absent when the call actually failed. Same core function
                    // the blocked leg and the CLI `= help:` block use.
                    let r = doiget_core::remediation::for_denial(&dc);
                    if !r.is_empty() {
                        error_obj.insert("remediation".into(), json!(r));
                    }
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
        "error": error_object(code, message),
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
    let session_id = ulid::Ulid::generate().to_string();
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
    let contact_email = doiget_core::orchestrator::contact_email_or_placeholder();
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
        // #454: the Tier-3 transport gate, mirroring the CLI builder.
        // Empty in a default build; the `CapabilityProfile` grant is
        // still what decides whether a TDM source is ever called.
        allowlists.extend(tier_3_allowlists());

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
                        // Issue #405: the Gold-OA counterpart. Separate flag
                        // because the trust argument is different — see
                        // `oa_registry_hosts`.
                        if cfg.trust_oa_registries {
                            hosts.extend(doiget_core::user_extension::oa_registry_hosts());
                        }
                        if !hosts.is_empty() {
                            tracing::info!(
                                count = hosts.len(),
                                trust_academic_repos = cfg.trust_academic_repos,
                                trust_oa_registries = cfg.trust_oa_registries,
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
    // Delegated to `doiget_core::user_extension::config_dir` (#504): this
    // was the second of three hand-maintained copies, and the CLI's
    // already disagreed with it about a blank `XDG_CONFIG_HOME`.
    Ok(doiget_core::user_extension::config_dir()?)
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
// generates `call_tool`, `list_tools`, and `get_tool`. `router =
// self.tool_router` points those at the per-instance field built in
// `Server::new` instead of the macro's default `Self::tool_router()`, so
// the feature-gated trimming done there is what the peer actually sees
// (issue #379). We provide `get_info` ourselves so the server identifies
// itself as `name = "doiget"`, advertises `protocolVersion =
// "2024-11-05"` (the version the smoke test asserts), and includes
// capability-aware `instructions`.
#[tool_handler(router = self.tool_router)]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerInfo {
        // Both `ServerInfo` and `Implementation` are `#[non_exhaustive]`
        // (since rmcp 1.6, still so in 3.x), so we go through the public
        // builders rather than struct-literal construction.
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new("doiget", VERSION))
            .with_instructions(format!(
                "doiget v{VERSION} \u{2014} Open Access paper fetcher (stdio MCP). \
                 Tier 1 sources are always-on; Tier 2/3 require build features and \
                 env-var grants. Call `doiget_capability_profile` for the runtime \
                 view; call `doiget_health` for an operational sanity check. \
                 RETRY CONTRACT: every failure carries `error.disposition`. \
                 `terminal` = the answer will not change, do not retry. \
                 `retry_after` = it may change on its own, retry with backoff. \
                 `needs_config` = it will not change by itself, but a named \
                 change makes it — surface that to the user instead of \
                 looping. Read that field, not the error code's name: \
                 NO_OA_AVAILABLE is `needs_config`, not something to wait out."
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
/// 2. `./papers` — `papers/` under the current working directory
///    (#344 / ADR-0036).
///
/// Returns `None` only when the current working directory can't be
/// determined or isn't valid UTF-8. Callers downgrade that to
/// `store_writable: false` rather than erroring the whole tool call.
///
/// # Why duplicate the CLI logic?
///
/// `doiget-mcp` cannot depend on `doiget-cli` — that would invert the
/// `doiget-cli -> doiget-mcp` wiring established by this PR and pull
/// `clap` etc. into the MCP crate. Lifting this helper into `doiget-core`
/// is a viable Phase-3 follow-up but is out of scope for this foundation.
/// Whether a `DOIGET_STORE_ROOT` value is usable as a path: non-empty and not
/// an unexpanded `${...}` placeholder. A Desktop-Extension config left blank
/// passes the literal `${user_config.store_root}`, which must never become a
/// filesystem path (it produced `os error 5` access-denied). See #369.
/// Advice to attach to a `doiget_paper_search` that matched nothing (#534).
///
/// OpenAlex free-text matching degrades sharply as a query lengthens: past
/// roughly eight terms it returns nothing at all rather than a partial match.
/// A human reading `0 results` shortens the query and tries again. An agent
/// reading `ok: true` with an empty array reads it as a fact about the world
/// and stops -- in the session that produced #534, eleven consecutive searches
/// returned zero for papers a three-to-five term query then found immediately,
/// and a known study was written off as unavailable.
///
/// Returns `None` for short queries: a zero-result two-term search really may
/// mean the work is not indexed, and a hint on every empty result would train
/// readers to skip it.
fn zero_result_hint(query: &str) -> Option<String> {
    // Where OpenAlex free-text matching starts failing outright. Not a hard
    // boundary -- a bound observed from the queries in #534, which is why the
    // wording says "roughly".
    const DEGRADES_PAST: usize = 8;

    let terms = query.split_whitespace().count();
    if terms <= DEGRADES_PAST {
        return None;
    }
    Some(format!(
        "This query has {terms} terms. OpenAlex free-text matching degrades sharply past roughly {DEGRADES_PAST} and returns nothing rather than a partial match, so zero results here is more likely to be about the query than about the literature. Retry with 3-5 distinctive terms - an author surname, a coined phrase, the distinguishing noun - before concluding the work is not indexed."
    ))
}

fn store_root_env_is_usable(value: &str) -> bool {
    let v = value.trim();
    !v.is_empty() && !v.contains("${")
}

fn resolve_store_root() -> Option<Utf8PathBuf> {
    if let Ok(s) = std::env::var("DOIGET_STORE_ROOT") {
        if store_root_env_is_usable(&s) {
            return Some(Utf8PathBuf::from(s.trim()));
        }
    }
    // `[store] root` from the same `config.toml` the network gate reads
    // (#441). Kept in step with the CLI resolver deliberately: a store root
    // that differs between `doiget fetch` and `doiget serve` would put an
    // agent's downloads somewhere the user's own commands cannot see.
    if let Some(root) = store_root_from_config() {
        return Some(root);
    }
    // Default: `papers/` under the current working directory (#344 / ADR-0036),
    // so an agent's fetches land where the work is; set DOIGET_STORE_ROOT for a
    // central library.
    let cwd = std::env::current_dir().ok()?;
    Utf8PathBuf::from_path_buf(cwd)
        .ok()
        .map(|d| d.join("papers"))
}

/// `[store] root` from the user's `config.toml`, if any (#441).
///
/// Warns on a malformed file rather than staying silent, for the reason the
/// #468 review established against the CLI twin: only ONE function in this
/// crate builds the HTTP client, so the "the parse error surfaces on the
/// network path" justification does not hold for `doiget_info`,
/// `doiget_search_local`, `doiget_list_recent` or `doiget_tag`.
///
/// The consequences are worse here than on the CLI, because an agent cannot
/// see a shell. `doiget_info` returns `metadata: null` for a paper the user
/// already has, which per this server's own tool description tells the agent
/// to fetch it again; and `doiget_tag` writes into the wrong store and
/// returns `ok: true`.
///
/// stdout stays clean either way (ADR-0001) — `tracing` is wired to stderr.
fn store_root_from_config() -> Option<Utf8PathBuf> {
    let path = config_dir_utf8().ok()?.join("doiget").join("config.toml");
    let cfg = match doiget_core::user_extension::load(&path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                path = %path,
                error = %e,
                "config.toml could not be read; [store] root ignored and the default store                  root used instead"
            );
            return None;
        }
    };
    let raw = cfg.store_root?;
    Some(doiget_core::user_extension::expand_store_root(&raw))
}

/// Best-effort writability probe for the resolved store root.
///
/// Walks up to the nearest **existing** ancestor of `path` and reports
/// whether it is a directory that is not marked read-only. Returns `false`
/// if no ancestor exists at all.
///
/// Issue #406: this used to answer the question by calling
/// `std::fs::create_dir_all(path)`, which made `doiget_health` — a tool
/// annotated `read_only_hint = true` — materialise `papers/` inside
/// whatever directory the server happened to be started from. For a daemon
/// that directory is indeterminate, and for an agent it is usually an
/// unrelated source repository. A probe must not be the thing that creates
/// the store.
///
/// The trade-off is deliberate: an existing, writable ancestor does not
/// prove the mkdir would succeed. That is why the field is documented as
/// best-effort — a wrong `true` costs a clear error on the first real
/// write, whereas the old approach cost a stray directory on every health
/// check.
fn probe_store_writable(path: &camino::Utf8Path) -> bool {
    let mut cur = Some(path);
    while let Some(p) = cur {
        // A relative root (`DOIGET_STORE_ROOT=papers`) walks up to `""`,
        // whose implicit base is the current directory — stat that rather
        // than reporting the root unwritable. `docs/CONFIG.md` §4 asks for
        // absolute paths, but answering "not writable" for a directory we
        // would happily create is a worse failure than being lenient.
        let probe = if p.as_str().is_empty() {
            camino::Utf8Path::new(".")
        } else {
            p
        };
        match std::fs::metadata(probe.as_std_path()) {
            Ok(md) => return md.is_dir() && !md.permissions().readonly(),
            // Not found (or not statable) — try the parent. `parent()`
            // yields `None` at the root, which ends the walk.
            Err(_) => cur = p.parent(),
        }
    }
    false
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
/// extra keys. `tdm_ieee` (#430) joins them on the same footing: a
/// fourth publisher must not silently change the meaning of the three
/// booleans §7 names. `metadata_sources` is the spec-canonical view of the
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
    if profile.tdm_ieee.is_some() {
        tier_3.push("tdm-ieee");
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
        // Additive per the §7 note that consumers must tolerate extra
        // keys; the four `tdm_*` booleans named in the contract are all
        // still present and unchanged (#430).
        "tdm_ieee": profile.tdm_ieee.is_some(),
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

    /// #534: a long query matching nothing is far more likely to be a
    /// query-length problem than an absent literature, and the agent reading
    /// the envelope is the one who cannot tell the difference.
    #[test]
    fn a_long_zero_result_query_is_told_why_it_may_be_zero() {
        let q = "lithium refractoriness after discontinuation kindling sensitization course of illness Post";
        let hint = zero_result_hint(q).expect("10 terms is past the threshold");
        assert!(hint.contains("10 terms"), "names the count: {hint}");
        assert!(
            hint.contains("3-5"),
            "says what to do instead, not only what went wrong: {hint}"
        );
    }

    /// A short query returning nothing may genuinely mean nothing is indexed.
    /// Hinting on every empty result would teach readers to skip the hint.
    #[test]
    fn a_short_zero_result_query_is_left_alone() {
        assert!(zero_result_hint("depersonalization derealization").is_none());
        assert!(zero_result_hint("").is_none());
    }

    /// The threshold counts terms, not characters: one very long term is still
    /// one term, and "shorten it" is not the advice to give.
    #[test]
    fn the_threshold_counts_terms_not_length() {
        assert!(zero_result_hint(&"a".repeat(400)).is_none());
        assert!(zero_result_hint("a b c d e f g h i").is_some());
    }

    /// #406: the store-writability probe MUST NOT create anything.
    /// `doiget_health` is annotated `read_only_hint = true`, and the old
    /// `create_dir_all` implementation made a health check materialise
    /// `papers/` in whatever directory the server was started from — for
    /// a daemon, an indeterminate one; for an agent, usually an unrelated
    /// source repository.
    #[test]
    fn probe_store_writable_creates_nothing() {
        let td = tempfile::TempDir::new().expect("tempdir");
        let base = camino::Utf8Path::from_path(td.path()).expect("tempdir is utf-8");
        let missing = base.join("papers");

        assert!(
            probe_store_writable(&missing),
            "an absent root under a writable parent must still probe writable"
        );
        assert!(
            !missing.exists(),
            "the probe must not create {missing} — see #406"
        );

        // Nested-absent: the walk has to climb more than one level.
        let deep = base.join("a").join("b").join("papers");
        assert!(
            probe_store_writable(&deep),
            "nested-absent must climb to {base}"
        );
        assert!(
            !base.join("a").exists(),
            "the probe must not create intermediates"
        );
    }

    /// An existing store root reports writable, and a path whose nearest
    /// existing ancestor is a FILE reports not-writable — a file cannot
    /// hold a store, so `true` there would be a lie the first write pays for.
    /// A relative `DOIGET_STORE_ROOT` walks up to `""`, whose implicit base
    /// is the cwd. Reporting it unwritable would be wrong — a write there
    /// succeeds — and the pre-#406 `create_dir_all` probe got this right.
    #[test]
    fn probe_store_writable_handles_a_relative_root() {
        // `papers` (no `./`) is the case that exhausts the walk.
        assert!(
            probe_store_writable(camino::Utf8Path::new("papers")),
            "a bare relative root resolves against the cwd, which is writable"
        );
        assert!(
            probe_store_writable(camino::Utf8Path::new("./papers")),
            "the dotted form must agree with the bare form"
        );
        assert!(
            !camino::Utf8Path::new("papers").exists(),
            "still creates nothing"
        );
    }

    #[test]
    fn probe_store_writable_distinguishes_dir_from_file() {
        let td = tempfile::TempDir::new().expect("tempdir");
        let base = camino::Utf8Path::from_path(td.path()).expect("tempdir is utf-8");

        let real = base.join("papers");
        std::fs::create_dir_all(real.as_std_path()).expect("mkdir");
        assert!(probe_store_writable(&real), "an existing dir is writable");

        let file = base.join("not-a-dir");
        std::fs::write(file.as_std_path(), b"x").expect("write");
        assert!(
            !probe_store_writable(&file.join("papers")),
            "a file ancestor must not report writable"
        );
    }

    /// #370: numeric tool params accept a JSON number OR a stringified
    /// number (`"10"`); absent / `null` / empty-string stay `None`.
    #[test]
    fn lenient_numbers_accept_string_or_number() {
        use serde_json::{from_value, json};

        let s: SearchLocalInput = from_value(json!({"query": "x", "limit": "10"})).unwrap();
        assert_eq!(s.limit, Some(10));
        let n: SearchLocalInput = from_value(json!({"query": "x", "limit": 10})).unwrap();
        assert_eq!(n.limit, Some(10));
        let a: SearchLocalInput = from_value(json!({"query": "x"})).unwrap();
        assert_eq!(a.limit, None);
        let z: SearchLocalInput = from_value(json!({"query": "x", "limit": null})).unwrap();
        assert_eq!(z.limit, None);
        let e: SearchLocalInput = from_value(json!({"query": "x", "limit": ""})).unwrap();
        assert_eq!(e.limit, None);
    }

    /// #370: a non-numeric or out-of-range string is still rejected — the
    /// deserializer is lenient about *form*, not *validity*.
    #[test]
    fn lenient_numbers_reject_non_numeric_string() {
        use serde_json::{from_value, json};
        assert!(from_value::<SearchLocalInput>(json!({"query": "x", "limit": "abc"})).is_err());
        // 300 overflows u8 (min_percentile) -> parse error.
        assert!(
            from_value::<PaperSearchInput>(json!({"query": "x", "min_percentile": "300"})).is_err()
        );
    }

    /// #370: every numeric input field across the tool surface (u32 / i32 /
    /// u64 / u8 / f64, `Option` + required) tolerates stringified numbers.
    #[test]
    fn lenient_numbers_cover_every_numeric_input() {
        use serde_json::{from_value, json};

        let g: ExpandCitationGraphInput =
            from_value(json!({"ref": "10.1/x", "depth": "2", "total": "50", "per_paper": "5"}))
                .unwrap();
        assert_eq!(
            (g.depth, g.total, g.per_paper),
            (Some(2), Some(50), Some(5))
        );

        let t: PaperTextInput =
            from_value(json!({"ref": "arxiv:2401.00001", "max_chars": "2000"})).unwrap();
        assert_eq!(t.max_chars, Some(2000));
        let x: PaperTexSourceInput =
            from_value(json!({"ref": "arxiv:2401.00001", "max_chars": "2000"})).unwrap();
        assert_eq!(x.max_chars, Some(2000));

        let p: PaperSearchInput = from_value(json!({
            "query": "x", "limit": "7", "from_year": "2010", "to_year": "2020",
            "min_citations": "100", "min_percentile": "90", "min_fwci": "1.5"
        }))
        .unwrap();
        assert_eq!(p.limit, Some(7));
        assert_eq!(p.from_year, Some(2010));
        assert_eq!(p.to_year, Some(2020));
        assert_eq!(p.min_citations, Some(100));
        assert_eq!(p.min_percentile, Some(90));
        assert_eq!(p.min_fwci, Some(1.5));

        let l: ListRecentInput = from_value(json!({"limit": "25"})).unwrap();
        assert_eq!(l.limit, Some(25));

        let r: ResolveCitationInput = from_value(json!({"query": "x", "limit": "3"})).unwrap();
        assert_eq!(r.limit, 3);
        let d: ResolveCitationInput = from_value(json!({"query": "x"})).unwrap();
        assert_eq!(d.limit, 5);
        let b: BatchResolveCitationsInput =
            from_value(json!({"queries": ["x"], "limit": "8"})).unwrap();
        assert_eq!(b.limit, 8);
    }

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
    fn store_root_env_usable_rejects_placeholder_and_empty() {
        // Real paths are usable.
        assert!(store_root_env_is_usable("/home/u/papers"));
        assert!(store_root_env_is_usable(r"C:\Users\u\papers"));
        // Empty / whitespace and an unexpanded placeholder are not (#369).
        assert!(!store_root_env_is_usable(""));
        assert!(!store_root_env_is_usable("   "));
        assert!(!store_root_env_is_usable("${user_config.store_root}"));
        assert!(!store_root_env_is_usable("${HOME}/papers"));
    }

    #[test]
    fn resolve_store_root_returns_some_on_normal_host() {
        // The default is `<cwd>/papers` (ADR-0036): either branch yields a
        // root on a normal host — `DOIGET_STORE_ROOT` when set, else the cwd
        // default, since current_dir() is always available. We deliberately
        // don't mutate env (the test process is shared with other tests).
        let got = resolve_store_root();
        assert!(
            got.is_some(),
            "resolve_store_root must resolve on a normal host"
        );
        // When DOIGET_STORE_ROOT is unset, the default MUST be `<cwd>/papers`
        // (not a `~/papers` home fallback) — a read-only env check (no
        // mutation) that catches a regression to the old default (review #352).
        if std::env::var_os("DOIGET_STORE_ROOT").is_none() {
            let expected =
                camino::Utf8PathBuf::from_path_buf(std::env::current_dir().expect("cwd available"))
                    .expect("cwd is valid UTF-8")
                    .join("papers");
            assert_eq!(got, Some(expected));
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

    /// #459: a blocked PDF leg must say what would lift it.
    ///
    /// Before this the envelope carried `denial_context` — the host and
    /// the 24 patterns that are not it — and nothing else. An agent that
    /// cannot find the one-line fix reports "this paper is not
    /// available", which for the DOI in ADR-0043 is false: it fetches
    /// after `trust_academic_repos = true`.
    #[test]
    fn a_blocked_leg_carries_the_remediation_that_would_lift_it() {
        use doiget_core::{DenialContext, DenialReason};

        let leg = PdfLegStatus::Blocked {
            code: ErrorCode::NetworkError,
            message: "redirect target strathprints.strath.ac.uk not in allowlist".to_string(),
            denial: Some(DenialContext {
                reason: DenialReason::RedirectNotInAllowlist,
                source: Some("oa-publisher".to_string()),
                attempted: Some("strathprints.strath.ac.uk".to_string()),
                expected: Some(vec!["*.arxiv.org".to_string()]),
                hop_index: None,
                cap: None,
                actual: None,
            }),
            suggested_arxiv_id: None,
        };
        let v = pdf_leg_json(&leg);
        let rem = v["remediation"]
            .as_array()
            .expect("a redirect denial must carry remediation");

        assert!(
            rem.iter().any(|x| x["value"] == "*.strath.ac.uk"),
            "the #443 registrable-domain widening must reach MCP too: {v}"
        );
        let flag = rem
            .iter()
            .find(|x| x["kind"] == "trust_flag")
            .expect("an *.ac.uk host must surface the curated flag");
        assert_eq!(flag["value"], "trust_academic_repos");
        assert!(
            flag["note"]
                .as_str()
                .unwrap_or_default()
                .contains("*.ac.uk"),
            "name the pattern that matched, or the flag reads as a guess: {flag}"
        );
    }

    /// The converse: a denial with no configuration channel must offer
    /// nothing. Suggesting a host to trust for an oversized body sends
    /// the caller after a fix that cannot work.
    #[test]
    fn a_denial_with_no_config_channel_carries_no_remediation() {
        use doiget_core::{DenialContext, DenialReason};

        let leg = PdfLegStatus::Blocked {
            code: ErrorCode::NetworkError,
            message: "body exceeded the size cap".to_string(),
            denial: Some(DenialContext {
                reason: DenialReason::SizeCapExceeded,
                source: Some("oa-publisher".to_string()),
                attempted: None,
                expected: None,
                hop_index: None,
                cap: Some(104_857_600),
                actual: Some(209_715_200),
            }),
            suggested_arxiv_id: None,
        };
        let v = pdf_leg_json(&leg);
        assert!(
            v.get("remediation").is_none(),
            "a size cap is not fixed by trusting a host: {v}"
        );
        assert!(
            v.get("denial_context").is_some(),
            "the denial context itself must still be there: {v}"
        );
    }

    /// #459: the trace, and the distinction it exists to make. Absent
    /// rather than `[]` when unavailable — #413 was filed because those
    /// two were the same observable.
    /// #471. `fetch_paper_success_envelope` is the only caller that inserts
    /// `"attempts"`, and `pdf_leg_json` the only one that inserts
    /// `"remediation"` -- and neither was called from any test. Every test
    /// naming those fields called the leaf helpers directly with
    /// hand-constructed values, so disconnecting either wiring point left
    /// the suite green while the trace and the remediation vanished from the
    /// envelope an agent reads.
    ///
    /// This drives the ENVELOPE BUILDER, so both insertions are exercised at
    /// the point where a real outcome meets the wire.
    #[test]
    fn the_success_envelope_carries_the_trace_and_the_remediation() {
        use doiget_core::orchestrator::{AttemptOutcome, FetchPaperOutcome, SourceAttempt};
        use doiget_core::{DenialContext, DenialReason, ErrorCode};

        let blocked = PdfLegStatus::Blocked {
            code: ErrorCode::NetworkError,
            message: "redirect target not in allowlist".to_string(),
            denial: Some(DenialContext {
                reason: DenialReason::RedirectNotInAllowlist,
                source: Some("oa-publisher".to_string()),
                attempted: Some("cdn.example.org".to_string()),
                expected: Some(vec!["good.example.org".to_string()]),
                hop_index: Some(1),
                cap: None,
                actual: None,
            }),
            suggested_arxiv_id: None,
        };
        let outcome = FetchPaperOutcome::for_test_synthetic_with_attempts(
            "doi__10_1234_foo",
            "oa-publisher",
            blocked,
            vec![
                SourceAttempt::new("crossref", AttemptOutcome::Resolved),
                SourceAttempt::new(
                    "hal",
                    AttemptOutcome::Disabled {
                        env: &["DOIGET_ENABLE_HAL"],
                    },
                ),
            ],
        );

        let env = fetch_paper_success_envelope(&outcome, "doi:10.1234/foo");

        // The trace reached the envelope.
        let rows = env["attempts"]
            .as_array()
            .unwrap_or_else(|| panic!("attempts must be on the envelope; got {env}"));
        assert_eq!(rows.len(), 2, "{env}");
        assert_eq!(rows[0]["source"], json!("crossref"));
        assert_eq!(
            rows[1]["required_env"],
            json!(["DOIGET_ENABLE_HAL"]),
            "the #470 structured form travels with it; got {env}"
        );

        // And so did the remediation, through `pdf_leg_json`.
        let rem = env["pdf"]["remediation"]
            .as_array()
            .unwrap_or_else(|| panic!("a redirect denial has a config channel; got {env}"));
        assert!(!rem.is_empty(), "{env}");
        assert_eq!(env["pdf"]["status"], json!("blocked"), "{env}");
    }

    #[test]
    fn attempts_json_distinguishes_no_trace_from_an_empty_one() {
        use doiget_core::orchestrator::{AttemptOutcome, SourceAttempt};

        assert!(
            attempts_json(&[]).is_null(),
            "no trace must be null, not an empty array"
        );

        let a = attempts_json(&[
            SourceAttempt::new(
                "hal",
                AttemptOutcome::Disabled {
                    env: &["DOIGET_ENABLE_HAL"],
                },
            ),
            SourceAttempt::new("core", AttemptOutcome::NoRecord),
        ]);
        let rows = a.as_array().expect("array");
        assert_eq!(rows[0]["outcome"], "not_consulted_disabled");
        assert_eq!(
            rows[0]["detail"], "DOIGET_ENABLE_HAL",
            "name the var to set, or the agent cannot act on it"
        );
        assert_eq!(rows[0]["consulted"], false);
        assert_eq!(rows[1]["outcome"], "consulted_no_record");
        assert_eq!(
            rows[1]["consulted"], true,
            "asked-and-empty is not the same as never-asked"
        );
    }

    /// #454: the MCP half of the Tier-3 transport gate.
    ///
    /// `build_http_client_for_fetch` is a hand-maintained twin of the CLI
    /// builder, and the two drifting is what left the Tier-3 allowlists
    /// with no caller in either. A guard in only one crate would let them
    /// drift again in the other.
    #[test]
    #[serial_test::serial]
    #[cfg(any(
        feature = "tdm-aps",
        feature = "tdm-elsevier",
        feature = "tdm-springer",
        feature = "tdm-ieee"
    ))]
    #[allow(clippy::vec_init_then_push)]
    fn the_production_client_registers_every_tier_3_source_key() {
        // Any base override takes the test-mode branch, which registers
        // whatever it is handed and would prove nothing.
        let _guards = [
            EnvGuard::unset("DOIGET_ARXIV_BASE"),
            EnvGuard::unset("DOIGET_ARXIV_SRC_BASE"),
            EnvGuard::unset("DOIGET_CROSSREF_BASE"),
            EnvGuard::unset("DOIGET_UNPAYWALL_BASE"),
            EnvGuard::unset("DOIGET_OA_PUBLISHER_BASE"),
            EnvGuard::unset("DOIGET_OPENALEX_BASE"),
            EnvGuard::unset("DOIGET_AR5IV_BASE"),
        ];

        let client = build_http_client_for_fetch().expect("production client builds");

        // Built by push rather than an array literal: with a single
        // `tdm-*` feature compiled the literal is a one-element loop,
        // which clippy denies. Same shape as `tier_3_allowlists()`,
        // and the `vec_init_then_push` allow is on the fn for the same
        // reason it is there — the pushes are `#[cfg]`-gated, so an
        // attribute per element is not expressible.
        let mut keys: Vec<&str> = Vec::new();
        #[cfg(feature = "tdm-aps")]
        keys.push("tdm-aps");
        #[cfg(feature = "tdm-elsevier")]
        keys.push("tdm-elsevier");
        #[cfg(feature = "tdm-springer")]
        keys.push("tdm-springer");
        #[cfg(feature = "tdm-ieee")]
        keys.push("tdm-ieee");
        assert!(!keys.is_empty(), "the guard must have checked something");
        for key in keys {
            assert!(
                client.source_allowlist(key).is_some(),
                "the production MCP client has no allowlist for `{key}`; a TDM fetch \
                 would die at UnknownSource (#454)"
            );
        }
    }

    /// #516: the MCP half of the Tier-2 transport gate.
    ///
    /// The CLI builder had this extend behind `#[cfg(feature =
    /// "citation")]` while the sources it serves are gated on
    /// `metadata`, so a `metadata`-only build reached them and died at
    /// `UnknownSource`. `build_http_client_for_fetch` registers
    /// `tier_2_allowlist()` unconditionally and is therefore correct
    /// today — this pins that, because "correct today in one of two
    /// hand-maintained twins" is exactly the state #454 was filed from.
    #[test]
    #[serial_test::serial]
    fn the_production_client_registers_every_tier_2_source_key() {
        // Any base override takes the test-mode branch, which registers
        // whatever it is handed and would prove nothing.
        let _guards = [
            EnvGuard::unset("DOIGET_ARXIV_BASE"),
            EnvGuard::unset("DOIGET_ARXIV_SRC_BASE"),
            EnvGuard::unset("DOIGET_CROSSREF_BASE"),
            EnvGuard::unset("DOIGET_UNPAYWALL_BASE"),
            EnvGuard::unset("DOIGET_OA_PUBLISHER_BASE"),
            EnvGuard::unset("DOIGET_OPENALEX_BASE"),
            EnvGuard::unset("DOIGET_AR5IV_BASE"),
        ];

        let client = build_http_client_for_fetch().expect("production client builds");

        let keys: Vec<String> = tier_2_allowlist()
            .iter()
            .map(|a| a.source.clone())
            .collect();
        assert!(!keys.is_empty(), "the guard must have checked something");
        for key in keys {
            assert!(
                client.source_allowlist(&key).is_some(),
                "the production MCP client has no allowlist for `{key}`; the \n                 optional chain reaches this source and the fetch would \n                 die at UnknownSource (#516)"
            );
        }
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
    /// #468 review: the CLI has three tests pinning the `[store] root`
    /// resolution ORDER; this crate had none, even though it carries a
    /// hand-duplicated copy of the resolver rather than calling the CLI's.
    ///
    /// That is the #454 shape — two independent copies of the same logic,
    /// one of them unguarded — repeated one PR later, and it matters more
    /// here: `doiget_tag` WRITES metadata into whatever root this returns,
    /// and reports `ok: true` either way.
    ///
    /// The load-bearing assertion is the negative one. Comparing only
    /// against the configured path would also pass if the config were
    /// ignored and the test happened to run from that directory.
    #[test]
    #[serial_test::serial]
    fn store_root_in_config_beats_the_cwd_default() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let cfg_root = camino::Utf8Path::from_path(tmp.path()).expect("utf8 tempdir");
        let doiget_dir = cfg_root.join("doiget");
        std::fs::create_dir_all(doiget_dir.as_std_path()).expect("mk dir");

        let lib = tempfile::TempDir::new().expect("tempdir");
        let library = camino::Utf8Path::from_path(lib.path())
            .expect("utf8 tempdir")
            .as_str()
            .replace('\u{5c}', "/");
        std::fs::write(
            doiget_dir.join("config.toml").as_std_path(),
            format!("[store]\nroot = \"{library}\"\n"),
        )
        .expect("write config.toml");

        let _guards = scoped_env_for_user_extension(cfg_root.as_str());
        let _no_env_root = EnvGuard::unset("DOIGET_STORE_ROOT");

        let got = resolve_store_root().expect("resolves on a normal host");

        let cwd_default = camino::Utf8PathBuf::try_from(std::env::current_dir().expect("cwd"))
            .expect("utf8 cwd")
            .join("papers");
        assert_ne!(
            got, cwd_default,
            "the config value was ignored and the cwd default answered instead"
        );
        assert_eq!(
            got.as_str().replace('\u{5c}', "/"),
            library,
            "[store] root must win over the cwd default, as it does on the CLI"
        );
    }

    /// The rung above it still wins, so adding the config rung did not
    /// demote the env var.
    #[test]
    #[serial_test::serial]
    fn env_beats_store_root_in_config_on_the_mcp_surface() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let cfg_root = camino::Utf8Path::from_path(tmp.path()).expect("utf8 tempdir");
        let doiget_dir = cfg_root.join("doiget");
        std::fs::create_dir_all(doiget_dir.as_std_path()).expect("mk dir");
        std::fs::write(
            doiget_dir.join("config.toml").as_std_path(),
            "[store]\nroot = \"/from/config\"\n",
        )
        .expect("write config.toml");

        let _guards = scoped_env_for_user_extension(cfg_root.as_str());
        let _env_root = EnvGuard::set("DOIGET_STORE_ROOT", "/from/env");

        let got = resolve_store_root().expect("resolves");
        assert_eq!(got.as_str(), "/from/env");
    }
}
