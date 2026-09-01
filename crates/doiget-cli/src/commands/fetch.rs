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
//! | `DOIGET_STORE_ROOT` | `./papers` (under the current working dir) | Filesystem store root |
//! | `DOIGET_LOG_PATH` | `<config>/doiget/access.jsonl` | Provenance log file |
//! | `DOIGET_CONTACT_EMAIL` | `doiget@localhost` | Polite-pool contact email (User-Agent and Crossref) |
//! | `DOIGET_UNPAYWALL_EMAIL` | (= contact email) | Unpaywall query-string email |
//! | `DOIGET_ARXIV_BASE` | `https://arxiv.org` | arXiv source base (test override) |
//! | `DOIGET_CROSSREF_BASE` | `https://api.crossref.org` | Crossref source base (test override) |
//! | `DOIGET_UNPAYWALL_BASE` | `https://api.unpaywall.org/v2` | Unpaywall source base (test override) |
//! | `DOIGET_OA_PUBLISHER_BASE` | (production allowlist) | OA publisher host allowlist override (test override) |

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use camino::{Utf8Path, Utf8PathBuf};

use super::output::print_err;
#[cfg(feature = "metadata")]
use doiget_core::http::tier_2_allowlist;
use doiget_core::http::{
    discovery_allowlist, fulltext_allowlist, oa_publisher_allowlist, tier_1_allowlist,
    tier_3_allowlists, HttpClient,
};
use doiget_core::orchestrator::{
    fetch_paper as core_fetch_paper, FetchPaperOutcome, PdfLegStatus, SourceAttempt,
};
use doiget_core::provenance::{Capability, LogEvent, LogResult, ProvenanceLog, RowInput};
use doiget_core::rate_limiter::RateLimiter;
use doiget_core::source::{FetchContext, FetchError};
use doiget_core::store::FsStore;
use doiget_core::{CapabilityProfile, DenialContext, DenialReason, ErrorCode, RateLimits, Ref};

/// Defer to docs/PROVENANCE_LOG.md §3: 26-char ULID per process invocation.
pub(crate) fn new_session_id() -> String {
    ulid::Ulid::generate().to_string()
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
pub(crate) fn resolve_log_path() -> Result<Utf8PathBuf> {
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

/// Config-dir resolution, delegated to `doiget_core::user_extension`.
///
/// This used to be one of three copies. The previous comment here asked
/// the reader to "keep the signature stable" because divergence from the
/// MCP-side copy "would silently desync the user-extension allowlist
/// surfaces" — and they had already diverged, this one accepting
/// `XDG_CONFIG_HOME=""` and resolving a *relative* config path under the
/// cwd where the MCP copy treated blank as unset. The shared resolver
/// keeps blank-is-unset, so a blank variable no longer silently selects a
/// different file.
///
/// Kept as a crate-visible wrapper so the ~20 call sites in
/// `commands::capabilities` / `commands::config` are unchanged.
pub(crate) fn config_dir_utf8() -> Result<Utf8PathBuf> {
    Ok(doiget_core::user_extension::config_dir()?)
}

/// Best-effort resolver-cache root (`docs/CACHE.md`). Honors
/// `DOIGET_CACHE_ROOT` first, then `XDG_CACHE_HOME/doiget` (POSIX), then
/// `LOCALAPPDATA\doiget\cache` (Windows), then `$HOME/.cache/doiget`.
/// Crate-visible so the `verify` command can enable the resolve cache.
pub(crate) fn cache_dir_utf8() -> Result<Utf8PathBuf> {
    if let Some(s) = read_env_utf8("DOIGET_CACHE_ROOT")? {
        return Ok(Utf8PathBuf::from(s));
    }
    if let Some(s) = read_env_utf8("XDG_CACHE_HOME")? {
        return Ok(Utf8PathBuf::from(s).join("doiget"));
    }
    if let Some(s) = read_env_utf8("LOCALAPPDATA")? {
        return Ok(Utf8PathBuf::from(s).join("doiget").join("cache"));
    }
    let home = home_dir_utf8()?;
    Ok(home.join(".cache").join("doiget"))
}

/// Build a metadata-resolution [`FetchContext`]: HTTP client, rate
/// limiter, and provenance log resolved from the environment, with the
/// resolver cache (`docs/CACHE.md`) enabled best-effort.
///
/// This is the shared context for the read-only resolve commands
/// (`verify`, `cite`) — neither persists to the store, so no store
/// handle is constructed. Enabling `cache_root` means repeat resolves of
/// the same ref are served from disk, avoiding upstream rate limits; if
/// the cache dir can't be resolved the run simply proceeds without it.
pub(crate) fn build_resolve_context() -> Result<FetchContext> {
    let session_id = new_session_id();
    let log_path = resolve_log_path()?;
    let http = Arc::new(build_http_client(None)?);
    let rate_limiter = Arc::new(RateLimiter::new(RateLimits::HARD_CODED));
    let log = Arc::new(
        ProvenanceLog::open(log_path, session_id.clone())
            .context("failed to open provenance log")?,
    );
    let cache_root = cache_dir_utf8().ok();
    Ok(FetchContext {
        http,
        rate_limiter,
        log,
        session_id,
        cache_root,
    })
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
pub(crate) fn build_http_client(user_agent: Option<&str>) -> Result<HttpClient> {
    let arxiv = std::env::var("DOIGET_ARXIV_BASE").ok();
    let crossref = std::env::var("DOIGET_CROSSREF_BASE").ok();
    let unpaywall = std::env::var("DOIGET_UNPAYWALL_BASE").ok();
    let oa_publisher = std::env::var("DOIGET_OA_PUBLISHER_BASE").ok();
    // Slice 16: `DOIGET_OPENALEX_BASE` selects a wiremock host for the
    // citation-graph BFS. Only meaningful with `--features citation`,
    // but reading the env unconditionally keeps the branch logic
    // simple and is harmless for default builds.
    let openalex_base = std::env::var("DOIGET_OPENALEX_BASE").ok();
    // ADR-0032: `DOIGET_AR5IV_BASE` selects a wiremock host for the
    // full-text extraction path (`doiget text`). Test-only override,
    // mirroring `DOIGET_ARXIV_BASE`.
    let ar5iv_base = std::env::var("DOIGET_AR5IV_BASE").ok();

    #[cfg(feature = "tdm-aps")]
    let tdm_aps = std::env::var("DOIGET_APS_BASE").ok();
    #[cfg(feature = "tdm-elsevier")]
    let tdm_elsevier = std::env::var("DOIGET_ELSEVIER_BASE").ok();
    #[cfg(feature = "tdm-springer")]
    let tdm_springer = std::env::var("DOIGET_SPRINGER_BASE").ok();
    #[cfg(feature = "tdm-ieee")]
    let tdm_ieee = std::env::var("DOIGET_IEEE_BASE").ok();
    if arxiv.is_none()
        && crossref.is_none()
        && unpaywall.is_none()
        && oa_publisher.is_none()
        && openalex_base.is_none()
        && ar5iv_base.is_none()
    {
        let mut allowlists = tier_1_allowlist();
        allowlists.extend(oa_publisher_allowlist());
        // ADR-0031: discovery search (`doiget search`) is Tier-1 OA
        // metadata, always-on, and ships in the default `oa-only` binary.
        // Register `api.openalex.org` under the `"openalex"` source key
        // UNCONDITIONALLY so `discovery::paper_search` can reach the
        // `/works?search=` endpoint without `--features citation`. In
        // citation builds the Tier-2 extend below re-registers the same
        // host under the same key (idempotent HashMap overwrite).
        allowlists.extend(discovery_allowlist());
        // ADR-0032: full-text extraction (`doiget text`) is Tier-1 OA
        // metadata, always-on. Register `ar5iv.labs.arxiv.org` under the
        // `"ar5iv"` source key unconditionally so `paper_text::paper_text`
        // can reach ar5iv in `oa-only` builds.
        allowlists.extend(fulltext_allowlist());
        // The Tier-2 transport gate. The sources it serves — OpenAlex,
        // Semantic Scholar, DOAJ, DataCite, HAL, OpenAIRE, CORE and
        // Europe PMC — are compiled under `metadata`, and
        // `resolve_optional_chain` is `#[cfg(feature = "metadata")]`,
        // so this extend MUST be gated on `metadata` too. It was gated
        // on `citation` for six releases: in a `--features metadata`
        // build (which CI's clippy matrix builds explicitly) the chain
        // ran, `can_serve` passed, and the request died at
        // `UnknownSource` because no allowlist entry existed for the
        // key (#516). CapabilityProfile.metadata.* is the runtime gate;
        // this is the transport gate, and the two must agree.
        #[cfg(feature = "metadata")]
        allowlists.extend(tier_2_allowlist());
        // #454: the Tier-3 transport gate. #444 made the orchestrator
        // reach these sources; without this line the fetch it then issues
        // under `tdm-aps` / `tdm-elsevier` / `tdm-springer` dies at
        // `UnknownSource`. Empty in a default build (ADR-0002 — no Tier-3
        // feature is compiled into published binaries), so this is a
        // no-op for the shipped surface.
        allowlists.extend(tier_3_allowlists());

        // ADR-0028 D2: merge user-extension hosts from
        // `<config_dir>/doiget/config.toml`. See
        // `doiget_core::user_extension` for the wire contract and
        // the (deferred) S3b provenance / doctor / capabilities
        // surfaces.
        //
        // Failure handling is opt-in-convenience: a missing config
        // is silent (Ok-empty), a malformed config emits
        // `tracing::warn!` and continues with the curated allowlist,
        // and an unresolvable config dir emits `tracing::debug!`
        // (only happens in stripped envs with no HOME / XDG /
        // APPDATA — review pass I3 / A1).
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

        return match user_agent {
            Some(ua) => HttpClient::new_with_user_agent(allowlists, ua),
            None => HttpClient::new(allowlists),
        }
        .context("building HTTP client");
    }

    // Test-base mode: build a relaxed client per overridden source.
    let mut owned: Vec<(String, String)> = Vec::new();
    // Tier-3 test bases, mirroring the MCP builder. Without these a wiremock
    // e2e cannot reach the TDM-fetched route on this surface either: the
    // override branch's table held only Tier-1/2 keys, so `tdm-aps` was absent
    // from the client's map and the attempt died as `no allowlist registered
    // for source tdm-aps` -- a harness gap that read like #454 coming back.
    //
    // Deliberately NOT part of the production-branch test above: setting only
    // `DOIGET_APS_BASE` to replay a recorded fixture must not silently switch
    // the process to the allow-http test client.
    for (source, base) in [
        ("arxiv", arxiv.as_deref()),
        #[cfg(feature = "tdm-aps")]
        ("tdm-aps", tdm_aps.as_deref()),
        #[cfg(feature = "tdm-elsevier")]
        ("tdm-elsevier", tdm_elsevier.as_deref()),
        #[cfg(feature = "tdm-springer")]
        ("tdm-springer", tdm_springer.as_deref()),
        #[cfg(feature = "tdm-ieee")]
        ("tdm-ieee", tdm_ieee.as_deref()),
        ("crossref", crossref.as_deref()),
        ("unpaywall", unpaywall.as_deref()),
        ("oa-publisher", oa_publisher.as_deref()),
        ("openalex", openalex_base.as_deref()),
        ("ar5iv", ar5iv_base.as_deref()),
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
/// Slice 2: `contact_email` / `unpaywall_email` are read by the
/// `doiget-core::orchestrator::fetch_paper` orchestrator itself
/// (`resolve_contact_email` / `resolve_unpaywall_email` in that module —
/// env var, then `[network]` in `config.toml`, then the default since
/// #504), so the CLI no longer threads them through. The fields
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
        // Through the core resolver even though this struct is not read
        // yet: a dormant third copy of the ladder is still a copy, and it
        // is the one nobody would think to update.
        let contact_email = doiget_core::orchestrator::contact_email_or_placeholder();
        let unpaywall_email = std::env::var("DOIGET_UNPAYWALL_EMAIL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| contact_email.clone());
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
        Self::from_env_with_ua(None)
    }

    /// Like [`from_env`](Self::from_env) but overrides the `User-Agent` on
    /// every HTTP request. Used by `doiget batch --user-agent`.
    pub(crate) fn from_env_with_ua(user_agent: Option<&str>) -> Result<Self> {
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
        let http = Arc::new(build_http_client(user_agent)?);
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
            cache_root: None,
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
    /// `error_code` is the terminal code the caller was given, and it is what
    /// makes the row answer "what did this session tell the user about this
    /// ref?" rather than only "something went wrong" (#507).
    pub(crate) fn log_session_end(
        &self,
        ok: bool,
        ref_input: Option<&str>,
        error_code: Option<&str>,
    ) {
        let result = if ok { LogResult::Ok } else { LogResult::Err };
        let _ = self.log.append(RowInput {
            event: LogEvent::SessionEnd,
            result,
            capability: Capability::Oa,
            ref_: ref_input,
            source: None,
            error_code,
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
    pub(crate) async fn fetch_one(&self, ref_: &Ref) -> Result<FetchPaperOutcome, FetchError> {
        // Pure data path: return the typed outcome (or typed error)
        // without any CLI-only rendering or exit-code synthesis. The
        // single-fetch caller (`run_with_options`) and the batch
        // caller (`commands::batch::classify_joined`) each render the
        // human / JSON surface and map to `CliExit` themselves — see
        // #210 for the rationale (batch's `--json` JSONL needs the
        // structured `FetchPaperOutcome` to emit `result.{safekey,
        // store_path, canonical_digest}` on success and
        // `denial_context` on a `PdfLegStatus::Blocked` outcome, which
        // was unreachable through the previous `Result<()>`
        // signature).
        let ctx = self.fetch_context();
        core_fetch_paper(ref_, &self.profile, &ctx, &self.store, self.store.root()).await
    }
}

/// `true` iff the outcome represents a clean fetch: `Fetched` (full
/// PDF), `NoOaUrl` (metadata-only by design), or `PreprintFallback`
/// (OA blocked but arXiv preprint auto-fetched — issue #325).
/// A `Blocked` PDF leg is a failure for SessionEnd / exit-code purposes.
/// Pulled out so both `run_with_options` and `commands::batch` agree on
/// the failure boundary.
pub(crate) fn outcome_is_clean_success(outcome: &FetchPaperOutcome) -> bool {
    // The rule lives in `doiget-core` now, because the MCP surface needs the
    // same boundary and had only half of it.
    outcome.is_clean_success()
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
            // #505: this is the ONLY outcome that reads as a result rather
            // than an error, which is why it had no trace -- there was no
            // `error[...]` block to hang one on. It is also the one where the
            // absence misleads most: the line above is byte-identical whether
            // the optional sources were on and had nothing, or off and never
            // asked.
            for line in not_found_trace_lines(ref_, &outcome.attempts) {
                print_err(format_args!("{line}"));
            }
        }
        // Issue #325: publisher PDF was blocked, arXiv preprint auto-fetched.
        PdfLegStatus::PreprintFallback { arxiv_id, .. } => {
            print_success(format_args!(
                "fetched {} ({} bytes) via arXiv preprint arxiv:{} -> {}",
                label, outcome.size_bytes, arxiv_id, outcome.path
            ));
        }
        // #458: the publisher served its own copy under the user's TDM
        // agreement. Named explicitly rather than left to the `_` arm
        // below, which would have printed the same line as a plain OA
        // fetch -- the user needs to know the open route failed and which
        // agreement was drawn on, because that is the one with terms
        // attached.
        PdfLegStatus::TdmFetched { source, .. } => {
            print_success(format_args!(
                "fetched {} ({} bytes) via {} under your TDM agreement (no open copy available) -> {}",
                label, outcome.size_bytes, source, outcome.path
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
            suggested_arxiv_id,
        } => {
            // Same #145 reclassification as the primary interception in
            // `fetch_one`, so this fail-closed fallback stays consistent.
            let effective = effective_blocked_code(*code, denial.as_ref());
            render_blocked_error(
                ref_,
                outcome,
                effective,
                message,
                denial.as_ref(),
                suggested_arxiv_id.as_deref(),
            );
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

    // #344: an identity-confirmation line so a caller can verify the RIGHT
    // paper landed without a second `doiget info` call. Skipped for the
    // Blocked fail-closed arm (it rendered an `error[CODE]:` line above, not
    // a success).
    if !matches!(outcome.pdf_leg, PdfLegStatus::Blocked { .. }) {
        emit_identity_line(outcome);
    }
}

/// Render the #344 identity line on stderr:
/// `     "<title>" by <author> et al. (<year>)  [<source>/<oa>]`.
/// Empty pieces are omitted; an unknown OA status renders as `?`.
fn emit_identity_line(outcome: &FetchPaperOutcome) {
    let by = match outcome.authors.as_slice() {
        [] => String::new(),
        [a] => format!(" by {a}"),
        [a, ..] => format!(" by {a} et al."),
    };
    let year = match outcome.year {
        Some(y) => format!(" ({y})"),
        None => String::new(),
    };
    let oa = outcome.oa_status.as_deref().unwrap_or("?");
    print_success(format_args!(
        "     \"{}\"{}{}  [{}/{}]",
        outcome.title, by, year, outcome.source, oa
    ));
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
pub async fn run_with_options(
    input: String,
    dry_run: bool,
    link: Option<Utf8PathBuf>,
    _mode: super::output::OutputMode,
) -> Result<()> {
    // `_mode` is threaded per ADR-0017 / #144. Quiet-suppression of the
    // success line is tracked in #203. The dry-run plan envelope is
    // product output (the requested artifact) and is unaffected by
    // mode.
    // Step 1: parse + safekey. Issue #119: render the cargo-style
    // `error[INVALID_REF]:` line + carry the exit code, rather than
    // letting the granular `RefParseError` fall out as an opaque anyhow
    // `{:?}` dump. Through the shared helper (#492) so a change to the
    // wording or the code reaches every command at once — this and `graph`
    // were the last two hand-inlined copies of its body.
    let ref_ = super::parse_ref_or_exit(&input)?;

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

    // Step 4: dispatch on ref kind. `fetch_one` now returns the
    // typed `FetchPaperOutcome` / `FetchError` per #210; the
    // single-fetch caller (this fn) owns rendering + exit code.
    let result = harness.fetch_one(&ref_).await;

    // Step 5: emit SessionEnd regardless of outcome. A `Blocked` PDF
    // leg is NOT a clean success even though the typed `Result` is
    // `Ok` — `outcome_is_clean_success` collapses both halves so the
    // SessionEnd `is_ok` field matches the user-facing exit code.
    let session_ok = match &result {
        Ok(o) => outcome_is_clean_success(o),
        Err(_) => false,
    };
    // #507: the code the USER was given, which for this command is not
    // always the `Result`'s. A blocked PDF leg is `Ok` with a failed leg and
    // an unclean session, and the leg carries the closed-set code -- recording
    // `None` there would log the one outcome an agent is most likely to retry
    // as having no reason at all.
    let session_err = match &result {
        Err(e) => Some(doiget_core::ErrorCode::from(e).as_wire()),
        Ok(o) => match &o.pdf_leg {
            PdfLegStatus::Blocked { code, .. } => Some(code.as_wire()),
            _ => None,
        },
    };
    harness.log_session_end(session_ok, Some(ref_.as_input_str()), session_err);

    // Step 6: render the user-facing surface and map to `CliExit`.
    // The Blocked-PDF reclassification logic that used to live inside
    // `fetch_one` was lifted here verbatim so the batch caller can
    // share the same `effective_blocked_code` / `render_blocked_error`
    // helpers (issue #210 / #145).
    match result {
        Ok(outcome) => {
            if let PdfLegStatus::Blocked {
                code,
                message,
                denial,
                suggested_arxiv_id,
            } = &outcome.pdf_leg
            {
                let effective = effective_blocked_code(*code, denial.as_ref());
                render_blocked_error(
                    &ref_,
                    &outcome,
                    effective,
                    message,
                    denial.as_ref(),
                    suggested_arxiv_id.as_deref(),
                );
                return Err(anyhow::Error::new(CliExit(cli_exit_code(effective))));
            }
            emit_success_line(&ref_, &outcome);
            // #344 Slice 2: optionally surface the artifact in the user's
            // working tree via a symlink (copy fallback). A link failure is a
            // warning, not a fetch failure — the PDF is already in the store.
            if let Some(dir) = link.as_deref() {
                emit_link_result(&ref_, &outcome, dir);
            }
            Ok(())
        }
        Err(e) => {
            render_fetch_error(&e);
            let code: ErrorCode = (&e).into();
            Err(anyhow::Error::new(CliExit(cli_exit_code(code))))
        }
    }
}

/// `--link` (#344 Slice 2): place a link to the fetched PDF in `dir` so the
/// artifact is visible in the user's working tree. The central store stays the
/// single source of truth; this only adds a pointer (or, where symlinks are
/// unavailable, a copy). Only PDF outcomes are linked — a metadata-only fetch
/// is reported as skipped. A link failure is a warning (stderr), never a fetch
/// failure: the artifact is already in the store.
fn emit_link_result(ref_: &Ref, outcome: &FetchPaperOutcome, dir: &Utf8Path) {
    let label = match ref_ {
        Ref::Arxiv(id) => format!("arxiv:{}", id.as_str()),
        Ref::Doi(doi) => format!("doi:{}", doi.as_str()),
    };
    if !matches!(
        outcome.pdf_leg,
        PdfLegStatus::Fetched
            | PdfLegStatus::PreprintFallback { .. }
            | PdfLegStatus::TdmFetched { .. }
    ) {
        print_success(format_args!(
            "note: --link skipped for {label} (no PDF — metadata-only fetch)"
        ));
        return;
    }
    let name = fetch_link_filename(
        &outcome.title,
        &outcome.authors,
        outcome.year,
        &outcome.safekey,
    );
    match link_artifact(dir, &outcome.path, &name) {
        Ok((path, kind)) => print_success(format_args!("linked {label} -> {path} ({kind})")),
        Err(e) => print_err(format_args!("warning: --link failed for {label}: {e}")),
    }
}

/// Build a human-readable, filesystem-safe PDF filename for `--link`:
/// `<surname><year>-<title-slug>.pdf`
/// (e.g. `vaswani2017-attention-is-all-you-need.pdf`), falling back to
/// `<safekey>.pdf` when no usable metadata is available.
fn fetch_link_filename(
    title: &str,
    authors: &[String],
    year: Option<i32>,
    safekey: &str,
) -> String {
    let surname = authors
        .first()
        .map(|a| slugify(a.split_whitespace().last().unwrap_or(a)))
        .unwrap_or_default();
    let year = year.map(|y| y.to_string()).unwrap_or_default();
    let title_slug: String = slugify(title)
        .split('-')
        .take(6)
        .collect::<Vec<_>>()
        .join("-");
    let mut stem = format!("{surname}{year}");
    if !stem.is_empty() && !title_slug.is_empty() {
        stem.push('-');
    }
    stem.push_str(&title_slug);
    let stem: String = stem.chars().take(80).collect();
    let stem = stem.trim_matches('-');
    if stem.is_empty() {
        format!("{safekey}.pdf")
    } else {
        format!("{stem}.pdf")
    }
}

/// Lowercase ASCII-alphanumeric slug: every run of non-alphanumeric characters
/// collapses to a single `-`, with no leading/trailing dashes. Pure and
/// filesystem-safe (no path separators, no `..`).
fn slugify(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Place a link to `src` (the store PDF) at `dir/name`. Tries a symlink first;
/// on failure (e.g. Windows without privilege, or a cross-device link) falls
/// back to a copy. Replaces a prior doiget symlink, but refuses to clobber an
/// unrelated regular file. Returns the written path and the mechanism used
/// (`"symlink"` | `"copy"`).
///
/// The symlink-vs-file check and the subsequent replace are not atomic: a
/// concurrent process swapping the entry between the two syscalls is an
/// accepted, out-of-scope race — the `--link` dir is the user's own working
/// directory, assumed single-writer (review #352).
fn link_artifact(
    dir: &Utf8Path,
    src: &Utf8Path,
    name: &str,
) -> Result<(Utf8PathBuf, &'static str)> {
    std::fs::create_dir_all(dir.as_std_path())
        .with_context(|| format!("creating link dir {dir}"))?;
    let dst = dir.join(name);
    if let Ok(meta) = std::fs::symlink_metadata(dst.as_std_path()) {
        if meta.file_type().is_symlink() {
            std::fs::remove_file(dst.as_std_path())
                .with_context(|| format!("replacing existing symlink {dst}"))?;
        } else {
            anyhow::bail!(
                "refusing to overwrite existing file {dst} (not a doiget symlink) — \
                 remove it or choose another --link dir"
            );
        }
    }
    match make_symlink(src, &dst) {
        Ok(()) => Ok((dst, "symlink")),
        Err(_) => {
            std::fs::copy(src.as_std_path(), dst.as_std_path())
                .with_context(|| format!("copying {src} -> {dst}"))?;
            Ok((dst, "copy"))
        }
    }
}

/// Cross-platform file symlink. On platforms without symlink support the caller
/// falls back to a copy.
#[cfg(unix)]
fn make_symlink(src: &Utf8Path, dst: &Utf8Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(src.as_std_path(), dst.as_std_path())
}

#[cfg(windows)]
fn make_symlink(src: &Utf8Path, dst: &Utf8Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(src.as_std_path(), dst.as_std_path())
}

#[cfg(not(any(unix, windows)))]
fn make_symlink(_src: &Utf8Path, _dst: &Utf8Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "symlinks unsupported on this platform",
    ))
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

/// Reclassify a `PdfLegStatus::Blocked` code at the CLI layer (issue
/// #145 / `docs/ERRORS.md` §2 "NETWORK_ERROR" vs §3.1 / §6).
///
/// The core maps *every* `FetchError::Http(_)` to
/// [`ErrorCode::NetworkError`] (`doiget_core::source`'s
/// `From<&FetchError> for ErrorCode`). `docs/ERRORS.md` §2 defines
/// `NETWORK_ERROR` as a transport / DNS / TLS fault where "retry usually
/// fine" — true for a real network blip, but **false** for a deliberate
/// supply-chain policy block (off-allowlist redirect, insecure-scheme
/// redirect, host-blocklist hit): retrying such a block never helps, so
/// surfacing it as `NETWORK_ERROR` (generic exit 1) misrepresents a flaky
/// network to humans and agents.
///
/// The orchestrator already preserves the true reason on the
/// [`DenialContext`] side-channel (the `From<&HttpError> for
/// Option<DenialContext>` impl walks reqwest's `source()` chain, so even
/// a redirect denial wrapped as `HttpError::Network` still yields
/// [`DenialReason::RedirectNotInAllowlist`]). When that reason is one of
/// the closed-set *policy* denials, promote the surface code to
/// [`ErrorCode::CapabilityDenied`] so the CLI renders
/// `error[CAPABILITY_DENIED]:` and [`cli_exit_code`] returns exit 3 —
/// the same code `fetch` / `graph` already use for capability denials.
/// Non-policy blocks (no `denial`, or a non-policy reason such as
/// `SizeCapExceeded` / `ContentTypeMismatch`) keep the core's code so a
/// genuine transport failure still reads as `NETWORK_ERROR`.
pub(crate) fn effective_blocked_code(code: ErrorCode, denial: Option<&DenialContext>) -> ErrorCode {
    match denial.map(|d| d.reason) {
        Some(
            DenialReason::RedirectNotInAllowlist
            | DenialReason::InsecureScheme
            | DenialReason::HostInBlockList,
        ) => ErrorCode::CapabilityDenied,
        _ => code,
    }
}

/// Snake-case wire token for a [`DenialReason`], matching the
/// `#[serde(rename_all = "snake_case")]` JSON/MCP surface (ADR-0023 §2)
/// so the CLI human line uses the SAME vocabulary as the machine
/// envelope (`docs/ERRORS.md` §3.1). Only the policy-denial reasons the
/// CLI inlines are enumerated; everything else degrades to a generic
/// token rather than drifting from the serde form.
fn denial_reason_wire(reason: DenialReason) -> &'static str {
    match reason {
        DenialReason::RedirectNotInAllowlist => "redirect_not_in_allowlist",
        DenialReason::InsecureScheme => "insecure_scheme",
        DenialReason::HostInBlockList => "host_in_block_list",
        _ => "policy_denied",
    }
}

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
        // A name filter that matched several entities is user-fixable by
        // narrowing the query → `docs/ERRORS.md` §4 exit 2 ("misuse").
        ErrorCode::Ambiguous => 2,
        // An unparsable ref is a bad argument, and §4's exit 1 is "at
        // least one fetch failed" — which does not describe a run where
        // nothing was fetched. `graph` had followed the table with a
        // hard-coded 2 while `fetch` and the eight #477 commands fell to
        // the `_ => 1` arm below, so the same input produced different
        // exit codes from the same binary (#492, ADR-0049).
        ErrorCode::InvalidRef => 2,
        _ => 1,
    }
}

// `widening_suggestions` / `looks_like_public_suffix` moved to
// `doiget_core::remediation` in #459 so the MCP and `batch --json`
// surfaces render the same suggestions this block does, rather than a
// second implementation of them. #454 is the recent lesson about two
// surfaces each keeping their own copy of a rule.

/// Build the ADR-0023 `denial_context` advisory lines shared by
/// [`render_fetch_error`] and [`render_blocked_error`]: the `= note:`
/// naming what was attempted and what the allowlist held, plus — for
/// `redirect_not_in_allowlist` — a `= help:` block naming the config file
/// and the two keys that widen the allowlist.
///
/// Issue #405: the note on its own reads as "this host is forbidden", when
/// what actually happened is "you have not enabled the class it belongs
/// to". `trust_academic_repos` and `[[network.additional_hosts]]` are the
/// supported fixes, so the denial names them instead of leaving the user to
/// find them in `CHANGELOG.md`.
///
/// Pure (returns the lines rather than printing them) so the wording is
/// unit-testable without capturing process stderr; `config_path` is passed
/// in for the same reason. `None` means the platform has no config dir, in
/// which case the file is named generically — a missing config dir must
/// never turn an advisory line into a hard error.
fn denial_note_lines(dc: &DenialContext, config_path: Option<&camino::Utf8Path>) -> Vec<String> {
    let attempted = dc.attempted.as_deref().unwrap_or("(unknown)");
    let mut out = vec![match &dc.expected {
        Some(exp) if !exp.is_empty() => {
            format!(
                "  = note: attempted {attempted}; allowed: {}",
                exp.join(", ")
            )
        }
        _ => format!("  = note: attempted {attempted}"),
    }];
    if dc.reason != DenialReason::RedirectNotInAllowlist {
        return out;
    }
    let where_ = config_path.map_or_else(
        || "your doiget config.toml".to_string(),
        |p| p.as_str().to_string(),
    );
    out.push(format!(
        "  = help: that host is not on the allowlist yet; widen it in {where_}"
    ));
    // #478: name the ONE flag that covers this host, not both.
    //
    // `trust_flag_for_host` already computes it, and
    // `remediation::for_denial` calls it -- so MCP and `batch --json`
    // consumers were getting the precise answer while the human was shown
    // two flags with nothing to choose between them, and following the
    // wrong one cost a round. The human has less context than the agent,
    // not more.
    //
    // `None` means neither flag covers the host (a genuine publisher). The
    // machine path offers no flag there; so does this one now, rather than
    // suggesting two settings that cannot possibly help.
    //
    // Three cases, and "we did not compute it" is not the same answer as
    // "we computed it and neither applies":
    match dc.attempted.as_deref() {
        // Known host, one flag covers it. Name that one.
        Some(h) => match doiget_core::remediation::trust_flag_for_host(h) {
            Some((flag, pattern, note)) => out.push(format!(
                "          [network] {flag} = true   # covers {pattern} ({note})"
            )),
            // Known host, neither flag covers it -- a genuine publisher.
            // The machine path offers no flag here (there is a test for
            // it: `a_publisher_host_offers_no_trust_flag`), so neither
            // does this one. Suggesting two settings that cannot help is
            // worse than saying so.
            None => out.push(
                "          # neither trust_academic_repos nor trust_oa_registries covers this host"
                    .to_string(),
            ),
        },
        // No host to test. Both flags stay listed, because the reason for
        // narrowing is absent rather than resolved.
        None => {
            out.push(
                "          [network] trust_academic_repos = true   # 15 curated academic suffixes"
                    .to_string(),
            );
            out.push(
                "          [network] trust_oa_registries  = true   # DOAJ, SciELO, Zenodo, OSF, HAL"
                    .to_string(),
            );
        }
    }
    if dc.attempted.is_some() {
        for (pattern, why) in doiget_core::remediation::widening_suggestions(attempted) {
            out.push(format!(
                "          [[network.additional_hosts]] host = \"{pattern}\"   # {why}"
            ));
        }
    }
    out.push("          see docs/CONFIG.md §3.1 for both".to_string());
    out
}

/// Print the [`denial_note_lines`] advisory block on stderr.
fn print_denial_notes(dc: &DenialContext) {
    for line in denial_note_lines(dc, super::user_config_path().as_deref()) {
        print_err(format_args!("{line}"));
    }
}

/// Render a terminal [`FetchError`] in the `docs/ERRORS.md` §3
/// "Researcher (CLI human)" form: `error[CODE]: message` on stderr,
/// plus an actionable `= note:` line carrying the ADR-0023
/// `denial_context` (attempted / expected hosts) when the failure was
/// a denial class. stdout stays clean (ADR-0001).
///
/// `pub(crate)` so sibling resolve commands (`commands::link`, …) render
/// typed failures — including the actionable denial note — through the
/// SAME path instead of open-coding `error[CODE]: msg` and dropping the
/// `denial_context` note (review #287).
pub(crate) fn render_fetch_error(e: &FetchError) {
    let code: ErrorCode = e.into();
    print_err(format_args!("error[{}]: {}", code.as_wire(), e));
    if let Some(dc) = Option::<DenialContext>::from(e) {
        print_denial_notes(&dc);
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
    suggested_arxiv_id: Option<&str>,
) {
    let label = match ref_ {
        Ref::Arxiv(id) => format!("arxiv:{}", id.as_str()),
        Ref::Doi(doi) => format!("doi:{}", doi.as_str()),
    };
    // Issue #145: when the block is a deliberate policy denial, name the
    // closed-set reason inline so a human/agent reading the
    // `error[CAPABILITY_DENIED]:` line immediately sees this is a
    // supply-chain policy block (retrying is futile), not a flaky network.
    match denial.map(|d| d.reason) {
        Some(
            reason @ (DenialReason::RedirectNotInAllowlist
            | DenialReason::InsecureScheme
            | DenialReason::HostInBlockList),
        ) => {
            print_err(format_args!(
                "error[{}]: {label}: an OA PDF was found but its host is blocked by \
                 supply-chain policy ({}): {message}",
                code.as_wire(),
                denial_reason_wire(reason)
            ));
        }
        _ => {
            print_err(format_args!(
                "error[{}]: {label}: an OA PDF was found but could not be retrieved: {message}",
                code.as_wire()
            ));
        }
    }
    if let Some(dc) = denial {
        print_denial_notes(dc);
    }
    // The metadata TOML still landed; point the user at it so the
    // partial result is not lost (it is still useful), without
    // pretending the fetch succeeded.
    print_err(format_args!(
        "  = note: metadata-only record written to {}",
        outcome.path
    ));
    if let Some(arxiv_id) = suggested_arxiv_id {
        print_err(format_args!(
            "  = suggest: Try fetching the arXiv version: doiget fetch arxiv:{}",
            arxiv_id
        ));
    }
    for line in blocked_trace_lines(&outcome.attempts, message) {
        print_err(format_args!("{line}"));
    }
}

/// The diagnostics for a found-nothing fetch (#505).
///
/// `no OA PDF available` means only "the sources that ran had nothing". With
/// the default profile that is three of eleven, and the sentence does not say
/// so -- #413 built the trace for exactly this distinction ("we asked and it
/// had nothing" versus "we never asked") and this was the path it never
/// reached.
///
/// Three blocks: what ran, what did not, and the line to paste.
/// Order the sources that were NOT consulted, for the found-nothing path
/// (#505 part 3).
///
/// The issue is explicit about the risk, and it governs this whole function:
///
/// > a ranking that is wrong is worse than no ranking, because it makes people
/// > stop early. So it must be an *ordering* of the full list, never a
/// > shortlist, and it must name the signal it ranked on.
///
/// Two positions have a real signal and the middle does not, so only two are
/// ranked:
///
/// * **`openalex` first.** It is categorically different from the rest: it
///   *lists* every location a work has, so with it enabled the answer is "this
///   repository has it", not "this repository might". Item 1 is a lookup; the
///   others are guesses, and the issue is emphatic that presenting both in one
///   list without saying which is which is the failure mode it is about.
/// * **`core` last.** Not a guess either -- its own module doc calls it "the
///   broadest single OA index outside Unpaywall and therefore the LAST fallback
///   in the chain". Broadest means least discriminating, so it is never the
///   first thing to try and never absent from the list.
///
/// **Everything between them is returned unordered, deliberately.** The issue
/// proposes ranking the middle on venue, author affiliation and funder, and
/// none of those reach this point: `FetchPaperOutcome` carries `title`,
/// `authors` and `year`, and the DOI prefix map is Tier-3-only (ADR-0041,
/// publisher TDM scoping) and absent from an `oa-only` build entirely. Putting
/// them in an order anyway would render a guess in the shape of a finding,
/// which is the one thing this must not do.
fn rank_unconsulted(
    attempts: &[SourceAttempt],
) -> (Vec<&'static str>, Vec<&'static str>, Vec<&'static str>) {
    let mut first = Vec::new();
    let mut middle = Vec::new();
    let mut last = Vec::new();
    for a in attempts {
        if a.outcome.required_env().is_none() {
            continue;
        }
        match a.source {
            "openalex" => first.push(a.source),
            "core" => last.push(a.source),
            other => middle.push(other),
        }
    }
    middle.sort_unstable();
    (first, middle, last)
}

fn not_found_trace_lines(ref_: &Ref, attempts: &[SourceAttempt]) -> Vec<String> {
    let mut out = Vec::new();
    if attempts.is_empty() {
        return out;
    }

    out.push("  = note: no OA copy found. sources this run:".to_string());
    out.extend(
        doiget_core::orchestrator::render_attempts(attempts)
            .lines()
            .map(|l| format!("  {l}")),
    );

    // Split the widening advice by whether it can actually be acted on.
    //
    // `resolve_metadata_flag` returns false when the variable IS set but the
    // Cargo feature was not compiled in -- it warns through `tracing` and
    // moves on, so the source reports `Disabled` naming a variable the user
    // has already set. Printing "set DOIGET_ENABLE_X" at someone who set it
    // an hour ago is the same species of unhelpful as the bare
    // `no OA PDF available` this issue is about, so say which case it is.
    let (unset, already_set): (Vec<_>, Vec<_>) = doiget_core::orchestrator::widening_env(attempts)
        .into_iter()
        .partition(|v| std::env::var_os(v).is_none());

    if !unset.is_empty() {
        // `widening_env` returns Tier-2 switches AND Tier-3 credential pairs.
        // Rendering every one as `VAR=1` produced `DOIGET_KEY_APS=1` -- an API
        // key that can never be valid, in a line whose whole purpose is to be
        // pasted. A flag is a flag; a key is a key.
        let assignments = unset
            .iter()
            .map(|v| {
                if v.starts_with("DOIGET_KEY_") {
                    format!("{v}=<your-api-key>")
                } else {
                    format!("{v}=1")
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        let target = match ref_ {
            Ref::Arxiv(id) => id.as_str().to_string(),
            Ref::Doi(doi) => doi.as_str().to_string(),
        };
        out.push("  = suggest: to widen the search:".to_string());
        out.push(format!("      {assignments} doiget fetch {target}"));
    }

    if !already_set.is_empty() {
        out.push(format!(
            "  = note: {} already set, but the source is still off -- this binary was built without the Cargo feature that provides it. Widening needs a differently-built binary, not another variable.",
            already_set.join(", ")
        ));
    }

    // #505 part 3. Ordered only where there is something to order on; see
    // `rank_unconsulted`.
    let (first, middle, last) = rank_unconsulted(attempts);
    if !first.is_empty() || !middle.is_empty() || !last.is_empty() {
        out.push("  = note: of the sources not consulted:".to_string());
        for s in &first {
            out.push(format!(
                "      1. {s:<12} lists every location a work has -- a lookup, not a guess"
            ));
        }
        if !middle.is_empty() {
            out.push(format!(
                "      then, in NO particular order: {}",
                middle.join("  ")
            ));
        }
        for s in &last {
            out.push(format!(
                "      last: {s:<9} the broadest index outside Unpaywall, so never the first try"
            ));
        }
        // Naming the signal is half of what the ranking is for. Saying "the
        // middle has none" is the honest form of that, and it stops the list
        // reading as an ordering it is not.
        if !middle.is_empty() {
            out.push(
                "  = note: the middle is unordered because nothing in this run distinguishes those sources -- venue, affiliation and funder would, and none of them reach here. An invented order would read as information."
                    .to_string(),
            );
        }
    }

    out
}

/// The `= note:`/`= suggest:` block appended to a blocked PDF leg (#445).
///
/// #413 attached the resolution trace to `NotFound` only. But "found
/// nowhere" and "found at one host that refused me" raise the same next
/// question — *did anything else have it?* — and only the first one got an
/// answer. A user with five optional sources enabled saw a bare 429 and no
/// indication that none of the five had been consulted.
///
/// Pure so the wording is asserted rather than assumed.
fn blocked_trace_lines(attempts: &[SourceAttempt], message: &str) -> Vec<String> {
    let mut out = Vec::new();
    // A rate limit is the one failure where retrying the same host later is
    // right and reconfiguring is wrong. The bare text reads like a
    // permanent block, so say which it is.
    if message.contains("429") {
        out.push(
            "  = suggest: HTTP 429 is a rate limit, not a policy block — it is transient. Retry \
                later, and set DOIGET_CONTACT_EMAIL for the polite pool."
                .to_string(),
        );
    }
    if attempts.is_empty() {
        return out;
    }
    let lead = if doiget_core::orchestrator::nothing_was_consulted(attempts) {
        "no other source was consulted for this DOI"
    } else {
        "the other sources were consulted and offered no alternative copy"
    };
    out.push(format!("  = note: {lead}:"));
    out.extend(
        doiget_core::orchestrator::render_attempts(attempts)
            .lines()
            .map(|l| format!("  {l}")),
    );
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// Save an env var and restore it on drop.
    ///
    /// Both client-builder tests below have to clear the
    /// `DOIGET_*_BASE` overrides to reach the production branch, and
    /// leaving one cleared would silently reroute an unrelated test.
    struct EnvGuard {
        key: &'static str,
        prev: Option<String>,
    }
    impl EnvGuard {
        fn save(key: &'static str) -> Self {
            Self {
                key,
                prev: std::env::var(key).ok(),
            }
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    /// #454: the guard the list-level one in `http.rs` cannot be.
    ///
    /// `every_tier_3_source_has_a_transport_allowlist_entry` asserts that
    /// `tier_3_aps_allowlist()` *contains* `"tdm-aps"`. It says nothing
    /// about whether anything hands that list to a client, and for three
    /// releases nothing did — so the assertion passed while a production
    /// fetch returned `UnknownSource { source_key: "tdm-aps" }`.
    ///
    /// This asserts the object the fetch actually goes through. It cannot
    /// pass for the reason that one did, because there is no list here to
    /// be right about in isolation.
    #[test]
    #[serial]
    #[cfg(any(
        feature = "tdm-aps",
        feature = "tdm-elsevier",
        feature = "tdm-springer",
        feature = "tdm-ieee"
    ))]
    #[allow(clippy::vec_init_then_push)]
    fn the_production_client_registers_every_tier_3_source_key() {
        // Every base override must be clear or `build_http_client` takes
        // the test-mode branch, which registers whatever it is given and
        // would prove nothing.
        let _g: Vec<EnvGuard> = [
            "DOIGET_ARXIV_BASE",
            "DOIGET_CROSSREF_BASE",
            "DOIGET_UNPAYWALL_BASE",
            "DOIGET_OA_PUBLISHER_BASE",
            "DOIGET_OPENALEX_BASE",
            "DOIGET_AR5IV_BASE",
        ]
        .iter()
        .map(|k| {
            let g = EnvGuard::save(k);
            std::env::remove_var(k);
            g
        })
        .collect();

        let client = build_http_client(None).expect("production client builds");

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
                "the production client has no allowlist for `{key}`; the orchestrator \
                 reaches this source and the fetch would die at UnknownSource (#454)"
            );
        }
    }

    /// #516: the Tier-2 half of the same guard, and the reason it is
    /// needed is that the Tier-3 lesson was not applied one tier up.
    ///
    /// `every_tier_2_source_has_a_transport_allowlist_entry` (in
    /// `http.rs`) asserts that `tier_2_allowlist()` *contains* every
    /// source key. That stayed true while the extend into the production
    /// client was gated on `citation` rather than `metadata`, so in a
    /// `--features metadata` build — which CI's clippy matrix builds
    /// explicitly — `resolve_optional_chain` ran, `can_serve` passed,
    /// and the request died at `UnknownSource`.
    ///
    /// This asserts the object the fetch goes through, and it enumerates
    /// from `tier_2_allowlist()` rather than a literal so a new source
    /// cannot be added to the list and missed here.
    #[test]
    #[serial]
    #[cfg(feature = "metadata")]
    fn the_production_client_registers_every_tier_2_source_key() {
        // Every base override must be clear or `build_http_client` takes
        // the test-mode branch, which registers whatever it is given and
        // would prove nothing.
        let _g: Vec<EnvGuard> = [
            "DOIGET_ARXIV_BASE",
            "DOIGET_CROSSREF_BASE",
            "DOIGET_UNPAYWALL_BASE",
            "DOIGET_OA_PUBLISHER_BASE",
            "DOIGET_OPENALEX_BASE",
            "DOIGET_AR5IV_BASE",
        ]
        .iter()
        .map(|k| {
            let g = EnvGuard::save(k);
            std::env::remove_var(k);
            g
        })
        .collect();

        let client = build_http_client(None).expect("production client builds");

        // Fully qualified on purpose: the `use` at the top of this file is
        // itself `#[cfg(feature = "metadata")]`, so importing it here would
        // turn a regression into a compile error in this file rather than the
        // assertion failure that names the actual defect.
        let keys: Vec<String> = doiget_core::http::tier_2_allowlist()
            .iter()
            .map(|a| a.source.clone())
            .collect();
        assert!(!keys.is_empty(), "the guard must have checked something");
        for key in keys {
            assert!(
                client.source_allowlist(&key).is_some(),
                "the production client has no allowlist for `{key}`; \n                 `resolve_optional_chain` reaches this source in a \n                 `metadata` build and the fetch would die at \n                 UnknownSource (#516)"
            );
        }
    }

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

    /// Review pass C2: end-to-end coverage of the user-extension
    /// merge inside `build_http_client`. Without this test the
    /// production path that turns a `config.toml`
    /// `[[network.additional_hosts]]` entry into a passing
    /// allowlist match is unexercised — every existing e2e sets
    /// `DOIGET_*_BASE` and short-circuits into the test-mode
    /// builder above.
    #[test]
    #[serial]
    fn build_http_client_merges_user_extension_into_oa_publisher_allowlist() {
        use std::io::Write;

        // Construct a tempdir + minimal config.toml under it.
        let td = tempfile::TempDir::new().expect("tempdir");
        let cfg_dir = td.path().join("doiget");
        std::fs::create_dir_all(&cfg_dir).expect("mkdir doiget/");
        let cfg_path = cfg_dir.join("config.toml");
        let mut f = std::fs::File::create(&cfg_path).expect("create config.toml");
        f.write_all(
            br#"
[[network.additional_hosts]]
host = "ruj.uj.edu.pl"
note = "Jagiellonian"

[[network.additional_hosts]]
host = "*.uj.edu.pl"
"#,
        )
        .expect("write config.toml");
        drop(f);

        // Save + override env so `config_dir_utf8()` lands on the
        // tempdir. Restored on Drop by `EnvGuard` (module-level since
        // #454, which needed the same save/restore). We also clear the
        // five `DOIGET_*_BASE` env vars to force the production
        // branch of `build_http_client`.
        let _g0 = EnvGuard::save("XDG_CONFIG_HOME");
        let _g1 = EnvGuard::save("APPDATA");
        let _g2 = EnvGuard::save("HOME");
        let _g3 = EnvGuard::save("USERPROFILE");
        let _g4 = EnvGuard::save("DOIGET_ARXIV_BASE");
        let _g5 = EnvGuard::save("DOIGET_CROSSREF_BASE");
        let _g6 = EnvGuard::save("DOIGET_UNPAYWALL_BASE");
        let _g7 = EnvGuard::save("DOIGET_OA_PUBLISHER_BASE");
        let _g8 = EnvGuard::save("DOIGET_OPENALEX_BASE");
        std::env::set_var("XDG_CONFIG_HOME", td.path());
        std::env::set_var("APPDATA", td.path());
        std::env::set_var("HOME", td.path());
        std::env::set_var("USERPROFILE", td.path());
        std::env::remove_var("DOIGET_ARXIV_BASE");
        std::env::remove_var("DOIGET_CROSSREF_BASE");
        std::env::remove_var("DOIGET_UNPAYWALL_BASE");
        std::env::remove_var("DOIGET_OA_PUBLISHER_BASE");
        std::env::remove_var("DOIGET_OPENALEX_BASE");

        let client = build_http_client(None).expect("HttpClient builds");
        let oa = client
            .source_allowlist("oa-publisher")
            .expect("oa-publisher source registered");

        // Pre-existing curated allowlist still effective.
        assert!(
            oa.redirect_hosts.iter().any(|p| p == "*.aps.org"),
            "curated *.aps.org MUST still be present after merge; got {:?}",
            oa.redirect_hosts
        );
        // User-added literal host passes match.
        assert!(
            oa.matches("ruj.uj.edu.pl"),
            "literal `ruj.uj.edu.pl` from user config MUST match"
        );
        // User-added wildcard passes match for a subdomain.
        assert!(
            oa.matches("alpha.uj.edu.pl"),
            "wildcard `*.uj.edu.pl` from user config MUST match alpha.uj.edu.pl"
        );
        // Unrelated host MUST still fail.
        assert!(
            !oa.matches("ruj.uj.edu.ru"),
            "host outside the suffix MUST NOT match"
        );
    }

    /// Issue #405: `[network] trust_oa_registries = true` MUST widen the
    /// production `oa-publisher` allowlist through the same
    /// `build_http_client` path a real fetch takes — the flag is worthless
    /// if it only sets a struct field. Pinned on the exact host that
    /// denied the reported Gold-OA fetch (`doaj.org`, an apex, which a
    /// single-suffix wildcard would NOT cover), and on the academic flag
    /// staying off so the two sets cannot silently imply each other.
    #[test]
    #[serial]
    fn build_http_client_merges_oa_registries_when_flag_is_set() {
        use std::io::Write;

        let td = tempfile::TempDir::new().expect("tempdir");
        let cfg_dir = td.path().join("doiget");
        std::fs::create_dir_all(&cfg_dir).expect("mkdir doiget/");
        let mut f = std::fs::File::create(cfg_dir.join("config.toml")).expect("create config");
        f.write_all(b"[network]\ntrust_oa_registries = true\n")
            .expect("write config.toml");
        drop(f);

        struct EnvGuard {
            key: &'static str,
            prev: Option<String>,
        }
        impl EnvGuard {
            fn save(key: &'static str) -> Self {
                Self {
                    key,
                    prev: std::env::var(key).ok(),
                }
            }
        }
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                match &self.prev {
                    Some(v) => std::env::set_var(self.key, v),
                    None => std::env::remove_var(self.key),
                }
            }
        }
        let _g: Vec<EnvGuard> = [
            "XDG_CONFIG_HOME",
            "APPDATA",
            "HOME",
            "USERPROFILE",
            "DOIGET_ARXIV_BASE",
            "DOIGET_CROSSREF_BASE",
            "DOIGET_UNPAYWALL_BASE",
            "DOIGET_OA_PUBLISHER_BASE",
            "DOIGET_OPENALEX_BASE",
        ]
        .iter()
        .map(|k| EnvGuard::save(k))
        .collect();
        for k in ["XDG_CONFIG_HOME", "APPDATA", "HOME", "USERPROFILE"] {
            std::env::set_var(k, td.path());
        }
        for k in [
            "DOIGET_ARXIV_BASE",
            "DOIGET_CROSSREF_BASE",
            "DOIGET_UNPAYWALL_BASE",
            "DOIGET_OA_PUBLISHER_BASE",
            "DOIGET_OPENALEX_BASE",
        ] {
            std::env::remove_var(k);
        }

        let client = build_http_client(None).expect("HttpClient builds");
        let oa = client
            .source_allowlist("oa-publisher")
            .expect("oa-publisher source registered");

        // ADR-0037: DOAJ is a DEFAULT allowlist entry now, so it is not
        // evidence that the flag worked. Assert on a host only the flag can
        // provide.
        assert!(
            oa.matches("zenodo.org"),
            "the zenodo apex must match with the flag set; got {:?}",
            oa.redirect_hosts
        );
        assert!(oa.matches("data.zenodo.org"), "wildcard covers subdomains");
        assert!(oa.matches("hal.science"), "hal apex must match");
        assert!(
            oa.redirect_hosts.iter().any(|p| p == "*.aps.org"),
            "the curated allowlist MUST survive the merge"
        );
        // The academic flag was NOT set, so its set must NOT be merged —
        // otherwise one flag silently grants what the other advertises.
        assert!(
            !oa.matches("strathprints.strath.ac.uk"),
            "trust_oa_registries MUST NOT imply trust_academic_repos"
        );
        assert!(
            !oa.matches("evil.example.com"),
            "unrelated host still denied"
        );
    }

    /// ADR-0031 D2: discovery search (`doiget search`) ships in the default
    /// `oa-only` binary, so `api.openalex.org` MUST be on the production
    /// allowlist under the `"openalex"` source key WITHOUT `--features
    /// metadata`. The Tier-2 `tier_2_allowlist()` extend is
    /// `#[cfg(feature = "metadata")]` (#516 moved it off `citation`);
    /// this test proves `discovery_allowlist()` covers that gap in the
    /// shipped `oa-only` build, where neither feature is compiled.
    #[test]
    #[serial]
    fn build_http_client_registers_openalex_for_discovery() {
        struct EnvGuard {
            key: &'static str,
            prev: Option<String>,
        }
        impl EnvGuard {
            fn save(key: &'static str) -> Self {
                Self {
                    key,
                    prev: std::env::var(key).ok(),
                }
            }
        }
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                match &self.prev {
                    Some(v) => std::env::set_var(self.key, v),
                    None => std::env::remove_var(self.key),
                }
            }
        }

        // Point config resolution at an empty tempdir and clear every
        // `DOIGET_*_BASE` so `build_http_client` takes the PRODUCTION
        // branch (not the test-base builder, which would register
        // "openalex" itself and mask the gap this test guards).
        let td = tempfile::TempDir::new().expect("tempdir");
        let _g0 = EnvGuard::save("XDG_CONFIG_HOME");
        let _g1 = EnvGuard::save("APPDATA");
        let _g2 = EnvGuard::save("HOME");
        let _g3 = EnvGuard::save("USERPROFILE");
        let _g4 = EnvGuard::save("DOIGET_ARXIV_BASE");
        let _g5 = EnvGuard::save("DOIGET_CROSSREF_BASE");
        let _g6 = EnvGuard::save("DOIGET_UNPAYWALL_BASE");
        let _g7 = EnvGuard::save("DOIGET_OA_PUBLISHER_BASE");
        let _g8 = EnvGuard::save("DOIGET_OPENALEX_BASE");
        std::env::set_var("XDG_CONFIG_HOME", td.path());
        std::env::set_var("APPDATA", td.path());
        std::env::set_var("HOME", td.path());
        std::env::set_var("USERPROFILE", td.path());
        std::env::remove_var("DOIGET_ARXIV_BASE");
        std::env::remove_var("DOIGET_CROSSREF_BASE");
        std::env::remove_var("DOIGET_UNPAYWALL_BASE");
        std::env::remove_var("DOIGET_OA_PUBLISHER_BASE");
        std::env::remove_var("DOIGET_OPENALEX_BASE");

        let client = build_http_client(None).expect("HttpClient builds");
        let oa = client
            .source_allowlist("openalex")
            .expect("openalex source registered for discovery (ADR-0031 D2)");
        assert!(
            oa.matches("api.openalex.org"),
            "api.openalex.org MUST be on the discovery allowlist; got {:?}",
            oa.redirect_hosts
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

    #[test]
    fn ambiguous_maps_to_exit_code_2() {
        // ADR-0031 D5: a name-filter ambiguity is user-fixable → exit 2,
        // distinct from the generic exit 1.
        assert_eq!(cli_exit_code(ErrorCode::Ambiguous), 2);
    }

    #[test]
    fn invalid_ref_maps_to_exit_code_2() {
        // ADR-0049: an unparsable ref is misuse. `docs/ERRORS.md` §4
        // reserves 1 for "at least one fetch was attempted and failed",
        // and nothing is fetched here. `Ambiguous` — a value that fails
        // to select one entity — was already 2; `InvalidRef` is a value
        // that fails to parse, and sat at the catch-all 1 next to it.
        assert_eq!(cli_exit_code(ErrorCode::InvalidRef), 2);
    }

    /// Minimal `DenialContext` carrying only `reason`; every other field
    /// is optional (ADR-0023 §3) so `None`/empty is a valid producer
    /// shape for the reclassification decision under test.
    fn denial(reason: DenialReason) -> DenialContext {
        DenialContext {
            reason,
            source: None,
            attempted: None,
            expected: None,
            hop_index: None,
            cap: None,
            actual: None,
        }
    }

    /// Issue #145 / `docs/ERRORS.md` §6.1: a policy-class denial reason
    /// on a `Blocked` OA-PDF leg must be reclassified from the core's
    /// blanket `NetworkError` to `CapabilityDenied` at the CLI layer, so
    /// the user-facing exit becomes 3 (not the generic 1) and a flaky
    /// network is not implied for a deliberate supply-chain block.
    #[test]
    fn policy_denials_reclassify_network_error_to_capability_denied() {
        for r in [
            DenialReason::RedirectNotInAllowlist,
            DenialReason::InsecureScheme,
            DenialReason::HostInBlockList,
        ] {
            let d = denial(r);
            assert_eq!(
                effective_blocked_code(ErrorCode::NetworkError, Some(&d)),
                ErrorCode::CapabilityDenied,
                "policy reason {r:?} must promote NetworkError -> CapabilityDenied"
            );
            assert_eq!(
                cli_exit_code(effective_blocked_code(ErrorCode::NetworkError, Some(&d))),
                3,
                "policy reason {r:?} must map to exit 3 (docs/ERRORS.md §4/§6.1)"
            );
        }
    }

    /// A genuine transport fault carries NO `DenialContext`; it must stay
    /// `NetworkError` / exit 1 — `docs/ERRORS.md` §2 "retry usually fine"
    /// is the correct signal there. (This is exactly the e2e
    /// `..._host_off_allowlist` path: first-leg connect failure, no
    /// redirect hop, so no allowlist denial is produced.)
    #[test]
    fn absent_denial_context_keeps_network_error() {
        assert_eq!(
            effective_blocked_code(ErrorCode::NetworkError, None),
            ErrorCode::NetworkError
        );
        assert_eq!(
            cli_exit_code(effective_blocked_code(ErrorCode::NetworkError, None)),
            1
        );
    }

    /// Non-policy denial reasons (size cap, content-type mismatch) are
    /// NOT supply-chain policy blocks; they keep the core's code so a
    /// genuine cap/transport class is not masked as a capability denial.
    #[test]
    fn non_policy_denials_keep_core_code() {
        for r in [
            DenialReason::SizeCapExceeded,
            DenialReason::ContentTypeMismatch,
        ] {
            let d = denial(r);
            assert_eq!(
                effective_blocked_code(ErrorCode::NetworkError, Some(&d)),
                ErrorCode::NetworkError,
                "non-policy reason {r:?} must NOT be reclassified"
            );
        }
    }

    /// The closed-set wire token used in the human `error[...]:` line
    /// must match the serde `snake_case` form so the CLI vocabulary does
    /// not drift from the JSON/MCP envelope (`docs/ERRORS.md` §3.1).
    #[test]
    fn denial_reason_wire_matches_serde_snake_case() {
        for r in [
            DenialReason::RedirectNotInAllowlist,
            DenialReason::InsecureScheme,
            DenialReason::HostInBlockList,
        ] {
            let serde_form = serde_json::to_string(&r).expect("serialize DenialReason");
            // serde_json wraps the enum unit variant in quotes.
            let serde_token = serde_form.trim_matches('"');
            assert_eq!(
                denial_reason_wire(r),
                serde_token,
                "CLI wire token for {r:?} must equal the serde snake_case form"
            );
        }
    }

    /// The `= help:` line names a file for the user to edit, so it MUST be
    /// the file `build_http_client` actually reads. `user_config_path` used
    /// `dirs::config_dir()`, which ignores `XDG_CONFIG_HOME` on Windows —
    /// so on a machine with cross-platform dotfiles the denial pointed at a
    /// `config.toml` the fetch path never opened. Naming the wrong file is
    /// worse than naming none.
    #[test]
    #[serial]
    fn denial_help_names_the_file_the_reader_loads() {
        struct EnvGuard(&'static str, Option<String>);
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                match &self.1 {
                    Some(v) => std::env::set_var(self.0, v),
                    None => std::env::remove_var(self.0),
                }
            }
        }
        let td = tempfile::TempDir::new().expect("tempdir");
        let _g: Vec<EnvGuard> = ["XDG_CONFIG_HOME", "APPDATA", "HOME", "USERPROFILE"]
            .iter()
            .map(|k| EnvGuard(k, std::env::var(k).ok()))
            .collect();
        std::env::set_var("XDG_CONFIG_HOME", td.path());

        let reader = super::config_dir_utf8()
            .expect("reader resolves")
            .join("doiget")
            .join("config.toml");
        let helped = crate::commands::user_config_path().expect("help path resolves");
        assert_eq!(
            helped, reader,
            "the denial help must name the config.toml the reader loads"
        );

        let mut dc = denial(DenialReason::RedirectNotInAllowlist);
        dc.attempted = Some("strathprints.strath.ac.uk".to_string());
        let joined = denial_note_lines(&dc, Some(helped.as_path())).join("\n");
        assert!(
            joined.contains(reader.as_str()),
            "rendered help must carry that path; got:\n{joined}"
        );
    }

    // ── #405: the denial must name the knob that unblocks it ─────────────

    /// A `redirect_not_in_allowlist` denial is not "this host is forbidden",
    /// it is "you have not enabled the class it belongs to". The advisory
    /// block MUST name the config file and BOTH supported keys, and echo the
    /// attempted host into the `additional_hosts` line so the fix is
    /// copy-pasteable (issue #405).
    #[test]
    fn redirect_denial_names_both_allowlist_keys_and_the_config_file() {
        let mut dc = denial(DenialReason::RedirectNotInAllowlist);
        dc.attempted = Some("strathprints.strath.ac.uk".to_string());
        dc.expected = Some(vec!["*.springer.com".to_string()]);

        let cfg = camino::Utf8PathBuf::from("/home/alice/.config/doiget/config.toml");
        let lines = denial_note_lines(&dc, Some(cfg.as_path()));
        let joined = lines.join("\n");

        assert!(
            joined.contains("attempted strathprints.strath.ac.uk; allowed: *.springer.com"),
            "the pre-existing note must survive; got:\n{joined}"
        );
        assert!(
            joined.contains("trust_academic_repos = true"),
            "the curated-set knob must be named; got:\n{joined}"
        );
        assert!(
            joined.contains("[[network.additional_hosts]] host = \"strathprints.strath.ac.uk\""),
            "the per-host escape hatch must echo the attempted host; got:\n{joined}"
        );
        assert!(
            joined.contains("/home/alice/.config/doiget/config.toml"),
            "the file the user must edit must be named; got:\n{joined}"
        );
        assert!(
            joined.contains("docs/CONFIG.md §3.1"),
            "the schema section must be named; got:\n{joined}"
        );
    }

    /// #478. Only ONE of the two flags covers any given host, and
    /// `remediation::trust_flag_for_host` already computes which -- so the
    /// MCP and `batch --json` consumers got the precise answer while the
    /// human was shown both with nothing to choose between them.
    #[test]
    fn the_help_names_only_the_trust_flag_that_covers_the_host() {
        let mut dc = denial(DenialReason::RedirectNotInAllowlist);
        dc.attempted = Some("strathprints.strath.ac.uk".to_string());
        let joined = denial_note_lines(&dc, None).join(
            "
",
        );

        assert!(
            joined.contains("trust_academic_repos = true"),
            "an *.ac.uk host is covered by the academic list; got:
{joined}"
        );
        assert!(
            !joined.contains("trust_oa_registries"),
            "trust_oa_registries does nothing for this host and must not be offered; got:
{joined}"
        );
        assert!(
            joined.contains("*.ac.uk"),
            "naming the pattern is what makes the suggestion checkable; got:
{joined}"
        );
    }

    /// And when neither covers it -- a genuine publisher host -- the human
    /// is told so rather than handed two settings that cannot help. The
    /// machine path already behaved this way
    /// (`a_publisher_host_offers_no_trust_flag` in `doiget-core`).
    #[test]
    fn a_publisher_host_is_offered_no_trust_flag_in_the_human_help() {
        let mut dc = denial(DenialReason::RedirectNotInAllowlist);
        dc.attempted = Some("link.springer.com".to_string());
        let joined = denial_note_lines(&dc, None).join(
            "
",
        );

        assert!(
            !joined.contains("trust_academic_repos = true"),
            "neither flag covers a publisher host; got:
{joined}"
        );
        assert!(
            !joined.contains("trust_oa_registries = true"),
            "neither flag covers a publisher host; got:
{joined}"
        );
        assert!(
            joined.contains("neither trust_academic_repos nor trust_oa_registries"),
            "saying so is the point -- silence would read as an omission; got:
{joined}"
        );
        // The per-host escape hatch is still the real answer here.
        assert!(
            joined.contains("additional_hosts]] host = \"link.springer.com\""),
            "got:
{joined}"
        );
    }

    /// The help block is specific to the allowlist. Other denial classes
    /// (an insecure scheme, a blocklisted host) are NOT fixed by widening
    /// the allowlist, so pointing at `trust_academic_repos` there would be
    /// actively misleading — they keep the bare `= note:`.
    #[test]
    fn non_allowlist_denials_get_no_allowlist_help() {
        for reason in [DenialReason::InsecureScheme, DenialReason::HostInBlockList] {
            let mut dc = denial(reason);
            dc.attempted = Some("evil.example.com".to_string());
            let lines = denial_note_lines(&dc, None);
            assert_eq!(
                lines.len(),
                1,
                "{reason:?} must emit the note only, got: {lines:?}"
            );
            assert!(
                !lines[0].contains("trust_academic_repos"),
                "{reason:?} is not fixed by widening the allowlist: {lines:?}"
            );
        }
    }

    /// A platform with no config dir still gets both keys — the advisory
    /// degrades to a generic file name rather than being suppressed, and
    /// `attempted: None` drops only the host-specific line.
    #[test]
    fn redirect_denial_help_degrades_without_config_dir_or_host() {
        let lines = denial_note_lines(&denial(DenialReason::RedirectNotInAllowlist), None);
        let joined = lines.join("\n");
        assert!(joined.contains("your doiget config.toml"), "{joined}");
        assert!(joined.contains("trust_academic_repos = true"), "{joined}");
        assert!(
            !joined.contains("additional_hosts]] host ="),
            "no attempted host means no copy-pasteable host line; got:\n{joined}"
        );
    }

    // ── #344 Slice 2: --link helpers ──────────────────────────────────────

    #[test]
    fn slugify_lowercases_and_collapses_non_alnum() {
        assert_eq!(
            slugify("Attention Is All You Need"),
            "attention-is-all-you-need"
        );
        assert_eq!(slugify("Foo/Bar: Baz!!"), "foo-bar-baz");
        assert_eq!(slugify("  spaced  "), "spaced");
        assert_eq!(slugify("!!!"), ""); // no alphanumerics → empty
    }

    #[test]
    fn fetch_link_filename_builds_readable_name() {
        let name = fetch_link_filename(
            "Attention Is All You Need",
            &["Ashish Vaswani".to_string()],
            Some(2017),
            "arxiv_1706.03762",
        );
        assert_eq!(name, "vaswani2017-attention-is-all-you-need.pdf");
    }

    #[test]
    fn fetch_link_filename_falls_back_to_safekey() {
        // No usable metadata (empty title, no authors/year) → safekey.pdf.
        assert_eq!(
            fetch_link_filename("", &[], None, "doi_10.1234_x"),
            "doi_10.1234_x.pdf"
        );
        // A title that slugifies to nothing also falls back.
        assert_eq!(
            fetch_link_filename("…—", &[], None, "doi_10.1234_y"),
            "doi_10.1234_y.pdf"
        );
    }

    #[test]
    fn link_artifact_creates_readable_artifact() {
        let td = tempfile::TempDir::new().expect("tempdir");
        let dir = camino::Utf8Path::from_path(td.path()).expect("utf8");
        let src = dir.join("src.pdf");
        std::fs::write(src.as_std_path(), b"%PDF-DATA").expect("write src");

        let (dst, _kind) = link_artifact(dir, &src, "out.pdf").expect("link");
        assert!(dst.exists(), "linked artifact must exist: {dst}");
        assert_eq!(
            std::fs::read(dst.as_std_path()).expect("read dst"),
            b"%PDF-DATA",
            "linked artifact (symlink or copy) must resolve to the source bytes"
        );
    }

    #[test]
    fn link_artifact_refuses_to_clobber_unrelated_file() {
        let td = tempfile::TempDir::new().expect("tempdir");
        let dir = camino::Utf8Path::from_path(td.path()).expect("utf8");
        let src = dir.join("src.pdf");
        std::fs::write(src.as_std_path(), b"%PDF-DATA").expect("write src");
        // A pre-existing, unrelated regular file at the target name.
        let taken = dir.join("taken.pdf");
        std::fs::write(taken.as_std_path(), b"USER-DATA").expect("write taken");

        let err = link_artifact(dir, &src, "taken.pdf").expect_err("must refuse");
        assert!(
            err.to_string().contains("refusing to overwrite"),
            "error must explain the refusal: {err}"
        );
        assert_eq!(
            std::fs::read(taken.as_std_path()).expect("read taken"),
            b"USER-DATA",
            "the user's file must be left untouched"
        );
    }
    /// #443, the reported case: `www.ams.org -> pubs.ams.org` cost two
    /// edit-run cycles because the help named only the hop that failed.
    #[test]
    fn a_refused_hop_also_offers_the_registrable_domain() {
        let mut dc = denial(DenialReason::RedirectNotInAllowlist);
        dc.attempted = Some("pubs.ams.org".to_string());
        let joined = denial_note_lines(&dc, None).join("\n");

        assert!(joined.contains(r#"host = "pubs.ams.org""#), "{joined}");
        assert!(
            joined.contains(r#"host = "*.ams.org""#),
            "the whole-publisher wildcard is what ends the loop in one step:\n{joined}"
        );
        assert!(
            joined.contains(r#"host = "ams.org""#),
            "a single-suffix wildcard does not match the apex, so offer it too:\n{joined}"
        );
    }

    /// A suggestion the config parser would reject is worse than none: the
    /// user pastes it and gets a second, more confusing error.
    #[test]
    fn every_suggestion_is_a_pattern_the_validator_accepts() {
        for host in [
            "pubs.ams.org",
            "www.ams.org",
            "ams.org",
            "strathprints.strath.ac.uk",
            "repository.ruj.uj.edu.pl",
            "link.springer.com",
        ] {
            for (pattern, _) in doiget_core::remediation::widening_suggestions(host) {
                doiget_core::user_extension::validate_pattern(&pattern).unwrap_or_else(|e| {
                    panic!("suggested `{pattern}` for `{host}`, which the validator rejects: {e:?}")
                });
            }
        }
    }

    /// The one suggestion that must never appear. Deriving the registrable
    /// domain by stripping a label is right for `pubs.ams.org` and very
    /// wrong for `foo.co.uk` — trusting `*.co.uk` is trusting a whole
    /// country's registry.
    #[test]
    fn a_public_suffix_is_never_offered() {
        for (host, forbidden) in [
            ("foo.co.uk", "co.uk"),
            ("foo.ac.jp", "ac.jp"),
            ("foo.com.au", "com.au"),
            ("example.org", "org"),
        ] {
            let joined: String = doiget_core::remediation::widening_suggestions(host)
                .into_iter()
                .map(|(p, _)| p)
                .collect::<Vec<_>>()
                .join(" ");
            assert!(
                !joined
                    .split(' ')
                    .any(|p| p == forbidden || p == format!("*.{forbidden}")),
                "offered the public suffix `{forbidden}` for `{host}`: {joined}"
            );
        }
    }

    /// `strathprints.strath.ac.uk` — four labels, so the parent
    /// `strath.ac.uk` is a real registration, not a public suffix.
    #[test]
    fn a_four_label_academic_host_still_gets_its_institution_wildcard() {
        let got: Vec<String> =
            doiget_core::remediation::widening_suggestions("strathprints.strath.ac.uk")
                .into_iter()
                .map(|(p, _)| p)
                .collect();
        assert!(
            got.iter().any(|p| p == "*.strath.ac.uk"),
            "expected the institution wildcard; got {got:?}"
        );
    }

    /// An apex host has no parent worth naming; the useful widening is
    /// downward.
    #[test]
    fn an_apex_host_offers_its_subdomains() {
        let got: Vec<String> = doiget_core::remediation::widening_suggestions("ams.org")
            .into_iter()
            .map(|(p, _)| p)
            .collect();
        assert_eq!(got, vec!["ams.org".to_string(), "*.ams.org".to_string()]);
    }
    /// #445: a 429 reads like a permanent block. It is the one failure
    /// where retrying the same host later is right and reconfiguring is
    /// wrong, so the message has to say which it is.
    #[test]
    fn a_rate_limited_block_says_the_limit_is_transient() {
        let joined = blocked_trace_lines(&[], "network error: HTTP 429 from https://ams.org/x.pdf")
            .join("\n");
        assert!(joined.contains("429"), "{joined}");
        assert!(joined.contains("transient"), "{joined}");
        assert!(
            joined.contains("Retry later"),
            "say what to DO, not just what happened:\n{joined}"
        );
        // A lost `\` line continuation leaves the source indentation
        // inside the literal, and every test above still passes because
        // each only asserts `contains`. Nothing in this block is
        // column-aligned, so an internal double space is that bug.
        for line in blocked_trace_lines(&[], "network error: HTTP 429 from https://ams.org/x.pdf") {
            assert!(
                !line.trim_start().contains("  "),
                "a lost line continuation left source indentation in the message:\n{line}"
            );
        }
    }

    /// The converse: a policy denial must not be described as transient,
    /// or the user retries forever instead of editing the allowlist.
    #[test]
    fn a_policy_block_is_not_described_as_transient() {
        let joined =
            blocked_trace_lines(&[], "redirect target x.example not in allowlist").join("\n");
        assert!(
            !joined.contains("transient"),
            "an allowlist denial is permanent until reconfigured:\n{joined}"
        );
    }

    /// The half of #445 that the #413 trace already answered for
    /// `NotFound`: *did anything else have it?*
    /// #505: the found-nothing path is the one outcome that reads as a
    /// result, so its silence is the most misleading. `no OA PDF available`
    /// is byte-identical whether the optional sources were on and had
    /// nothing or off and never asked.
    #[test]
    fn a_found_nothing_fetch_says_what_it_consulted_and_what_it_did_not() {
        use doiget_core::orchestrator::{AttemptOutcome, SourceAttempt};
        let ref_ = Ref::parse("10.1137/0117004").expect("valid doi");
        let attempts = vec![
            SourceAttempt::new("unpaywall", AttemptOutcome::NoRecord),
            SourceAttempt::new(
                "hal",
                AttemptOutcome::Disabled {
                    env: &["DOIGET_ENABLE_HAL"],
                },
            ),
        ];
        let joined = not_found_trace_lines(&ref_, &attempts).join(
            "
",
        );

        assert!(
            joined.contains("unpaywall") && joined.contains("no record"),
            "what ran, and what it said:
{joined}"
        );
        assert!(
            joined.contains("DOIGET_ENABLE_HAL"),
            "a source never asked must still name its switch:
{joined}"
        );
        // The line to paste, not prose about it.
        assert!(
            joined.contains("DOIGET_ENABLE_HAL=1 doiget fetch 10.1137/0117004"),
            "the widening command must be runnable as printed:
{joined}"
        );
    }

    /// #505 part 3, and the property the issue cares about most: the ranking
    /// is an ORDERING OF THE FULL LIST, never a shortlist.
    ///
    /// > a ranking that is wrong is worse than no ranking, because it makes
    /// > people stop early.
    ///
    /// A source that is dropped from the list is a source the reader will not
    /// try, so every unconsulted source must appear somewhere.
    #[test]
    fn the_ranking_lists_every_unconsulted_source_and_drops_none() {
        use doiget_core::orchestrator::{AttemptOutcome, SourceAttempt};
        let disabled = |name: &'static str, env: &'static [&'static str]| {
            SourceAttempt::new(name, AttemptOutcome::Disabled { env })
        };
        let attempts = vec![
            SourceAttempt::new("crossref", AttemptOutcome::NoRecord),
            disabled("core", &["DOIGET_ENABLE_CORE"]),
            disabled("openalex", &["DOIGET_ENABLE_OPENALEX"]),
            disabled("hal", &["DOIGET_ENABLE_HAL"]),
            disabled("europe-pmc", &["DOIGET_ENABLE_EUROPE_PMC"]),
        ];

        let (first, middle, last) = rank_unconsulted(&attempts);
        let mut all: Vec<&str> = first
            .iter()
            .chain(middle.iter())
            .chain(last.iter())
            .copied()
            .collect();
        all.sort_unstable();
        assert_eq!(
            all,
            vec!["core", "europe-pmc", "hal", "openalex"],
            "every source that was not consulted must appear, and only those"
        );

        // A consulted source contributes nothing: it already answered.
        assert!(!all.contains(&"crossref"));

        // The two positions that HAVE a signal.
        assert_eq!(first, vec!["openalex"], "the lookup goes first");
        assert_eq!(last, vec!["core"], "the broadest index goes last");
        assert_eq!(middle, vec!["europe-pmc", "hal"]);
    }

    /// The rendered form must mark item 1 as categorically different and must
    /// say the middle is unordered. Presenting a lookup and a guess in one
    /// list without saying which is which is the failure mode #505 is about.
    #[test]
    fn the_rendered_ranking_says_which_part_is_a_guess() {
        use doiget_core::orchestrator::{AttemptOutcome, SourceAttempt};
        let ref_ = Ref::parse("10.1137/0117004").expect("valid doi");
        let attempts = vec![
            SourceAttempt::new("crossref", AttemptOutcome::NoRecord),
            SourceAttempt::new(
                "openalex",
                AttemptOutcome::Disabled {
                    env: &["DOIGET_ENABLE_OPENALEX"],
                },
            ),
            SourceAttempt::new(
                "hal",
                AttemptOutcome::Disabled {
                    env: &["DOIGET_ENABLE_HAL"],
                },
            ),
            SourceAttempt::new(
                "core",
                AttemptOutcome::Disabled {
                    env: &["DOIGET_ENABLE_CORE"],
                },
            ),
        ];
        let joined = not_found_trace_lines(&ref_, &attempts).join(
            "
",
        );

        assert!(
            joined.contains("a lookup, not a guess"),
            "item 1 must be marked as categorically different:
{joined}"
        );
        assert!(
            joined.contains("NO particular order"),
            "the middle must not read as an ordering:
{joined}"
        );
        assert!(
            joined.contains("An invented order would read as information"),
            "and it must say WHY there is no order, which is the named signal:
{joined}"
        );
        assert!(
            joined.contains("never the first try"),
            "core's position must carry its own reason:
{joined}"
        );
    }

    /// Nothing to rank when nothing was skipped, and the common path gains no
    /// noise from a feature about the uncommon one.
    #[test]
    fn a_run_that_skipped_nothing_gets_no_ranking() {
        use doiget_core::orchestrator::{AttemptOutcome, SourceAttempt};
        let ref_ = Ref::parse("10.1137/0117004").expect("valid doi");
        let attempts = vec![SourceAttempt::new("crossref", AttemptOutcome::NoRecord)];
        let joined = not_found_trace_lines(&ref_, &attempts).join(
            "
",
        );
        assert!(
            !joined.contains("not consulted:"),
            "no skipped sources means no ranking block:
{joined}"
        );
    }

    /// No trace at all when there is nothing to say. An empty attempt list
    /// means the chain never recorded anything, and inventing a block for it
    /// would be noise on the one path users see most.
    #[test]
    fn no_attempts_means_no_found_nothing_trace() {
        let ref_ = Ref::parse("10.1137/0117004").expect("valid doi");
        assert!(not_found_trace_lines(&ref_, &[]).is_empty());
    }

    /// The advice has to be actionable to be worth printing.
    ///
    /// `resolve_metadata_flag` returns false when the variable is SET but the
    /// Cargo feature was not compiled in, so the source still reports
    /// `Disabled` naming a variable the user already set. Telling them to set
    /// it again is the same species of unhelpful as the bare
    /// `no OA PDF available` this issue is about.
    #[test]
    #[serial]
    fn an_already_set_switch_is_reported_as_a_build_problem_not_a_config_one() {
        use doiget_core::orchestrator::{AttemptOutcome, SourceAttempt};
        let _guard = EnvGuard::save("DOIGET_ENABLE_HAL");
        std::env::set_var("DOIGET_ENABLE_HAL", "1");

        let ref_ = Ref::parse("10.1137/0117004").expect("valid doi");
        let attempts = vec![SourceAttempt::new(
            "hal",
            AttemptOutcome::Disabled {
                env: &["DOIGET_ENABLE_HAL"],
            },
        )];
        let joined = not_found_trace_lines(&ref_, &attempts).join(
            "
",
        );

        assert!(
            !joined.contains("doiget fetch 10.1137/0117004"),
            "must NOT tell them to set what they have already set:
{joined}"
        );
        assert!(
            joined.contains("built without"),
            "must name the real blocker, which is the build:
{joined}"
        );
    }

    #[test]
    fn a_blocked_leg_reports_which_other_sources_were_consulted() {
        use doiget_core::orchestrator::{AttemptOutcome, SourceAttempt};
        let attempts = vec![
            SourceAttempt::new("core", AttemptOutcome::NoRecord),
            SourceAttempt::new(
                "hal",
                AttemptOutcome::Disabled {
                    env: &["DOIGET_ENABLE_HAL"],
                },
            ),
        ];
        let joined = blocked_trace_lines(&attempts, "HTTP 429").join("\n");
        assert!(
            joined.contains("the other sources were consulted"),
            "at least one WAS consulted:\n{joined}"
        );
        assert!(
            joined.contains("core") && joined.contains("no record"),
            "{joined}"
        );
        assert!(
            joined.contains("DOIGET_ENABLE_HAL"),
            "a source that was never asked must still name its switch:\n{joined}"
        );
    }

    /// All five flags off is a configuration problem, not a data problem,
    /// and must not read as "nothing else has this paper".
    #[test]
    fn a_blocked_leg_with_nothing_consulted_says_so() {
        use doiget_core::orchestrator::{AttemptOutcome, SourceAttempt};
        let attempts = vec![
            SourceAttempt::new(
                "core",
                AttemptOutcome::Disabled {
                    env: &["DOIGET_ENABLE_CORE"],
                },
            ),
            SourceAttempt::new(
                "hal",
                AttemptOutcome::Disabled {
                    env: &["DOIGET_ENABLE_HAL"],
                },
            ),
        ];
        let joined = blocked_trace_lines(&attempts, "HTTP 429").join("\n");
        assert!(
            joined.contains("no other source was consulted"),
            "must not imply the paper is unavailable elsewhere:\n{joined}"
        );
    }

    /// An arXiv fetch has no optional chain; it must not grow an empty
    /// note block.
    #[test]
    fn no_attempts_means_no_trace_block() {
        let lines = blocked_trace_lines(&[], "not-a-pdf body");
        assert!(lines.is_empty(), "{lines:?}");
    }
}
