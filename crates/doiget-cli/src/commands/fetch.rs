//! `doiget fetch <ref>` subcommand.
//!
//! Phase 1 scope:
//!
//! - **arXiv refs** — full end-to-end: PDF bytes are fetched via the
//!   `doiget_core::sources::arxiv::ArxivSource`, the `[doiget]`
//!   extension table is populated with the resolved license, source,
//!   size, and `fetched_at`, and the result is written to the on-disk
//!   store with both the metadata TOML and the PDF.
//! - **DOI refs** — Crossref metadata + Unpaywall license enrichment + an
//!   OA PDF fetch when Unpaywall's `best_oa_location.url_for_pdf` (or
//!   `best_oa_location.url`) resolves to a host on the synthetic
//!   `"oa-publisher"` allowlist (`docs/REDIRECT_ALLOWLIST.md` §3). The OA
//!   URL host check is informed-best-effort; if the host is not on the
//!   allowlist or the body fails the magic-byte check, the orchestrator
//!   logs a `Fetch err` row under `source = "oa-publisher"` and falls back
//!   to metadata-only success — the metadata is still useful.
//!
//! ## Provenance contract
//!
//! Per `docs/PROVENANCE_LOG.md` §3, every invocation emits at least one
//! `SessionStart`, one or more `Fetch` rows (one per source consulted), one
//! `StoreWrite` row on success, and one `SessionEnd`. Each `Fetch` row is
//! appended by the underlying `Source` impl; the orchestrator owns the
//! session-bookend rows and the `StoreWrite` row.
//!
//! ## Configuration surface
//!
//! Hard-coded paths with env-var overrides; full `config.toml` plumbing
//! arrives in a follow-up. See `docs/CONFIG.md` for the eventual surface.
//!
//! | Env var | Default | Purpose |
//! |---|---|---|
//! | `DOIGET_STORE_ROOT` | `$HOME/papers` (or `%USERPROFILE%\papers` on Windows) | Filesystem store root |
//! | `DOIGET_LOG_PATH` | `<config>/doiget/access.jsonl` | Provenance log file |
//! | `DOIGET_CONTACT_EMAIL` | `doiget@localhost` | Polite-pool contact email (User-Agent and Crossref) |
//! | `DOIGET_UNPAYWALL_EMAIL` | (= contact email) | Unpaywall query-string email |
//! | `DOIGET_ARXIV_BASE` | `https://arxiv.org` | arXiv source base (test override) |
//! | `DOIGET_CROSSREF_BASE` | `https://api.crossref.org` | Crossref source base (test override) |
//! | `DOIGET_UNPAYWALL_BASE` | `https://api.unpaywall.org/v2` | Unpaywall source base (test override) |
//! | `DOIGET_OA_PUBLISHER_BASE` | (production allowlist) | OA publisher host allowlist override (test override) |

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use camino::Utf8PathBuf;

#[cfg(feature = "citation")]
use doiget_core::http::tier_2_allowlist;
use doiget_core::http::{oa_publisher_allowlist, tier_1_allowlist, HttpClient};
use doiget_core::orchestrator::{fetch_paper as core_fetch_paper, FetchPaperOutcome, PdfLegStatus};
use doiget_core::provenance::{Capability, LogEvent, LogResult, ProvenanceLog, RowInput};
use doiget_core::rate_limiter::RateLimiter;
use doiget_core::source::{FetchContext, FetchError};
use doiget_core::store::FsStore;
use doiget_core::{CapabilityProfile, DenialContext, ErrorCode, RateLimits, Ref};

/// Defer to docs/PROVENANCE_LOG.md §3: 26-char ULID per process invocation.
fn new_session_id() -> String {
    ulid::Ulid::new().to_string()
}

// ---------------------------------------------------------------------------
// Dry-run plan / preview (ADR-0022)
// ---------------------------------------------------------------------------

// The structured `FetchPlan` shape, the `build_fetch_plan` builder, and
// the `build_dry_run_envelope` JSON-shape helper live in `doiget-core`
// so the MCP server can produce a bit-identical envelope without
// depending on `doiget-cli`. The CLI re-exports them here for callers
// that already `use doiget_cli::commands::fetch`.
pub use doiget_core::dry_run::{
    build_dry_run_envelope, build_fetch_plan, FetchPlan, PdfSourcePlan, RateLimitBudget,
};

/// Serialize the dry-run envelope and write it to stdout. Used by the
/// `--dry-run` flag on `doiget fetch` and `doiget batch`. The envelope
/// shape matches ADR-0022 §1 / `docs/MCP_TOOLS.md` §10.
///
/// `pub` so `commands::batch` (multi-ref dry-run) can reuse it. The
/// function lives in `doiget-cli` (not `doiget-core`) because `println!`
/// is a CLI concern; the MCP server uses [`build_dry_run_envelope`]
/// directly and routes the bytes via JSON-RPC.
///
/// `print_stdout` is workspace-deny for MCP stdio safety (ADR-0001 /
/// `docs/SECURITY.md` §3); `--dry-run` is a CLI-only path that never
/// runs under the MCP server, so the localized `#[allow]` is the
/// minimal intervention — same pattern used by `commands::config`,
/// `commands::info`, etc.
#[allow(clippy::print_stdout)]
pub fn emit_dry_run_plan_to_stdout(ref_: &Ref, plan: &FetchPlan) -> Result<()> {
    let envelope = build_dry_run_envelope(ref_, plan);
    let s = serde_json::to_string(&envelope).context("serializing dry-run envelope to JSON")?;
    println!("{s}");
    Ok(())
}

/// Resolve the provenance log path. `DOIGET_LOG_PATH` wins; otherwise
/// fall back to `<config>/doiget/access.jsonl` per `docs/PROVENANCE_LOG.md`
/// §1.
fn resolve_log_path() -> Result<Utf8PathBuf> {
    if let Some(s) = read_env_utf8("DOIGET_LOG_PATH")? {
        return Ok(Utf8PathBuf::from(s));
    }
    let cfg = config_dir_utf8()?;
    Ok(cfg.join("doiget").join("access.jsonl"))
}

/// Read an env var and assert it is valid UTF-8. Returns `Ok(None)` if
/// unset; `Ok(Some(s))` if set and UTF-8; `Err(...)` if set but non-UTF-8.
/// `std::env::var` already requires UTF-8 (returns `VarError::NotUnicode`
/// otherwise); we wrap it to surface a friendlier error and avoid the
/// banned `std::path::PathBuf` round-trip.
fn read_env_utf8(key: &str) -> Result<Option<String>> {
    match std::env::var(key) {
        Ok(s) => Ok(Some(s)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(anyhow!("{key} is not valid UTF-8")),
    }
}

/// Best-effort home-dir resolution without depending on the `dirs` crate
/// (every new dep adds cargo-vet exemption churn). Honors `HOME` first
/// (POSIX + most CI), then `USERPROFILE` (Windows).
fn home_dir_utf8() -> Result<Utf8PathBuf> {
    if let Some(s) = read_env_utf8("HOME")? {
        return Ok(Utf8PathBuf::from(s));
    }
    if let Some(s) = read_env_utf8("USERPROFILE")? {
        return Ok(Utf8PathBuf::from(s));
    }
    Err(anyhow!("neither HOME nor USERPROFILE is set"))
}

/// Best-effort config-dir resolution. Honors `XDG_CONFIG_HOME` first
/// (POSIX), then `APPDATA` (Windows), then falls back to `$HOME/.config`.
fn config_dir_utf8() -> Result<Utf8PathBuf> {
    if let Some(s) = read_env_utf8("XDG_CONFIG_HOME")? {
        return Ok(Utf8PathBuf::from(s));
    }
    if let Some(s) = read_env_utf8("APPDATA")? {
        return Ok(Utf8PathBuf::from(s));
    }
    let home = home_dir_utf8()?;
    Ok(home.join(".config"))
}

/// Construct the workspace-wide [`HttpClient`].
///
/// Production path: `HttpClient::new(tier_1_allowlist() ∪ oa_publisher_allowlist())` —
/// strict HTTPS-only with the canonical Tier-1 redirect allowlist (Crossref,
/// Unpaywall, arXiv) plus the synthetic `"oa-publisher"` allowlist used for
/// the OA PDF leg of the DOI fetch path (`fetch_doi` issues
/// `HttpClient::fetch_pdf("oa-publisher", url)` against the URL Unpaywall
/// returned in `best_oa_location`). The OA-publisher list is
/// informed-best-effort per `docs/REDIRECT_ALLOWLIST.md` §3.
///
/// Test path: when any of the three `DOIGET_*_BASE` env vars is set, build a
/// multi-source relaxed-`https_only` client whose per-source allowlist is
/// derived from the corresponding env-var hosts. The `oa-publisher` source
/// key is registered against the same host (typically the wiremock origin)
/// when `DOIGET_OA_PUBLISHER_BASE` is set — this lets the integration tests
/// under `tests/fetch_doi_oa_pdf_e2e.rs` exercise the full PDF leg without
/// touching the real network.
fn build_http_client() -> Result<HttpClient> {
    let arxiv = std::env::var("DOIGET_ARXIV_BASE").ok();
    let crossref = std::env::var("DOIGET_CROSSREF_BASE").ok();
    let unpaywall = std::env::var("DOIGET_UNPAYWALL_BASE").ok();
    let oa_publisher = std::env::var("DOIGET_OA_PUBLISHER_BASE").ok();
    // Slice 16: `DOIGET_OPENALEX_BASE` selects a wiremock host for the
    // citation-graph BFS. Only meaningful with `--features citation`,
    // but reading the env unconditionally keeps the branch logic
    // simple and is harmless for default builds.
    let openalex_base = std::env::var("DOIGET_OPENALEX_BASE").ok();

    if arxiv.is_none()
        && crossref.is_none()
        && unpaywall.is_none()
        && oa_publisher.is_none()
        && openalex_base.is_none()
    {
        let mut allowlists = tier_1_allowlist();
        allowlists.extend(oa_publisher_allowlist());
        // Slice 16: when the `citation` feature is compiled in, the
        // graph subcommand walks OpenAlex Work IDs via
        // `ctx.http.fetch_bytes("openalex", ...)`. The Tier 2
        // allowlist registers the `api.openalex.org` host under
        // that source key. CapabilityProfile.metadata.openalex is
        // the runtime gate; the allowlist is the transport gate.
        #[cfg(feature = "citation")]
        allowlists.extend(tier_2_allowlist());
        return HttpClient::new(allowlists).context("building HTTP client");
    }

    // Test-base mode: build a relaxed client per overridden source.
    let mut owned: Vec<(String, String)> = Vec::new();
    for (source, base) in [
        ("arxiv", arxiv.as_deref()),
        ("crossref", crossref.as_deref()),
        ("unpaywall", unpaywall.as_deref()),
        ("oa-publisher", oa_publisher.as_deref()),
        ("openalex", openalex_base.as_deref()),
    ] {
        if let Some(b) = base {
            let url = url::Url::parse(b)
                .with_context(|| format!("DOIGET_*_BASE for {source} is not a URL: {b}"))?;
            let host = url
                .host_str()
                .ok_or_else(|| anyhow!("base URL has no host: {b}"))?;
            owned.push((source.to_string(), host.to_string()));
        }
    }
    let entries: Vec<(&str, &str)> = owned
        .iter()
        .map(|(s, h)| (s.as_str(), h.as_str()))
        .collect();
    Ok(HttpClient::new_for_tests_allow_http_multi(&entries))
}

// Slice 2: the per-source env-aware constructors that used to live here
// (`build_arxiv_source`, `build_crossref_source`, `build_unpaywall_source`)
// moved into `doiget-core::orchestrator` so the core `fetch_paper`
// orchestrator and the MCP server both honor the same `DOIGET_*_BASE`
// test-override surface. The CLI no longer constructs sources directly —
// it builds the `FetchContext` + `FsStore` and hands them to the core
// orchestrator.

/// Resolved configuration derived from the environment.
///
/// Slice 2: `contact_email` / `unpaywall_email` are now read by the
/// `doiget-core::orchestrator::fetch_paper` orchestrator directly from
/// the env (`contact_email_from_env` / `unpaywall_email_from_env` in
/// that module), so the CLI no longer threads them through. The fields
/// stay here so a future slice that adds CLI-flag overrides has a
/// natural attachment point — the `#[allow(dead_code)]` is the minimal
/// intervention until that slice lands.
#[allow(dead_code)]
pub(crate) struct OrchestratorConfig {
    pub(crate) store_root: Utf8PathBuf,
    pub(crate) log_path: Utf8PathBuf,
    pub(crate) contact_email: String,
    pub(crate) unpaywall_email: String,
}

impl OrchestratorConfig {
    fn from_env() -> Result<Self> {
        let store_root = super::resolve_store_root()?;
        let log_path = resolve_log_path()?;
        let contact_email =
            std::env::var("DOIGET_CONTACT_EMAIL").unwrap_or_else(|_| "doiget@localhost".into());
        let unpaywall_email =
            std::env::var("DOIGET_UNPAYWALL_EMAIL").unwrap_or_else(|_| contact_email.clone());
        Ok(Self {
            store_root,
            log_path,
            contact_email,
            unpaywall_email,
        })
    }
}

/// Reusable fetch harness shared by `doiget fetch <ref>` (single ref) and
/// `doiget batch <path>` (many refs). Owns the shared foundation modules
/// (`HttpClient` / `RateLimiter` / `ProvenanceLog`), the on-disk store, and
/// the resolved capability profile, plus the session bookkeeping required by
/// `docs/PROVENANCE_LOG.md` §3 (the 26-char ULID `session_id`).
///
/// Construction is performed once via [`FetchHarness::from_env`]. Per-ref
/// orchestration runs through [`FetchHarness::fetch_one`]; bookend rows go
/// via [`FetchHarness::log_session_start`] / [`FetchHarness::log_session_end`]
/// so the orchestrator can frame either one fetch or many.
pub(crate) struct FetchHarness {
    pub(crate) http: Arc<HttpClient>,
    pub(crate) rate_limiter: Arc<RateLimiter>,
    pub(crate) log: Arc<ProvenanceLog>,
    pub(crate) store: FsStore,
    pub(crate) profile: CapabilityProfile,
    pub(crate) session_id: String,
    /// Resolved config; Slice 2 keeps this on the harness for the
    /// CLI-only env diagnostics path (`commands::config::doctor`), even
    /// though `fetch_one` no longer needs it (the core orchestrator
    /// re-reads contact email from env directly).
    #[allow(dead_code)]
    pub(crate) cfg: OrchestratorConfig,
}

impl FetchHarness {
    /// Build a harness from the same env-var surface documented at the top
    /// of this module. Creates the log parent directory if missing, opens
    /// the provenance log (allocating a fresh `session_id`), and constructs
    /// the HTTP client honoring `DOIGET_*_BASE` overrides for tests.
    pub(crate) fn from_env() -> Result<Self> {
        let cfg = OrchestratorConfig::from_env()?;
        if let Some(parent) = cfg.log_path.parent() {
            if !parent.as_str().is_empty() {
                std::fs::create_dir_all(parent.as_std_path())
                    .with_context(|| format!("creating log dir {parent}"))?;
            }
        }
        let session_id = new_session_id();
        let log = Arc::new(
            ProvenanceLog::open(cfg.log_path.clone(), session_id.clone())
                .context("opening provenance log")?,
        );
        let http = Arc::new(build_http_client()?);
        let rate_limiter = Arc::new(RateLimiter::new(RateLimits::HARD_CODED));
        let store = FsStore::new(cfg.store_root.clone()).context("opening store")?;
        let profile = CapabilityProfile::from_env().context("resolving capability profile")?;

        Ok(Self {
            http,
            rate_limiter,
            log,
            store,
            profile,
            session_id,
            cfg,
        })
    }

    /// Build a [`FetchContext`] view over this harness's foundation modules.
    /// Creating one is cheap (cloning three `Arc`s + a `String`); per-ref
    /// orchestration constructs one on demand.
    pub(crate) fn fetch_context(&self) -> FetchContext {
        FetchContext {
            http: self.http.clone(),
            rate_limiter: self.rate_limiter.clone(),
            log: self.log.clone(),
            session_id: self.session_id.clone(),
        }
    }

    /// Append a `SessionStart` row. `ref_input` is the raw user-supplied ref
    /// string (single-fetch path); pass `None` for batch sessions where no
    /// single ref attributes the session.
    pub(crate) fn log_session_start(&self, ref_input: Option<&str>) -> Result<()> {
        self.log
            .append(RowInput {
                event: LogEvent::SessionStart,
                result: LogResult::Ok,
                capability: Capability::Oa,
                ref_: ref_input,
                source: None,
                error_code: None,
                size_bytes: None,
                license: None,
                store_path: None,
                // Session bookend — no audit identity (ADR-0021 §1).
                canonical_digest: None,
            })
            .context("appending SessionStart row")?;
        Ok(())
    }

    /// Append a `SessionEnd` row. `ref_input` mirrors the `log_session_start`
    /// argument; pass `None` for batch sessions. The result is best-effort —
    /// if this append fails, the caller already has the underlying fetch
    /// error (if any) and we don't override it.
    pub(crate) fn log_session_end(&self, ok: bool, ref_input: Option<&str>) {
        let result = if ok { LogResult::Ok } else { LogResult::Err };
        let _ = self.log.append(RowInput {
            event: LogEvent::SessionEnd,
            result,
            capability: Capability::Oa,
            ref_: ref_input,
            source: None,
            error_code: None,
            size_bytes: None,
            license: None,
            store_path: None,
            // Session bookend — no audit identity (ADR-0021 §1).
            canonical_digest: None,
        });
    }

    /// Run a single ref through the per-kind orchestration (arxiv → PDF +
    /// metadata; doi → metadata-only via Crossref + Unpaywall, with an
    /// informed-best-effort OA PDF leg). Errors here are scoped to this
    /// one ref — the caller decides whether to abort the surrounding
    /// session.
    ///
    /// Slice 2: delegates to
    /// [`doiget_core::orchestrator::fetch_paper`] for the actual work
    /// (which both CLI and MCP now share). This function keeps the
    /// CLI-only stderr success-line print.
    pub(crate) async fn fetch_one(&self, ref_: &Ref) -> Result<()> {
        let ctx = self.fetch_context();
        match core_fetch_paper(ref_, &self.profile, &ctx, &self.store, self.store.root()).await {
            Ok(outcome) => {
                // Issue #145 / `docs/ERRORS.md` §3 + §6: a `Blocked` PDF
                // leg means an OA PDF *was* discovered but could not be
                // retrieved. Metadata was written, but this is NOT a
                // clean success — emitting only `print_success` here made
                // a denied PDF visually indistinguishable from a genuine
                // metadata-only result and exited 0 (a silent failure).
                // Route it through the SAME `error[CODE]:` stderr channel
                // + `cli_exit_code(...)` mapping `fetch` already uses for
                // `CapabilityDenied`, using the structured code the
                // orchestrator mapped from the underlying transport error.
                if let PdfLegStatus::Blocked {
                    code,
                    message,
                    denial,
                } = &outcome.pdf_leg
                {
                    render_blocked_error(ref_, &outcome, *code, message, denial.as_ref());
                    return Err(anyhow::Error::new(CliExit(cli_exit_code(*code))));
                }
                emit_success_line(ref_, &outcome);
                Ok(())
            }
            Err(e) => {
                // Issue #119: render the cargo-style `error[CODE]:`
                // line + denial note HERE (while the error is still
                // typed), then carry only the exit code to `main`.
                render_fetch_error(&e);
                let code: ErrorCode = (&e).into();
                Err(anyhow::Error::new(CliExit(cli_exit_code(code))))
            }
        }
    }
}

/// CLI-only one-line success message on stderr (ADR-0001 stdio
/// convention). Renders the [`FetchPaperOutcome`] in the same form the
/// pre-Slice-2 CLI emitted: a full-PDF success names the PDF path; a
/// metadata-only DOI fallback (size_bytes == 0) names the metadata TOML
/// path the orchestrator wrote.
fn emit_success_line(ref_: &Ref, outcome: &FetchPaperOutcome) {
    let label = match ref_ {
        Ref::Arxiv(id) => format!("arxiv:{}", id.as_str()),
        Ref::Doi(doi) => format!("doi:{}", doi.as_str()),
    };
    match &outcome.pdf_leg {
        PdfLegStatus::Fetched => {
            print_success(format_args!(
                "fetched {} ({} bytes) -> {}",
                label, outcome.size_bytes, outcome.path
            ));
        }
        PdfLegStatus::NoOaUrl => {
            print_success(format_args!(
                "fetched {} (metadata-only: no OA PDF available) -> {}",
                label, outcome.path
            ));
        }
        // Issue #145: `Blocked` is NO LONGER a success outcome. It is
        // intercepted in `fetch_one` BEFORE `emit_success_line` is
        // called and rendered via `render_blocked_error` with a
        // non-zero exit (`docs/ERRORS.md` §3/§6 — no silent failures).
        // Reaching this arm would mean the interception regressed, so we
        // fail closed: surface the `error[CODE]:` line here too rather
        // than printing a misleading success line.
        PdfLegStatus::Blocked {
            code,
            message,
            denial,
        } => {
            render_blocked_error(ref_, outcome, *code, message, denial.as_ref());
        }
        // `PdfLegStatus` is `#[non_exhaustive]`; a future variant
        // degrades to the size-based wording rather than failing the
        // downstream-crate build.
        _ => {
            if outcome.size_bytes == 0 {
                print_success(format_args!(
                    "fetched {} (metadata-only) -> {}",
                    label, outcome.path
                ));
            } else {
                print_success(format_args!(
                    "fetched {} ({} bytes) -> {}",
                    label, outcome.size_bytes, outcome.path
                ));
            }
        }
    }
}

/// Run the `doiget fetch <ref>` subcommand.
///
/// `dry_run` (ADR-0022 §1): when `true`, build a [`FetchPlan`] from the
/// parsed [`Ref`] and the configured store root, serialize it as JSON to
/// stdout, and return `Ok(())` immediately, **without** building a
/// `FetchHarness` (no provenance log open), without contacting the
/// network, without writing to the store, and without appending a
/// provenance row.
///
/// When `dry_run` is `false`, the function runs the normal end-to-end
/// orchestration path: open the provenance log, dispatch the per-kind
/// orchestrator, emit a `SessionStart` / `SessionEnd` bookend pair.
///
/// On success returns `Ok(())` and writes a one-line success message to
/// stderr (per ADR-0001 stdio convention — no stdout writes from `fetch`
/// on the normal path). On failure, returns an `anyhow::Error` and emits
/// a `SessionEnd` row with `result=err` to the provenance log before
/// returning.
///
/// # History
///
/// Slice 5 (PR #84 advisory item A2/A3 refactor): the previous
/// `FetchOptions { dry_run: bool }` single-field option bundle plus the
/// thin `run(input)` backwards-compat wrapper were collapsed into this
/// single `dry_run: bool` parameter — the option bundle's single-bool
/// shape was YAGNI, and the wrapper only existed to spare integration
/// tests a `FetchOptions::default()` literal.
pub async fn run_with_options(input: String, dry_run: bool) -> Result<()> {
    // Step 1: parse + safekey. Issue #119: render the cargo-style
    // `error[INVALID_REF]:` line + carry the exit code, rather than
    // letting the granular `RefParseError` fall out as an opaque
    // anyhow `{:?}` dump.
    let ref_ = match Ref::parse(&input) {
        Ok(r) => r,
        Err(e) => {
            print_err(format_args!(
                "error[{}]: invalid ref: {e}",
                ErrorCode::InvalidRef.as_wire()
            ));
            return Err(anyhow::Error::new(CliExit(cli_exit_code(
                ErrorCode::InvalidRef,
            ))));
        }
    };

    // Dry-run branch: build the plan and emit it. NO harness, NO network,
    // NO store write, NO provenance row. Posture-lint ADR-0022 §5 will
    // verify this branch never reaches `HttpClient::fetch_*`,
    // `FsStore::write_*`, or `ProvenanceLog::append`.
    if dry_run {
        // Resolve store root for path projections. Failures here surface
        // as a normal CLI error (not as a denial) — same behaviour the
        // non-dry-run path would exhibit on a misconfigured environment.
        let store_root = super::resolve_store_root()?;
        let plan = build_fetch_plan(&ref_, &store_root);
        emit_dry_run_plan_to_stdout(&ref_, &plan)?;
        return Ok(());
    }

    // Step 2: build harness (foundation modules + provenance log).
    let harness = FetchHarness::from_env()?;

    // Step 3: emit SessionStart. Fail-closed if the log write fails — the
    // surrounding fetch MUST NOT proceed (`docs/PROVENANCE_LOG.md` §5).
    harness.log_session_start(Some(ref_.as_input_str()))?;

    // Step 4: dispatch on ref kind.
    let result = harness.fetch_one(&ref_).await;

    // Step 5: emit SessionEnd regardless of outcome. Best-effort: if this
    // append also fails, surface the underlying fetch error (or a fresh one
    // if the fetch was Ok).
    harness.log_session_end(result.is_ok(), Some(ref_.as_input_str()));

    result
}

/// Single-line user-visible success message, written to stderr per ADR-0001
/// (stdio convention — the CLI never writes a success line to stdout). This
/// is the one place where `eprintln!` is intentional; the workspace
/// `clippy::print_stderr` lint is `warn` so the localized `#[allow]` is the
/// minimal intervention.
#[allow(clippy::print_stderr)]
fn print_success(args: std::fmt::Arguments<'_>) {
    eprintln!("{args}");
}

/// Stderr sink for the `docs/ERRORS.md` §3 human-error lines. Mirrors
/// [`print_success`]; the localized `#[allow]` is the minimal
/// intervention for the workspace `clippy::print_stderr` lint.
#[allow(clippy::print_stderr)]
fn print_err(args: std::fmt::Arguments<'_>) {
    eprintln!("{args}");
}

/// Carries a `docs/ERRORS.md` §4 process exit code out of a CLI
/// command to `main`, which owns the actual `std::process::exit`
/// (calling it inside `run_with_options` would kill in-process
/// integration tests). The human-readable `error[CODE]: …` line has
/// ALREADY been written to stderr by `render_fetch_error` before
/// this is constructed, so `main` must NOT print it again. Issue #119.
#[derive(Debug)]
pub struct CliExit(pub i32);

impl std::fmt::Display for CliExit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "exiting with status {}", self.0)
    }
}

impl std::error::Error for CliExit {}

/// `docs/ERRORS.md` §4 closed-code → process exit code. Anything not
/// individually listed falls under "at least one fetch failed" (1).
///
/// `pub(crate)` so sibling subcommands (`commands::graph`, …) route
/// their typed denials through the SAME centralized mapping instead of
/// open-coding magic exit numbers — keeps the `ErrorCode`→exit contract
/// single-sourced (issue #149).
pub(crate) fn cli_exit_code(code: ErrorCode) -> i32 {
    match code {
        ErrorCode::CapabilityDenied => 3,
        ErrorCode::StoreError | ErrorCode::LogError => 4,
        ErrorCode::FetchTimeout => 124,
        _ => 1,
    }
}

/// Render a terminal [`FetchError`] in the `docs/ERRORS.md` §3
/// "Researcher (CLI human)" form: `error[CODE]: message` on stderr,
/// plus an actionable `= note:` line carrying the ADR-0023
/// `denial_context` (attempted / expected hosts) when the failure was
/// a denial class. stdout stays clean (ADR-0001).
fn render_fetch_error(e: &FetchError) {
    let code: ErrorCode = e.into();
    print_err(format_args!("error[{}]: {}", code.as_wire(), e));
    if let Some(dc) = Option::<DenialContext>::from(e) {
        let attempted = dc.attempted.as_deref().unwrap_or("(unknown)");
        match &dc.expected {
            Some(exp) if !exp.is_empty() => {
                print_err(format_args!(
                    "  = note: attempted {attempted}; allowed: {}",
                    exp.join(", ")
                ));
            }
            _ => {
                print_err(format_args!("  = note: attempted {attempted}"));
            }
        }
    }
}

/// Render a `PdfLegStatus::Blocked` outcome in the `docs/ERRORS.md` §3
/// "Researcher (CLI human)" form. Issue #145: an OA PDF was discovered
/// but could not be retrieved — the metadata WAS written, but this is a
/// denial, not a clean success. We emit the same `error[CODE]:` stderr
/// shape as [`render_fetch_error`] (so pipelines and humans see an
/// unambiguous failure), name the metadata path that DID land so the
/// partial result is still discoverable, and surface the ADR-0023
/// `denial_context` note when present. stdout stays clean (ADR-0001).
fn render_blocked_error(
    ref_: &Ref,
    outcome: &FetchPaperOutcome,
    code: ErrorCode,
    message: &str,
    denial: Option<&DenialContext>,
) {
    let label = match ref_ {
        Ref::Arxiv(id) => format!("arxiv:{}", id.as_str()),
        Ref::Doi(doi) => format!("doi:{}", doi.as_str()),
    };
    print_err(format_args!(
        "error[{}]: {label}: an OA PDF was found but could not be retrieved: {message}",
        code.as_wire()
    ));
    if let Some(dc) = denial {
        let attempted = dc.attempted.as_deref().unwrap_or("(unknown)");
        match &dc.expected {
            Some(exp) if !exp.is_empty() => {
                print_err(format_args!(
                    "  = note: attempted {attempted}; allowed: {}",
                    exp.join(", ")
                ));
            }
            _ => {
                print_err(format_args!("  = note: attempted {attempted}"));
            }
        }
    }
    // The metadata TOML still landed; point the user at it so the
    // partial result is not lost (it is still useful), without
    // pretending the fetch succeeded.
    print_err(format_args!(
        "  = note: metadata-only record written to {}",
        outcome.path
    ));
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn new_session_id_is_26_chars() {
        // ULID textual form is fixed-width 26 chars (Crockford base32).
        // `docs/PROVENANCE_LOG.md` §3 requires this exact length.
        let id = new_session_id();
        assert_eq!(id.len(), 26, "session id must be 26 chars: {:?}", id);
        // Crockford base32 uses uppercase letters and digits; specifically
        // I, L, O, U are excluded. Every char must be ASCII alphanumeric.
        assert!(
            id.chars().all(|c| c.is_ascii_alphanumeric()),
            "ulid must be ASCII alphanumeric: {:?}",
            id
        );
    }

    // Slice 2: the `extract_crossref_fields_*` unit tests moved to
    // `doiget_core::orchestrator::tests` along with the function they
    // covered. The CLI no longer owns those helpers; the marker test
    // below keeps the CLI's `fetch::tests` non-empty after the helper
    // migration so a future regression that nukes the delegation path
    // surfaces as a build failure (the `FetchPaperOutcome` re-import
    // would stop resolving).
    #[test]
    fn fetch_paper_outcome_is_reachable_from_cli() {
        let _ = std::any::type_name::<doiget_core::orchestrator::FetchPaperOutcome>();
    }
}
