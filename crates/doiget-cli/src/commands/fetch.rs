//! `doiget fetch <ref>` subcommand.
//!
//! Phase 1 scope:
//!
//! - **arXiv refs** — full end-to-end: PDF bytes are fetched via
//!   [`ArxivSource`], the `[doiget]` extension table is populated with the
//!   resolved license, source, size, and `fetched_at`, and the result is
//!   written to the on-disk store with both the metadata TOML and the PDF.
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

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use chrono::Utc;

use doiget_core::http::{oa_publisher_allowlist, tier_1_allowlist, HttpClient, HttpError};
use doiget_core::provenance::{Capability, LogEvent, LogResult, ProvenanceLog, RowInput};
use doiget_core::rate_limiter::RateLimiter;
use doiget_core::source::{FetchContext, FetchResult, Source};
use doiget_core::sources::arxiv::ArxivSource;
use doiget_core::sources::crossref::CrossrefSource;
use doiget_core::sources::unpaywall::UnpaywallSource;
use doiget_core::store::{DoigetExtension, FsStore, Metadata, Store};
use doiget_core::{ArxivId, CapabilityProfile, Doi, RateLimits, Ref, Safekey, SCHEMA_VERSION};

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

/// Resolve the on-disk store root. `DOIGET_STORE_ROOT` wins; otherwise
/// fall back to `$HOME/papers` (POSIX) or `%USERPROFILE%\papers` (Windows).
fn resolve_store_root() -> Result<Utf8PathBuf> {
    if let Some(s) = read_env_utf8("DOIGET_STORE_ROOT")? {
        return Ok(Utf8PathBuf::from(s));
    }
    let home = home_dir_utf8()?;
    Ok(home.join("papers"))
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

    if arxiv.is_none() && crossref.is_none() && unpaywall.is_none() && oa_publisher.is_none() {
        let mut allowlists = tier_1_allowlist();
        allowlists.extend(oa_publisher_allowlist());
        return HttpClient::new(allowlists).context("building HTTP client");
    }

    // Test-base mode: build a relaxed client per overridden source.
    let mut owned: Vec<(String, String)> = Vec::new();
    for (source, base) in [
        ("arxiv", arxiv.as_deref()),
        ("crossref", crossref.as_deref()),
        ("unpaywall", unpaywall.as_deref()),
        ("oa-publisher", oa_publisher.as_deref()),
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

/// Construct an [`ArxivSource`] honoring `DOIGET_ARXIV_BASE` if set.
fn build_arxiv_source() -> Result<ArxivSource> {
    if let Ok(s) = std::env::var("DOIGET_ARXIV_BASE") {
        let url =
            url::Url::parse(&s).with_context(|| format!("DOIGET_ARXIV_BASE is not a URL: {s}"))?;
        return Ok(ArxivSource::with_base(url));
    }
    Ok(ArxivSource::new())
}

/// Construct a [`CrossrefSource`] honoring `DOIGET_CROSSREF_BASE` if set.
fn build_crossref_source(contact_email: &str) -> Result<CrossrefSource> {
    if let Ok(s) = std::env::var("DOIGET_CROSSREF_BASE") {
        let url = url::Url::parse(&s)
            .with_context(|| format!("DOIGET_CROSSREF_BASE is not a URL: {s}"))?;
        return Ok(CrossrefSource::with_base(url, contact_email.to_string()));
    }
    Ok(CrossrefSource::new(contact_email.to_string()))
}

/// Construct an [`UnpaywallSource`] honoring `DOIGET_UNPAYWALL_BASE` if set.
fn build_unpaywall_source(contact_email: &str) -> Result<UnpaywallSource> {
    if let Ok(s) = std::env::var("DOIGET_UNPAYWALL_BASE") {
        let url = url::Url::parse(&s)
            .with_context(|| format!("DOIGET_UNPAYWALL_BASE is not a URL: {s}"))?;
        return Ok(UnpaywallSource::with_base(url, contact_email.to_string()));
    }
    Ok(UnpaywallSource::new(contact_email.to_string()))
}

/// Resolved configuration derived from the environment.
pub(crate) struct OrchestratorConfig {
    pub(crate) store_root: Utf8PathBuf,
    pub(crate) log_path: Utf8PathBuf,
    pub(crate) contact_email: String,
    pub(crate) unpaywall_email: String,
}

impl OrchestratorConfig {
    fn from_env() -> Result<Self> {
        let store_root = resolve_store_root()?;
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
        });
    }

    /// Run a single ref through the per-kind orchestration (arxiv → PDF +
    /// metadata; doi → metadata-only via Crossref + Unpaywall). Errors here
    /// are scoped to this one ref — the caller decides whether to abort the
    /// surrounding session.
    pub(crate) async fn fetch_one(&self, ref_: &Ref) -> Result<()> {
        let safekey = ref_.safekey();
        let ctx = self.fetch_context();
        match ref_ {
            Ref::Arxiv(id) => {
                fetch_arxiv(
                    id,
                    ref_,
                    &self.profile,
                    &ctx,
                    &self.store,
                    &safekey,
                    &self.cfg,
                )
                .await
            }
            Ref::Doi(doi) => {
                fetch_doi(
                    doi,
                    ref_,
                    &self.profile,
                    &ctx,
                    &self.store,
                    &safekey,
                    &self.cfg,
                )
                .await
            }
        }
    }
}

/// Per-fetch option bundle threaded from `main.rs` clap dispatch.
///
/// The bundle is additive: `Default` produces the historical
/// `pub async fn run(input)` behaviour, so existing callers (integration
/// tests under `crates/doiget-cli/tests/`) that go through [`run`]
/// continue to work unchanged. New callers (the binary's
/// `clap::Subcommand::Fetch` arm and the dry-run integration test) go
/// through [`run_with_options`].
///
/// Currently exposes a single field — `dry_run` per ADR-0022 §1.
#[derive(Debug, Clone, Default)]
pub struct FetchOptions {
    /// When `true`, build a [`FetchPlan`], emit it as JSON on stdout, and
    /// return `Ok(())` without touching the network or filesystem and
    /// without appending a provenance row.
    pub dry_run: bool,
}

/// Run the `doiget fetch <ref>` subcommand with the historical (dry-run-
/// disabled) defaults.
///
/// Equivalent to `run_with_options(input, FetchOptions::default())`. Kept
/// at this signature so the existing in-process integration tests under
/// `crates/doiget-cli/tests/` (which call `fetch::run("...".into())`)
/// continue to compile unchanged.
///
/// On success returns `Ok(())` and writes a one-line success message to
/// stderr (per ADR-0001 stdio convention — no stdout writes from `fetch`
/// on the normal path). On failure, returns an `anyhow::Error` and emits
/// a `SessionEnd` row with `result=err` to the provenance log before
/// returning.
pub async fn run(input: String) -> Result<()> {
    run_with_options(input, FetchOptions::default()).await
}

/// Run the `doiget fetch <ref>` subcommand with the given options.
///
/// When `opts.dry_run` is `true` (per ADR-0022 §1):
///   - Build a [`FetchPlan`] from the parsed [`Ref`] and the configured
///     store root.
///   - Serialize it to JSON and write to stdout.
///   - Return `Ok(())` immediately, **without** building a
///     [`FetchHarness`] (no provenance log open), without contacting the
///     network, without writing to the store, and without appending a
///     provenance row.
///
/// When `opts.dry_run` is `false`, the body is identical to the
/// historical [`run`] implementation.
pub async fn run_with_options(input: String, opts: FetchOptions) -> Result<()> {
    // Step 1: parse + safekey. Granular `RefParseError` collapses to anyhow
    // via `?`; the higher-level CLI binary maps the error to its exit code.
    let ref_ = Ref::parse(&input).with_context(|| format!("invalid ref: {input}"))?;

    // Dry-run branch: build the plan and emit it. NO harness, NO network,
    // NO store write, NO provenance row. Posture-lint ADR-0022 §5 will
    // verify this branch never reaches `HttpClient::fetch_*`,
    // `FsStore::write_*`, or `ProvenanceLog::append`.
    if opts.dry_run {
        // Resolve store root for path projections. Failures here surface
        // as a normal CLI error (not as a denial) — same behaviour the
        // non-dry-run path would exhibit on a misconfigured environment.
        let store_root = resolve_store_root()?;
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

/// arXiv branch — full PDF + minimal metadata.
async fn fetch_arxiv(
    id: &ArxivId,
    ref_: &Ref,
    profile: &CapabilityProfile,
    ctx: &FetchContext,
    store: &FsStore,
    safekey: &Safekey,
    _cfg: &OrchestratorConfig,
) -> Result<()> {
    let source = build_arxiv_source()?;
    if !source.can_serve(profile, ref_) {
        return Err(anyhow!("arXiv source declined to serve {}", id.as_str()));
    }

    let FetchResult {
        license,
        pdf_bytes,
        final_url,
        ..
    } = source
        .fetch(ref_, profile, ctx)
        .await
        .with_context(|| format!("arxiv fetch failed for {}", id.as_str()))?;
    let pdf = pdf_bytes.ok_or_else(|| anyhow!("arxiv source returned no PDF bytes"))?;
    let size_bytes = pdf.len() as u64;

    // Phase 1 minimal metadata: title placeholder = "arxiv:<id>". Full
    // export.arxiv.org Atom-feed parsing is deferred to a follow-up PR.
    let metadata = Metadata {
        schema_version: SCHEMA_VERSION.to_string(),
        title: format!("arxiv:{}", id.as_str()),
        authors: Vec::new(),
        year: None,
        doi: None,
        arxiv_id: Some(id.clone()),
        abstract_: None,
        venue: None,
        publisher: None,
        issn: None,
        isbn: None,
        type_: None,
        keywords: Vec::new(),
        url: final_url.as_ref().map(|u| u.to_string()),
        pdf_path: Some(format!("{}.pdf", safekey.as_str())),
        doiget: Some(DoigetExtension {
            fetched_at: Utc::now(),
            source: "arxiv".to_string(),
            license: license.clone(),
            size_bytes,
            mcp_call_id: None,
        }),
        other: BTreeMap::new(),
    };

    // The Store::write contract takes a path to the PDF (not bytes); stage
    // bytes to a tempfile so the existing atomic-rename code path applies.
    let tmp = tempfile::NamedTempFile::new().context("creating PDF staging tempfile")?;
    std::fs::write(tmp.path(), &pdf).context("staging PDF bytes")?;
    let pdf_src = Utf8Path::from_path(tmp.path())
        .ok_or_else(|| anyhow!("staging tempfile path is not UTF-8"))?
        .to_path_buf();

    write_to_store(store, safekey, &metadata, Some(&pdf_src), ctx).await?;
    drop(tmp); // explicit drop to keep the tempfile alive across write_to_store

    // Stderr success line per ADR-0001 stdio convention. The CLI MUST
    // never write a success line to stdout (stdio is reserved for MCP
    // JSON-RPC frames in `doiget serve`). `eprintln!` is the canonical way
    // to surface a one-line user-visible confirmation; the workspace lint
    // is warn-level, but `-D warnings` in CI promotes it, so an explicit
    // localized allow is required here.
    let pdf_path = store.root().join(format!("{}.pdf", safekey.as_str()));
    print_success(format_args!(
        "fetched arxiv:{} ({} bytes) -> {}",
        id.as_str(),
        size_bytes,
        pdf_path
    ));
    Ok(())
}

/// DOI branch — Crossref metadata + Unpaywall license enrichment, plus
/// (Phase 1 success criterion) an OA PDF fetch when Unpaywall returns a
/// `best_oa_location` URL whose host is in the `oa-publisher` allowlist
/// (`docs/REDIRECT_ALLOWLIST.md` §3).
///
/// **Partial-success semantics.** Per the `informed-best-effort` posture in
/// `docs/REDIRECT_ALLOWLIST.md` §3, an OA URL whose host is NOT in the
/// allowlist (or whose body fails the magic-byte check) does NOT abort the
/// fetch — the metadata is still useful. The PDF outcome is logged as a
/// distinct `Fetch` row with `source = "oa-publisher"` and the metadata
/// write proceeds either way.
async fn fetch_doi(
    doi: &Doi,
    ref_: &Ref,
    profile: &CapabilityProfile,
    ctx: &FetchContext,
    store: &FsStore,
    safekey: &Safekey,
    cfg: &OrchestratorConfig,
) -> Result<()> {
    // Crossref first — bibliographic fields.
    let crossref = build_crossref_source(&cfg.contact_email)?;
    let cross = crossref
        .fetch(ref_, profile, ctx)
        .await
        .with_context(|| format!("crossref fetch failed for {}", doi.as_str()))?;
    let crossref_meta = cross.metadata_json.unwrap_or(serde_json::Value::Null);
    let extracted = extract_crossref_fields(&crossref_meta);

    // Unpaywall second — license enrichment + OA URL discovery. A failure
    // here is non-fatal: we still write the Crossref-derived metadata.
    let unpaywall = build_unpaywall_source(&cfg.unpaywall_email)?;
    let upw_result = unpaywall.fetch(ref_, profile, ctx).await;
    let (license, source_label, oa_url) = match upw_result {
        Ok(r) => {
            let oa = extract_best_oa_url(r.metadata_json.as_ref());
            let label = if r.license != "unknown" {
                "unpaywall".to_string()
            } else {
                "crossref".to_string()
            };
            (r.license, label, oa)
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "unpaywall fetch failed; continuing with crossref-only metadata"
            );
            ("unknown".to_string(), "crossref".to_string(), None)
        }
    };

    // Step: OA PDF leg. Try the URL Unpaywall returned via the
    // `oa-publisher` source allowlist. Magic-byte check is enforced inside
    // `HttpClient::fetch_pdf`. Any failure here is logged as a distinct
    // `Fetch err` row and falls through to metadata-only success.
    let pdf_outcome = if let Some(url) = oa_url {
        try_fetch_oa_pdf(doi, &url, ctx).await
    } else {
        None
    };

    // If the PDF leg succeeded, the on-disk source label flips to
    // `oa-publisher` (we got actual content, not just a metadata-derived
    // license). Otherwise keep the metadata-source label.
    let (final_source_label, size_bytes, pdf_path_relative, pdf_staged) = match &pdf_outcome {
        Some((bytes, _final_url)) => {
            let staged = stage_pdf_to_tempfile(bytes.as_ref())?;
            (
                "oa-publisher".to_string(),
                bytes.len() as u64,
                Some(format!("{}.pdf", safekey.as_str())),
                Some(staged),
            )
        }
        None => (source_label, 0u64, None, None),
    };

    let metadata = Metadata {
        schema_version: SCHEMA_VERSION.to_string(),
        title: extracted.title.unwrap_or_else(|| doi.as_str().to_string()),
        authors: extracted.authors,
        year: extracted.year,
        doi: Some(doi.clone()),
        arxiv_id: None,
        abstract_: None,
        venue: extracted.venue,
        publisher: None,
        issn: None,
        isbn: None,
        type_: extracted.type_,
        keywords: Vec::new(),
        url: cross.final_url.as_ref().map(|u| u.to_string()),
        pdf_path: pdf_path_relative,
        doiget: Some(DoigetExtension {
            fetched_at: Utc::now(),
            source: final_source_label,
            license,
            size_bytes,
            mcp_call_id: None,
        }),
        other: BTreeMap::new(),
    };

    let pdf_src_path = pdf_staged
        .as_ref()
        .and_then(|tmp| Utf8Path::from_path(tmp.path()).map(|p| p.to_path_buf()));
    write_to_store(store, safekey, &metadata, pdf_src_path.as_deref(), ctx).await?;
    drop(pdf_staged); // keep tempfile alive across write_to_store

    if pdf_outcome.is_some() {
        let pdf_path = store.root().join(format!("{}.pdf", safekey.as_str()));
        print_success(format_args!(
            "fetched doi:{} ({} bytes) -> {}",
            doi.as_str(),
            metadata.doiget.as_ref().map(|d| d.size_bytes).unwrap_or(0),
            pdf_path
        ));
    } else {
        let toml_path = store
            .root()
            .join(".metadata")
            .join(format!("{}.toml", safekey.as_str()));
        print_success(format_args!(
            "fetched doi:{} (metadata-only) -> {}",
            doi.as_str(),
            toml_path
        ));
    }
    Ok(())
}

/// Pull the preferred OA URL out of an Unpaywall `metadata_json` envelope.
/// Tries `best_oa_location.url_for_pdf` first (direct PDF link), falling
/// back to `best_oa_location.url` (landing page that typically redirects
/// to the PDF). Returns `None` if neither field is present, malformed, or
/// fails to parse as a URL.
fn extract_best_oa_url(meta: Option<&serde_json::Value>) -> Option<url::Url> {
    let meta = meta?;
    let loc = meta.get("best_oa_location")?;
    let candidate = loc
        .get("url_for_pdf")
        .and_then(|v| v.as_str())
        .or_else(|| loc.get("url").and_then(|v| v.as_str()))?;
    url::Url::parse(candidate).ok()
}

/// Attempt the OA PDF fetch under the `"oa-publisher"` source key. Logs a
/// `Fetch ok` row on success and a `Fetch err` row with
/// `error_code = "NETWORK_ERROR"` on every failure mode (redirect denied,
/// magic-byte mismatch, transport, status, timeout). Returns `Some((bytes,
/// final_url))` on success, `None` otherwise — the caller treats `None` as
/// "metadata-only success", per the `informed-best-effort` posture in
/// `docs/REDIRECT_ALLOWLIST.md` §3.
async fn try_fetch_oa_pdf(
    doi: &Doi,
    url: &url::Url,
    ctx: &FetchContext,
) -> Option<(Vec<u8>, url::Url)> {
    const SOURCE: &str = "oa-publisher";
    let _permit = ctx.rate_limiter.acquire(SOURCE).await;
    match ctx.http.fetch_pdf(SOURCE, url.clone()).await {
        Ok((body, final_url)) => {
            let size_bytes = body.len() as u64;
            // Best-effort log row; if appending fails, surface a tracing
            // warning but still return success — the body is in memory and
            // the metadata write below will still go through.
            if let Err(e) = ctx.log.append(RowInput {
                event: LogEvent::Fetch,
                result: LogResult::Ok,
                capability: Capability::Oa,
                ref_: Some(doi.as_str()),
                source: Some(SOURCE),
                error_code: None,
                size_bytes: Some(size_bytes),
                license: None,
                store_path: None,
            }) {
                tracing::warn!(error = %e, "appending oa-publisher Fetch ok row failed");
            }
            Some((body.to_vec(), final_url))
        }
        Err(e) => {
            // Categorize for tracing visibility — the on-disk row is the
            // same `NETWORK_ERROR` for all transport-level outcomes per
            // `docs/ERRORS.md`.
            match &e {
                HttpError::RedirectDenied { host, .. } => {
                    tracing::info!(
                        oa_url = %url,
                        denied_host = %host,
                        "OA URL host outside oa-publisher allowlist; metadata-only fallback"
                    );
                }
                HttpError::NotAPdf { .. } => {
                    tracing::info!(
                        oa_url = %url,
                        "OA URL did not return a PDF magic byte; metadata-only fallback"
                    );
                }
                other => {
                    tracing::warn!(
                        oa_url = %url,
                        error = %other,
                        "OA PDF fetch failed; metadata-only fallback"
                    );
                }
            }
            let _ = ctx.log.append(RowInput {
                event: LogEvent::Fetch,
                result: LogResult::Err,
                capability: Capability::Oa,
                ref_: Some(doi.as_str()),
                source: Some(SOURCE),
                error_code: Some("NETWORK_ERROR"),
                size_bytes: None,
                license: None,
                store_path: None,
            });
            None
        }
    }
}

/// Stage PDF bytes to a tempfile so the existing `Store::write` atomic-
/// rename code path applies (the store takes a path, not bytes). Mirrors
/// the staging helper inlined in `fetch_arxiv`.
fn stage_pdf_to_tempfile(bytes: &[u8]) -> Result<tempfile::NamedTempFile> {
    let tmp = tempfile::NamedTempFile::new().context("creating PDF staging tempfile")?;
    std::fs::write(tmp.path(), bytes).context("staging PDF bytes")?;
    Ok(tmp)
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

/// Subset of Crossref `message` we extract for the on-disk metadata.
struct CrossrefFields {
    title: Option<String>,
    authors: Vec<String>,
    year: Option<i32>,
    venue: Option<String>,
    type_: Option<String>,
}

/// Defensively pull the bibliographic fields out of a Crossref envelope's
/// `message` object. Every field is optional; malformed shapes degrade to
/// `None` rather than panicking.
fn extract_crossref_fields(msg: &serde_json::Value) -> CrossrefFields {
    let title = msg
        .get("title")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let authors = msg
        .get("author")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|a| {
                    let family = a.get("family").and_then(|v| v.as_str());
                    let given = a.get("given").and_then(|v| v.as_str());
                    match (family, given) {
                        (Some(f), Some(g)) => Some(format!("{f}, {g}")),
                        (Some(f), None) => Some(f.to_string()),
                        (None, Some(g)) => Some(g.to_string()),
                        _ => None,
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    let year = msg
        .get("issued")
        .and_then(|v| v.get("date-parts"))
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_i64())
        .and_then(|n| i32::try_from(n).ok());

    let venue = msg
        .get("container-title")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let type_ = msg
        .get("type")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    CrossrefFields {
        title,
        authors,
        year,
        venue,
        type_,
    }
}

/// Persist `metadata` (and optionally a PDF at `pdf_src`) to the store, then
/// emit a `StoreWrite` provenance row. Failures of either step are
/// fail-closed.
async fn write_to_store(
    store: &FsStore,
    safekey: &Safekey,
    metadata: &Metadata,
    pdf_src: Option<&Utf8Path>,
    ctx: &FetchContext,
) -> Result<()> {
    let store_path_relative = if pdf_src.is_some() {
        format!("{}.pdf", safekey.as_str())
    } else {
        format!(".metadata/{}.toml", safekey.as_str())
    };
    let size_bytes = metadata.doiget.as_ref().map(|d| d.size_bytes).unwrap_or(0);
    let license = metadata.doiget.as_ref().map(|d| d.license.as_str());
    let source_name = metadata.doiget.as_ref().map(|d| d.source.as_str());

    match store.write(safekey, metadata, pdf_src) {
        Ok(()) => {
            ctx.log
                .append(RowInput {
                    event: LogEvent::StoreWrite,
                    result: LogResult::Ok,
                    capability: Capability::Oa,
                    ref_: metadata
                        .doi
                        .as_ref()
                        .map(|d| d.as_str())
                        .or_else(|| metadata.arxiv_id.as_ref().map(|a| a.as_str())),
                    source: source_name,
                    error_code: None,
                    size_bytes: Some(size_bytes),
                    license,
                    store_path: Some(&store_path_relative),
                })
                .context("appending StoreWrite row")?;
            Ok(())
        }
        Err(e) => {
            // Best-effort: record the StoreWrite failure before propagating.
            let _ = ctx.log.append(RowInput {
                event: LogEvent::StoreWrite,
                result: LogResult::Err,
                capability: Capability::Oa,
                ref_: metadata
                    .doi
                    .as_ref()
                    .map(|d| d.as_str())
                    .or_else(|| metadata.arxiv_id.as_ref().map(|a| a.as_str())),
                source: source_name,
                error_code: Some("STORE_ERROR"),
                size_bytes: None,
                license: None,
                store_path: Some(&store_path_relative),
            });
            Err(anyhow::Error::new(e).context("writing to store"))
        }
    }
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

    #[test]
    fn extract_crossref_fields_parses_all_fields() {
        let msg = serde_json::json!({
            "title": ["Example Title"],
            "author": [
                { "family": "Smith", "given": "Alice" },
                { "family": "Jones", "given": "Bob" }
            ],
            "issued": { "date-parts": [[2024, 1, 15]] },
            "container-title": ["Phys. Rev. X"],
            "type": "journal-article"
        });
        let f = extract_crossref_fields(&msg);
        assert_eq!(f.title.as_deref(), Some("Example Title"));
        assert_eq!(
            f.authors,
            vec!["Smith, Alice".to_string(), "Jones, Bob".to_string()]
        );
        assert_eq!(f.year, Some(2024));
        assert_eq!(f.venue.as_deref(), Some("Phys. Rev. X"));
        assert_eq!(f.type_.as_deref(), Some("journal-article"));
    }

    #[test]
    fn extract_crossref_fields_tolerates_missing_fields() {
        let msg = serde_json::json!({});
        let f = extract_crossref_fields(&msg);
        assert!(f.title.is_none());
        assert!(f.authors.is_empty());
        assert!(f.year.is_none());
        assert!(f.venue.is_none());
        assert!(f.type_.is_none());
    }

    #[test]
    fn extract_crossref_fields_handles_partial_author_records() {
        // An author with only `family` should still produce an entry; an
        // entry with neither `family` nor `given` is skipped.
        let msg = serde_json::json!({
            "author": [
                { "family": "Carol" },
                { "given": "David" },
                {}
            ]
        });
        let f = extract_crossref_fields(&msg);
        assert_eq!(f.authors, vec!["Carol".to_string(), "David".to_string()]);
    }
}
