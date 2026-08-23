//! `doiget config <action>` — config introspection.
//!
//! This subcommand is intentionally read-only and does NOT touch the network
//! or instantiate the Store. Phase 1 resolves config from environment
//! variables only with default fallbacks; the user `config.toml` reader
//! lands in a follow-up. See `docs/CONFIG.md` for the canonical schema.
//!
//! `print_stdout` is denied workspace-wide for MCP stdio safety (ADR-0001 /
//! `docs/SECURITY.md` §3). The `config show` and `config path` actions are
//! the *spec'd* stdout channel for human-facing introspection — they are
//! never invoked from inside an MCP session (`doiget serve` runs a
//! different code path), so the lint is locally relaxed below.

use anyhow::{Context, Result};
use camino::Utf8PathBuf;

use super::fetch::CliExit;

/// Snapshot of the env-var + default-fallback config that `doiget` would
/// use on the current machine.
///
/// Phase 1 surface: env vars only (`DOIGET_STORE_ROOT`, `DOIGET_LOG_PATH`,
/// `DOIGET_CONTACT_EMAIL`, `DOIGET_UNPAYWALL_EMAIL`) layered over
/// XDG / known-folder defaults. Phase 2 will layer the user config.toml
/// underneath the env vars per `docs/CONFIG.md` §1.
///
/// Issue #142: `log_path` is resolved from `DOIGET_LOG_PATH` — the ONLY
/// log env var `docs/CONFIG.md` §4 documents — using the exact same
/// resolution the provenance-log *writer*
/// (`commands::fetch::resolve_log_path` / `commands::audit_log`) uses, so
/// `config show` reports the path the writer actually uses. The previously
/// read, undocumented `DOIGET_LOG_DIR` has been dropped.
#[derive(Debug, serde::Serialize)]
pub struct ResolvedConfig {
    /// Root of the on-disk paper store. Default: `./papers` (under the cwd).
    pub store_root: Utf8PathBuf,
    /// Which rung of the ADR-0036 order produced `store_root` (#441).
    ///
    /// Reported because the failure it guards against is invisible
    /// otherwise: a `[store] root` that is present but unread resolves to
    /// the cwd default, and the two coincide whenever the user happens to
    /// run from the directory they configured.
    pub store_root_source: String,
    /// Directory holding doiget's append-only logs. Derived from
    /// `log_path`'s parent so it always agrees with the writer.
    pub log_dir: Utf8PathBuf,
    /// JSON-Lines provenance log file path. `DOIGET_LOG_PATH` when set,
    /// otherwise `<config_dir>/doiget/access.jsonl` (`docs/CONFIG.md` §4).
    pub log_path: Utf8PathBuf,
    /// Directory holding `config.toml` and `credentials.toml`.
    pub config_dir: Utf8PathBuf,
    /// Path of the user config file (may not exist on disk yet).
    pub config_path: Utf8PathBuf,
    /// Contact email for the polite User-Agent header (and Unpaywall fallback).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contact_email: Option<String>,
    /// Unpaywall-specific contact email; falls back to `contact_email` when unset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unpaywall_email: Option<String>,
}

impl ResolvedConfig {
    /// Resolve the live config from process environment + platform defaults.
    ///
    /// Errors only if the platform config directory (`dirs::config_dir()`) or
    /// the current working directory cannot be determined or is non-UTF-8
    /// (an unknown / locked-down platform); on every realistic POSIX or
    /// Windows host this returns `Ok` even with no `DOIGET_*` env vars set.
    pub fn from_env() -> Result<Self> {
        // Issue #405: resolve the config dir the SAME way the READER does
        // (`commands::fetch::config_dir_utf8`, which `build_http_client`
        // uses to load `[[network.additional_hosts]]`), for the same
        // reason `store_root` and `log_path` already reuse their writers'
        // resolvers — `config show` / `config path` / `doctor` must never
        // name a file other than the one that is actually read.
        //
        // They diverged before this: `dirs::config_dir()` resolves the
        // Windows roaming AppData through the known-folder API and ignores
        // `XDG_CONFIG_HOME` entirely, while `config_dir_utf8()` checks
        // `XDG_CONFIG_HOME` first on every platform. So on Windows a user
        // with `XDG_CONFIG_HOME` set — normal for cross-platform dotfiles —
        // got `doiget fetch` reading one `config.toml` while
        // `doiget config doctor` validated a different one and reported
        // "user-extension hosts loaded: 0" about a file the fetch path had
        // never opened. That makes the #405 doctor hint point at the wrong
        // file, which is worse than not printing it.
        let cfg = super::fetch::config_dir_utf8()?;

        // Store root: identical resolution to where artifacts actually land
        // (`super::resolve_store_root`) so `config show` / `doctor` never drifts
        // from the writer — `DOIGET_STORE_ROOT` else `./papers` under the cwd
        // (#344 / ADR-0036).
        let (store_root, store_root_source) = super::resolve_store_root_with_source()?;

        // Issue #142: resolve the log path the SAME way the writer does
        // (`commands::fetch::resolve_log_path` / `commands::audit_log`):
        // `DOIGET_LOG_PATH` (the only log env var documented in
        // `docs/CONFIG.md` §4) when set, otherwise
        // `<config_dir>/doiget/access.jsonl`. The undocumented
        // `DOIGET_LOG_DIR` is no longer read, so `config show` can no
        // longer disagree with the path the provenance log is written to.
        let log_path = match std::env::var("DOIGET_LOG_PATH") {
            Ok(s) if !s.is_empty() => Utf8PathBuf::from(s),
            _ => cfg.join("doiget").join("access.jsonl"),
        };
        // `log_dir` is purely derived from `log_path` so the two can never
        // drift; fall back to the config dir for a path with no parent.
        let log_dir = log_path
            .parent()
            .map(Utf8PathBuf::from)
            .unwrap_or_else(|| cfg.join("doiget"));

        let config_dir = cfg.join("doiget");
        let config_path = config_dir.join("config.toml");

        Ok(Self {
            store_root,
            store_root_source: store_root_source.label().to_string(),
            log_dir,
            log_path,
            config_dir,
            config_path,
            contact_email: std::env::var("DOIGET_CONTACT_EMAIL").ok(),
            unpaywall_email: std::env::var("DOIGET_UNPAYWALL_EMAIL").ok(),
        })
    }
}

/// Dispatch entrypoint for `doiget config <action>`.
///
/// `action` is one of `show`, `path`, `doctor`. Anything else returns
/// `Err`; clap currently passes the raw string through.
//
// `print_stdout` and `print_stderr` are workspace-deny / workspace-warn for
// MCP stdio safety. The `config` subcommand is the explicit human-facing
// stdout channel for the resolved config; `doctor`'s checklist lines also
// belong on stderr by design (stdout stays clean for `| jq` style pipes
// when we add `--json` later).
#[allow(clippy::print_stdout, clippy::print_stderr)]
pub async fn run(
    action: String,
    mode: super::output::OutputMode,
    network: bool,
    force: bool,
) -> Result<()> {
    // `mode` honors ADR-0017: `Quiet` suppresses the TOML dump (`show`)
    // and the path println! (`path`); `doctor` is unaffected because its
    // per-check output is on stderr and only the failure/success exit
    // code is the user-visible signal (#203). Json body for `show` is
    // tracked in #204.
    let cfg = ResolvedConfig::from_env()?;
    if network && action != "doctor" {
        eprintln_err("error: --network applies to `config doctor` only");
        return Err(anyhow::Error::new(CliExit(2)));
    }
    if force && action != "init" {
        eprintln_err("error: --force applies to `config init` only");
        return Err(anyhow::Error::new(CliExit(2)));
    }
    match action.as_str() {
        "show" => match mode {
            super::output::OutputMode::Quiet => {}
            super::output::OutputMode::Json => {
                // #204: `ResolvedConfig` is `Serialize` (already used for
                // the TOML branch).
                let s = serde_json::to_string_pretty(&cfg)
                    .map_err(|e| anyhow::anyhow!("serialise config to JSON: {e}"))?;
                println!("{s}");
            }
            _ => {
                let s = toml::to_string_pretty(&cfg)?;
                print!("{s}");
            }
        },
        "init" => init_config(&cfg, force, mode)?,
        "path" => match mode {
            super::output::OutputMode::Quiet => {}
            super::output::OutputMode::Json => {
                // Minimal JSON object so callers can parse the path
                // uniformly; no trailing-newline ambiguity vs the raw
                // `path` form.
                println!(
                    "{}",
                    serde_json::json!({ "config_path": cfg.config_path.as_str() })
                );
            }
            _ => {
                println!("{}", cfg.config_path);
            }
        },
        "doctor" => {
            let mut all_ok = true;
            let store_parent = cfg.store_root.parent().map(|p| p.as_str()).unwrap_or("");
            // Issue #406: the default store root is `./papers` under the
            // CWD (ADR-0036), so it MOVES every time the user `cd`s. A
            // check that says only "parent exists" confirms a path the
            // user cannot see; naming it is what makes the cwd-relative
            // default self-evident instead of surprising.
            check(
                &format!("store_root: {}", cfg.store_root),
                true,
                None,
                &mut all_ok,
            );
            // #441: naming the rung is the whole point. A `[store] root`
            // that is set but unread lands on the cwd default, and the two
            // coincide whenever the user runs from the directory they
            // configured — so "store_root: /home/me/papers" alone cannot
            // distinguish "your setting worked" from "your setting was
            // ignored and you happen to be standing in it".
            eprintln!("       from: {}", cfg.store_root_source);
            if cfg.store_root_source == super::StoreRootSource::CwdDefault.label() {
                eprintln!(
                    "       note: relative to the current directory (ADR-0036). Set DOIGET_STORE_ROOT"
                );
                eprintln!("             (or store.root in config.toml) for one central library.");
            }
            check(
                "store_root parent exists",
                cfg.store_root.parent().map(|p| p.exists()).unwrap_or(true),
                Some(&format!(
                    "create the parent directory or override via \
                     DOIGET_STORE_ROOT\n               \
                     missing parent: {store_parent}"
                )),
                &mut all_ok,
            );
            let log_parent = cfg.log_dir.parent().map(|p| p.as_str()).unwrap_or("");
            check(
                "log_dir parent exists",
                cfg.log_dir.parent().map(|p| p.exists()).unwrap_or(true),
                Some(&format!(
                    "create the parent directory or override via \
                     DOIGET_LOG_PATH\n               \
                     missing parent: {log_parent}"
                )),
                &mut all_ok,
            );
            check(
                "contact_email set",
                cfg.contact_email.is_some(),
                Some(
                    "set DOIGET_CONTACT_EMAIL to your email address\n               \
                     e.g. export DOIGET_CONTACT_EMAIL=you@institution.edu\n               \
                     (required for the polite User-Agent header and Unpaywall API)",
                ),
                &mut all_ok,
            );
            // ADR-0028 D2: surface user-extension allowlist health. A
            // missing config.toml is normal (curated set only); a
            // present-but-malformed config.toml is a doctor failure so
            // the operator finds out before fetch attempts silently
            // skip the extension path. `user_extension::load` returns
            // `Ok(vec![])` for not-found, so the OK arm always reports
            // a count.
            match doiget_core::user_extension::load(&cfg.config_path) {
                Ok(cfg_ext) => {
                    check(
                        &format!(
                            "user-extension hosts loaded: {} (academic={}, oa_registries={})",
                            cfg_ext.additional_hosts.len(),
                            cfg_ext.trust_academic_repos,
                            cfg_ext.trust_oa_registries
                        ),
                        true,
                        None,
                        &mut all_ok,
                    );
                    // Issue #405: reporting `trust_academic_repos=false`
                    // states the fact without naming the fix, and a default
                    // install (no config.toml at all) is exactly the posture
                    // whose OA fetches get denied at an off-allowlist
                    // redirect. When nothing has widened the allowlist, name
                    // the file and both keys. `check` swallows tips on a
                    // passing check by design, so this is a separate
                    // advisory line — the check itself stays `[ ok ]`,
                    // because having no config is a valid posture.
                    let widened = cfg_ext.trust_academic_repos
                        || cfg_ext.trust_oa_registries
                        || !cfg_ext.additional_hosts.is_empty();
                    if !widened {
                        eprintln!("       note: built-in allowlist only. To widen it, edit");
                        eprintln!("             {}", cfg.config_path);
                        eprintln!(
                            "             [network] trust_academic_repos = true   # *.ac.uk, \
                             *.ac.jp, ..."
                        );
                        eprintln!(
                            "             [network] trust_oa_registries  = true   # DOAJ, \
                             SciELO, Zenodo, ..."
                        );
                        eprintln!(
                            "             [[network.additional_hosts]]            # anything \
                             else — docs/CONFIG.md §3.1"
                        );
                    }
                }
                Err(e) => check(
                    &format!("user-extension config invalid: {e}"),
                    false,
                    Some(&format!(
                        "fix {} — see docs/CONFIG.md §3 for the \
                         [[network.additional_hosts]] schema",
                        cfg.config_path
                    )),
                    &mut all_ok,
                ),
            }
            // Issue #407: the network section is opt-in behind `--network`
            // because it makes real outbound requests. Everything above is
            // local and always runs, so `--network` extends the report
            // rather than replacing it.
            if network {
                network_report(&cfg).await;
            }
            // Trying to actually create the dirs would have side-effects;
            // keep doctor read-only and just check existence of parents.
            if !all_ok {
                // Issue #149: a failing doctor means missing/invalid
                // config — `docs/ERRORS.md` §4 classes "missing config"
                // as misuse → exit 2 (the per-check `[FAIL]` lines were
                // already written to stderr by `check`).
                eprintln_err("error: config doctor: one or more checks failed");
                return Err(anyhow::Error::new(CliExit(2)));
            }
        }
        other => {
            // Issue #149: an unknown subcommand action is clear argument
            // misuse → `docs/ERRORS.md` §4 exit 2, not the generic exit 1
            // a bare `bail!` produced.
            eprintln_err(&format!(
                "error: unknown config action: {other}; expected `init` / `show` / `path` / `doctor`"
            ));
            return Err(anyhow::Error::new(CliExit(2)));
        }
    }
    Ok(())
}

/// Stderr sink for the `docs/ERRORS.md` §3 human-error lines. The
/// localized `#[allow]` is the minimal intervention for the workspace
/// `clippy::print_stderr` lint (same pattern as `commands::fetch`).
#[allow(clippy::print_stderr)]
fn eprintln_err(msg: &str) {
    eprintln!("{msg}");
}

/// Emit one `[ ok ]` / `[FAIL]` checklist line to stderr and update the
/// running pass/fail flag. Stderr is used so that `doiget config doctor`
/// stdout stays empty for green runs (script-friendly).
///
/// When `ok` is `false` and `tip` is `Some`, a remediation tip is printed
/// on the next line, indented so it is visually attached to the failed
/// check (issue #322).
/// The commented `config.toml` template written by `doiget config init`.
///
/// Issue #408: on a fresh install `~/.config/doiget/config.toml` does not
/// exist, nothing creates it, and four of the settings that decide the
/// outcome of a session live in it. Three of those four fail *silently*.
/// Every commented line here doubles as documentation at the place the user
/// is already looking.
///
/// Pure and `pub(crate)` so the tests can assert the template actually
/// mentions each load-bearing key — a template that silently loses one is
/// the failure mode worth guarding against.
pub(crate) fn config_template() -> &'static str {
    // NOTE: every key below is commented out on purpose. Writing live values
    // would change behaviour just by running `init`; the file's job is to be
    // a discoverable, annotated menu, not a new set of defaults.
    r#"# ~/.config/doiget/config.toml — written by `doiget config init`.
#
# Every field is optional and every line below is commented out: this file
# documents the choices, it does not change behaviour until you uncomment
# something. Re-run `doiget config init --force` to restore this template.
#
# See docs/CONFIG.md for the full schema, and run `doiget config doctor`
# (add --network for outbound checks) to see what is actually in effect.

[store]
# Where fetched papers are written.
#
# DEFAULT: `./papers` — relative to the CURRENT WORKING DIRECTORY, so it
# moves with you (ADR-0036). That is deliberate: artifacts land where the
# work is, instead of somewhere you have to go looking for. The cost is that
# fetching from many directories leaves several small stores. Set this for a
# single central library.
#
# Overridden by DOIGET_STORE_ROOT and by --store-root, which share a rung
# above this one. A leading `~` IS expanded here (a config file has no
# shell, unlike the env var).
# root = "/home/you/papers"

[network]
# Contact address for the polite pool. STRONGLY RECOMMENDED.
#
# Without it doiget still queries Unpaywall, but as `doiget@localhost`, from
# the non-polite pool — where you may be throttled or refused. Since the
# automatic arXiv-preprint fallback fires on what Unpaywall reports, a
# throttled response quietly costs you that fallback too.
# unpaywall_email = "you@institution.edu"

# Allow the curated academic-repository suffixes, i.e. where institutions
# host their own Green OA:
#   *.ac.uk  *.ac.jp  *.jst.go.jp  *.edu.au  *.edu.cn  *.ac.cn  *.edu.pl
#   *.ac.nz  *.ac.za  *.ac.in  *.edu.br  *.edu.tw  *.edu.tr  *.edu.ar
#   *.edu.mx
#
# Without this, an OA PDF on e.g. `strathprints.strath.ac.uk` is denied with
# `error[CAPABILITY_DENIED] ... redirect_not_in_allowlist`.
# trust_academic_repos = false

# Allow the curated cross-publisher OA registries and repositories:
#   scielo.org  zenodo.org  osf.io  hal.science  core.ac.uk  (+ subdomains)
#
# Separate from the flag above because the trust argument differs: one is
# "this institution publishes its own work here", the other is "this registry
# indexes open content across publishers". DOAJ needs no flag — it is on the
# default allowlist (ADR-0037).
# trust_oa_registries = false

# Anything outside both curated sets. Each entry is a literal FQDN or a
# single-suffix wildcard (`*.example.edu`); multi-segment globs are rejected
# at load time, as are unknown keys in this table.
# [[network.additional_hosts]]
# host = "repository.example.edu"
# note = "free-text, optional"

# Request timeouts, in seconds.
# connect_timeout_sec = 10
# read_timeout_sec = 60
# total_timeout_sec = 300

[output]
# mode = "human"     # human | json | quiet | mcp
# color = "auto"     # auto | always | never
# progress = false
# emoji = false
"#
}

/// `doiget config init` — write [`config_template`] to the resolved config
/// path (issue #408).
///
/// Refuses to overwrite an existing file unless `force`. That refusal is the
/// whole safety property: the file may hold a user's hand-written allowlist,
/// and silently replacing it with a fully commented-out template would
/// disable every host they had added.
#[allow(clippy::print_stdout, clippy::print_stderr)]
fn init_config(cfg: &ResolvedConfig, force: bool, mode: super::output::OutputMode) -> Result<()> {
    let path = &cfg.config_path;
    let existed = path.exists();
    if existed && !force {
        eprintln_err(&format!(
            "error: {path} already exists; pass --force to overwrite it"
        ));
        return Err(anyhow::Error::new(CliExit(2)));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent.as_std_path())
            .with_context(|| format!("creating config directory {parent}"))?;
    }
    std::fs::write(path.as_std_path(), config_template())
        .with_context(|| format!("writing {path}"))?;

    match mode {
        super::output::OutputMode::Quiet => {}
        super::output::OutputMode::Json => {
            println!(
                "{}",
                serde_json::json!({
                    "ok": true,
                    "config_path": path.as_str(),
                    "overwritten": existed,
                })
            );
        }
        _ => {
            let verb = if existed { "overwrote" } else { "wrote" };
            println!("{verb} {path}");
            eprintln_err(
                "  = note: every field is commented out; nothing changed until you edit it",
            );
        }
    }
    Ok(())
}

/// Classification of a single publisher probe (issue #407).
///
/// The point of the enum is the `BotChallenge` arm. A publisher WAF answers
/// a scripted client with `202 Accepted` and an empty body; a report that
/// only printed the status would call that a success and send the user off
/// to debug their subscription, when the binding constraint is that they
/// are not a browser. Status and body size together separate the two.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeVerdict {
    /// 2xx with a non-empty body — the host served this client.
    Ok {
        /// HTTP status observed.
        status: u16,
        /// Body size in bytes.
        bytes: usize,
    },
    /// 2xx with an empty body. Almost always a bot-challenge holding
    /// response, not a paywall and not an outage.
    BotChallenge {
        /// HTTP status observed (typically 202).
        status: u16,
    },
    /// 401 / 403 — reached the host, which refused. A subscription or
    /// credential question, not a transport one.
    Refused {
        /// HTTP status observed (401 or 403).
        status: u16,
    },
    /// Any other status.
    Status {
        /// HTTP status observed.
        status: u16,
    },
    /// The host is not on the source allowlist, so no request was sent.
    NotAllowlisted,
    /// Transport failure — DNS, TLS, connect, or timeout.
    Unreachable {
        /// Rendered transport error.
        reason: String,
    },
}

impl ProbeVerdict {
    /// Map a [`doiget_core::http::ProbeOutcome`] to a verdict. Pure, so
    /// the classification —
    /// the part with the actual judgement in it — is unit-testable without
    /// a network or a mock server.
    pub fn classify(status: u16, body_bytes: usize) -> Self {
        match status {
            200..=299 if body_bytes == 0 => Self::BotChallenge { status },
            200..=299 => Self::Ok {
                status,
                bytes: body_bytes,
            },
            401 | 403 => Self::Refused { status },
            other => Self::Status { status: other },
        }
    }

    /// One-line rendering: what happened, then what it means.
    pub fn render(&self) -> String {
        match self {
            Self::Ok { status, bytes } => format!("{status} {bytes} bytes    ok"),
            Self::BotChallenge { status } => {
                format!("{status} empty body  bot challenge — needs a TDM key or a real browser")
            }
            Self::Refused { status } => {
                format!("{status}               reached, refused — subscription or credential")
            }
            Self::Status { status } => format!("{status}               unexpected status"),
            Self::NotAllowlisted => {
                "not allowlisted   no request sent; add the host or enable a trust flag".to_string()
            }
            Self::Unreachable { reason } => format!("unreachable       {reason}"),
        }
    }
}

/// `doiget config doctor --network` — the outbound half of the report
/// (issue #407).
///
/// Answers the question a user on an institutional network actually has:
/// *which publishers will talk to me?* One GET per probed host, no retries,
/// and only against hosts already on the `oa-publisher` allowlist — a
/// doctor that probed arbitrary hosts would be an SSRF gadget wearing a
/// diagnostic hat.
///
/// **Egress address is deliberately not reported.** Determining it requires
/// asking a third-party echo service, which would be a new outbound
/// dependency and a new `PRIVACY.md` entry for a diagnostic. The report
/// names the proxy configuration in effect — the part doiget actually
/// knows — and leaves the address to `curl`.
#[allow(clippy::print_stderr)]
async fn network_report(cfg: &ResolvedConfig) {
    eprintln!();
    eprintln!("network (--network):");

    for var in ["HTTPS_PROXY", "https_proxy", "NO_PROXY", "no_proxy"] {
        if let Ok(v) = std::env::var(var) {
            if !v.is_empty() {
                eprintln!("  proxy           {var}={v}");
            }
        }
    }
    eprintln!(
        "  egress          not probed (needs a third-party echo service; try `curl ifconfig.me`)"
    );
    eprintln!("                  a proxy fixes addressing, never a bot wall");

    match &cfg.contact_email {
        Some(e) => eprintln!("  unpaywall       polite pool as {e}"),
        None => eprintln!(
            "  unpaywall       non-polite pool (no DOIGET_CONTACT_EMAIL; may be throttled)"
        ),
    }

    let client = match crate::commands::fetch::build_http_client(None) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("  probes          unavailable: {e}");
            return;
        }
    };
    let Some(allow) = client.source_allowlist("oa-publisher") else {
        eprintln!("  probes          unavailable: oa-publisher source not registered");
        return;
    };
    eprintln!(
        "  oa-publisher    {} host patterns allowlisted",
        allow.redirect_hosts.len()
    );

    // Publishers a paywalled-literature user is most likely to ask about.
    // Listed whether or not they are allowlisted: "ieee.org NOT
    // allowlisted" is the single most useful line in the report for the
    // #407 case, and it can only be printed for a host we name up front.
    const PROBES: &[(&str, &str)] = &[
        ("link.springer.com", "https://link.springer.com/robots.txt"),
        ("www.mdpi.com", "https://www.mdpi.com/robots.txt"),
        ("journals.plos.org", "https://journals.plos.org/robots.txt"),
        ("arxiv.org", "https://arxiv.org/robots.txt"),
        (
            "ieeexplore.ieee.org",
            "https://ieeexplore.ieee.org/robots.txt",
        ),
        ("dl.acm.org", "https://dl.acm.org/robots.txt"),
        ("epubs.siam.org", "https://epubs.siam.org/robots.txt"),
        ("doaj.org", "https://doaj.org/robots.txt"),
    ];
    for (host, url) in PROBES {
        let verdict = if !allow.matches(host) {
            ProbeVerdict::NotAllowlisted
        } else {
            match url::Url::parse(url) {
                Err(e) => ProbeVerdict::Unreachable {
                    reason: format!("bad probe URL: {e}"),
                },
                Ok(u) => match client.probe("oa-publisher", u).await {
                    Ok(o) => ProbeVerdict::classify(o.status, o.body_bytes),
                    Err(e) => ProbeVerdict::Unreachable {
                        reason: e.to_string(),
                    },
                },
            }
        };
        eprintln!("  probe {host:<22} {}", verdict.render());
    }
    eprintln!();
    eprintln!("  IP-based subscription does not imply fetchability: a publisher WAF can");
    eprintln!("  answer a scripted client with a challenge regardless of entitlement. The");
    eprintln!("  routes that work are per-publisher TDM credentials (docs/CONFIG.md §6)");
    eprintln!("  or a real browser on the subscribing network.");
}

#[allow(clippy::print_stderr)]
fn check(label: &str, ok: bool, tip: Option<&str>, all_ok: &mut bool) {
    let mark = if ok { "[ ok ]" } else { "[FAIL]" };
    eprintln!("{mark} {label}");
    if !ok {
        if let Some(t) = tip {
            eprintln!("       tip: {t}");
        }
        *all_ok = false;
    }
}

// ---------------------------------------------------------------------------
// Tests — env-mutating, serialized via serial_test (same convention as
// `doiget-core::tests`). Each test resets the four env vars it touches via
// an EnvGuard RAII drop guard so that prior values are restored on panic.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

    use super::*;

    /// RAII guard that captures the prior value of an env var on
    /// construction and restores it on drop. Mirrors the convention in
    /// `crates/doiget-core/src/lib.rs::tests`.
    struct EnvGuard {
        var: &'static str,
        prior: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn unset(var: &'static str) -> Self {
            let prior = std::env::var_os(var);
            // SAFETY: tests are serialized via `#[serial_test::serial]`;
            // no other thread reads/writes env state concurrently.
            std::env::remove_var(var);
            EnvGuard { var, prior }
        }

        fn set(var: &'static str, value: &str) -> Self {
            let prior = std::env::var_os(var);
            std::env::set_var(var, value);
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

    /// Unset every env var the `config` subcommand reads. Returns guards
    /// that restore prior values on drop.
    fn unset_all_doiget_config_env() -> Vec<EnvGuard> {
        [
            "DOIGET_STORE_ROOT",
            "DOIGET_LOG_PATH",
            "DOIGET_CONTACT_EMAIL",
            "DOIGET_UNPAYWALL_EMAIL",
        ]
        .iter()
        .map(|v| EnvGuard::unset(v))
        .collect()
    }

    #[test]
    #[serial_test::serial]
    fn from_env_uses_cwd_default_when_unset() {
        let _g = unset_all_doiget_config_env();
        let cfg = ResolvedConfig::from_env().expect("config resolves on test host");
        // The default must be `<cwd>/papers` (ADR-0036), NOT `<home>/papers` —
        // assert the full path so a regression back to the home directory is
        // actually caught (a bare `ends_with("papers")` passes for both).
        let cwd =
            camino::Utf8PathBuf::from_path_buf(std::env::current_dir().expect("cwd is available"))
                .expect("cwd is valid UTF-8");
        assert_eq!(
            cfg.store_root,
            cwd.join("papers"),
            "store_root should default to <cwd>/papers when DOIGET_STORE_ROOT is unset; got {}",
            cfg.store_root
        );
        assert_eq!(cfg.contact_email, None);
        assert_eq!(cfg.unpaywall_email, None);
    }

    /// Issue #405: `config show` / `config path` / `doctor` MUST name the
    /// same `config.toml` that `build_http_client` reads. Before this,
    /// `ResolvedConfig` used `dirs::config_dir()` while the reader used
    /// `fetch::config_dir_utf8()`; on Windows the former ignores
    /// `XDG_CONFIG_HOME` (known-folder API), so the doctor validated a
    /// file the fetch path never opened.
    #[test]
    #[serial_test::serial]
    fn config_path_matches_the_resolver_the_reader_uses() {
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
        let _guards: Vec<EnvGuard> = ["XDG_CONFIG_HOME", "APPDATA", "HOME", "USERPROFILE"]
            .iter()
            .map(|k| EnvGuard(k, std::env::var(k).ok()))
            .collect();
        std::env::set_var("XDG_CONFIG_HOME", td.path());

        let cfg = ResolvedConfig::from_env().expect("resolve config");
        let reader = crate::commands::fetch::config_dir_utf8()
            .expect("reader resolves")
            .join("doiget")
            .join("config.toml");
        assert_eq!(
            cfg.config_path, reader,
            "doctor must validate the file the reader loads"
        );
        assert!(
            cfg.config_path.as_str().starts_with(
                camino::Utf8Path::from_path(td.path())
                    .expect("utf-8 tempdir")
                    .as_str()
            ),
            "XDG_CONFIG_HOME must win on every platform; got {}",
            cfg.config_path
        );
    }

    #[test]
    #[serial_test::serial]
    fn from_env_overrides_via_env() {
        let _g = unset_all_doiget_config_env();
        // Use a platform-appropriate absolute path so Utf8PathBuf::try_from
        // succeeds on Windows too (where "/tmp/foo" is a relative path on
        // the current drive — still UTF-8, still fine for this assertion).
        let _override = EnvGuard::set("DOIGET_STORE_ROOT", "/tmp/foo");
        let cfg = ResolvedConfig::from_env().expect("config resolves on test host");
        assert_eq!(cfg.store_root.as_str(), "/tmp/foo");
    }

    /// Issue #142: `config show` MUST report the same `log_path` the
    /// provenance-log writer uses. The writer keys off `DOIGET_LOG_PATH`
    /// (the only log env var documented in `docs/CONFIG.md` §4); the
    /// resolver must do the same, and `log_dir` must be that path's
    /// parent — never an independently-resolved (and divergent) value.
    #[test]
    #[serial_test::serial]
    fn log_path_follows_doiget_log_path_env() {
        let _g = unset_all_doiget_config_env();
        let _override = EnvGuard::set("DOIGET_LOG_PATH", "/var/lib/doiget/access.jsonl");
        let cfg = ResolvedConfig::from_env().expect("config resolves on test host");
        assert_eq!(
            cfg.log_path.as_str(),
            "/var/lib/doiget/access.jsonl",
            "config show must echo DOIGET_LOG_PATH verbatim (issue #142)"
        );
        assert_eq!(
            cfg.log_dir.as_str(),
            "/var/lib/doiget",
            "log_dir must be derived from log_path's parent so the two cannot drift"
        );
    }

    // ── #408: `config init` ──────────────────────────────────────────────

    /// The template's job is to document the keys that fail *silently* on a
    /// default install. If one is ever dropped from it, the file stops being
    /// the answer to #408 while still looking fine.
    #[test]
    fn template_documents_every_silently_defaulting_key() {
        let t = config_template();
        for key in [
            "[store]",
            "root =",
            "unpaywall_email",
            "trust_academic_repos",
            "trust_oa_registries",
            "[[network.additional_hosts]]",
        ] {
            assert!(t.contains(key), "template must mention {key}");
        }
        // ADR-0036 / ADR-0037 are the two non-obvious defaults; the template
        // must say what they are, not merely name the keys.
        assert!(
            t.contains("CURRENT WORKING DIRECTORY"),
            "store root default"
        );
        assert!(
            t.contains("doiget@localhost"),
            "non-polite pool consequence"
        );
        assert!(t.contains("DOAJ needs no flag"), "post-ADR-0037 accuracy");
    }

    /// Every line must be inert: writing live values would mean `init`
    /// silently changed behaviour just by being run.
    #[test]
    fn template_is_entirely_commented_out() {
        for line in config_template().lines() {
            let t = line.trim();
            if t.is_empty() || t.starts_with('#') {
                continue;
            }
            assert!(
                t.starts_with('[') && t.ends_with(']') && !t.starts_with("[["),
                "only bare section headers may be live; found: {line:?}"
            );
        }
    }

    /// The template must round-trip as TOML — a malformed one would be
    /// written happily and only fail on the user's next command.
    #[test]
    fn template_parses_as_toml() {
        let v: toml::Value = toml::from_str(config_template()).expect("template is valid TOML");
        // With everything commented out it must carry no live keys beyond the
        // empty section tables.
        for (name, tbl) in v.as_table().expect("table") {
            assert!(
                tbl.as_table().expect("section").is_empty(),
                "section [{name}] must be empty in the template"
            );
        }
    }

    // ── #407: probe classification ───────────────────────────────────────

    /// The load-bearing case. A publisher WAF answers a scripted client
    /// with `202 Accepted` and an empty body. Status alone reads that as
    /// success and sends the user off to debug a subscription that is not
    /// the problem — the measurement in #407 was exactly
    /// `status=202 body=0 bytes` from a subscribing university address.
    #[test]
    fn empty_2xx_body_is_a_bot_challenge_not_a_success() {
        assert_eq!(
            ProbeVerdict::classify(202, 0),
            ProbeVerdict::BotChallenge { status: 202 }
        );
        assert_eq!(
            ProbeVerdict::classify(200, 0),
            ProbeVerdict::BotChallenge { status: 200 },
            "an empty 200 is the same holding response wearing a different code"
        );
        assert!(
            ProbeVerdict::classify(202, 0)
                .render()
                .contains("bot challenge"),
            "the verdict must name the diagnosis, not just the status"
        );
    }

    #[test]
    fn non_empty_2xx_is_ok() {
        assert_eq!(
            ProbeVerdict::classify(200, 1234),
            ProbeVerdict::Ok {
                status: 200,
                bytes: 1234
            }
        );
    }

    /// 401/403 is a different diagnosis from a challenge: the host talked
    /// to us and declined, so the next step is credentials, not a browser.
    #[test]
    fn auth_statuses_are_refused_not_challenged() {
        for code in [401u16, 403] {
            assert_eq!(
                ProbeVerdict::classify(code, 0),
                ProbeVerdict::Refused { status: code },
                "{code} must not be misread as a bot challenge"
            );
        }
        assert_eq!(
            ProbeVerdict::classify(404, 0),
            ProbeVerdict::Status { status: 404 }
        );
    }

    /// Every verdict renders something a user can act on; none is blank.
    #[test]
    fn every_verdict_renders_non_empty_advice() {
        let all = [
            ProbeVerdict::Ok {
                status: 200,
                bytes: 1,
            },
            ProbeVerdict::BotChallenge { status: 202 },
            ProbeVerdict::Refused { status: 403 },
            ProbeVerdict::Status { status: 500 },
            ProbeVerdict::NotAllowlisted,
            ProbeVerdict::Unreachable {
                reason: "dns".into(),
            },
        ];
        for v in &all {
            assert!(!v.render().trim().is_empty(), "{v:?} rendered empty");
        }
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn doctor_fails_without_contact_email() {
        // Issue #149: a failing doctor is "missing config" → exit 2.
        // The human-readable line moved to stderr; the error now carries
        // a `CliExit(2)` rather than a Display-formatted anyhow string.
        let _g = unset_all_doiget_config_env();
        let err = run(
            "doctor".into(),
            crate::commands::output::OutputMode::Human,
            false,
            false,
        )
        .await
        .expect_err("doctor should fail when DOIGET_CONTACT_EMAIL is unset");
        let cli_exit = err
            .downcast_ref::<CliExit>()
            .expect("failing doctor must carry a CliExit (issue #149)");
        assert_eq!(
            cli_exit.0, 2,
            "missing/invalid config is misuse → exit 2, not the generic exit 1"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn doctor_passes_with_contact_email() {
        let _g = unset_all_doiget_config_env();
        let _email = EnvGuard::set("DOIGET_CONTACT_EMAIL", "alice@example.org");
        // config_dir() and the cwd resolve to real, existing parents on every
        // supported test host (store root defaults to <cwd>/papers, ADR-0036).
        run(
            "doctor".into(),
            crate::commands::output::OutputMode::Human,
            false,
            false,
        )
        .await
        .expect("doctor should pass with contact email + valid config dir and cwd");
    }

    /// ADR-0028 D2: a malformed `<config_dir>/doiget/config.toml`
    /// causes `doiget config doctor` to FAIL (exit 2). Linux-only
    /// because `dirs::config_dir()` resolves differently on each
    /// platform:
    ///   - Linux: `$XDG_CONFIG_HOME` or `$HOME/.config` (env-driven,
    ///     testable).
    ///   - macOS: `~/Library/Application Support` (Known Folder via
    ///     `NSSearchPathForDirectoriesInDomains`, ignores
    ///     `XDG_CONFIG_HOME`).
    ///   - Windows: `%FOLDERID_RoamingAppData%` (Known Folder API,
    ///     ignores `APPDATA` env in child processes via
    ///     `assert_cmd`).
    /// The malformed-config FAIL path is platform-independent; this
    /// test covers the wiring on the one platform where it CAN be
    /// exercised in a hermetic test.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[serial_test::serial]
    async fn doctor_fails_with_malformed_user_extension_config() {
        let _g = unset_all_doiget_config_env();
        let _email = EnvGuard::set("DOIGET_CONTACT_EMAIL", "alice@example.org");

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let cfg_root = camino::Utf8Path::from_path(tmp.path()).expect("utf8 tempdir");
        let doiget_dir = cfg_root.join("doiget");
        std::fs::create_dir_all(doiget_dir.as_std_path()).expect("mk dir");
        let config_toml = doiget_dir.join("config.toml");
        // Empty `host` value triggers `PatternError::Empty`, which
        // the doctor surfaces as a FAIL. `note` is valid TOML so the
        // top-level parse succeeds — only the pattern validation
        // path produces the error we're pinning.
        std::fs::write(
            config_toml.as_std_path(),
            "[[network.additional_hosts]]\nhost = \"\"\n",
        )
        .expect("write config.toml");

        // `fetch::config_dir_utf8()` — which `ResolvedConfig` now shares
        // with the reader — honors `XDG_CONFIG_HOME` first on every
        // platform, so pointing it at our tempdir routes
        // `cfg.config_path` to our crafted file.
        let _x = EnvGuard::set("XDG_CONFIG_HOME", cfg_root.as_str());

        let err = run(
            "doctor".into(),
            crate::commands::output::OutputMode::Human,
            false,
            false,
        )
        .await
        .expect_err("doctor should fail when user-extension config is malformed");
        let cli_exit = err
            .downcast_ref::<CliExit>()
            .expect("failing doctor must carry a CliExit");
        assert_eq!(cli_exit.0, 2);
    }

    /// Issue #322: `check` must emit a `tip:` line to stderr when the
    /// check fails and a tip is provided. Passing `ok=true` must NOT
    /// emit the tip line even when one is supplied.
    #[test]
    fn check_emits_tip_on_failure_only() {
        let mut flag = true;
        // Passing check — tip must be swallowed.
        check("passing check", true, Some("should not appear"), &mut flag);
        assert!(flag, "all_ok must stay true for a passing check");

        // Failing check with tip — all_ok must flip.
        check(
            "failing check",
            false,
            Some("set DOIGET_CONTACT_EMAIL"),
            &mut flag,
        );
        assert!(!flag, "all_ok must flip to false on a failing check");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn unknown_action_errors() {
        // Issue #149: an unknown action is clear argument misuse →
        // `docs/ERRORS.md` §4 exit 2. The descriptive line moved to
        // stderr; the error carries `CliExit(2)`.
        let _g = unset_all_doiget_config_env();
        let err = run(
            "bogus".into(),
            crate::commands::output::OutputMode::Human,
            false,
            false,
        )
        .await
        .expect_err("bogus action should error");
        let cli_exit = err
            .downcast_ref::<CliExit>()
            .expect("unknown config action must carry a CliExit (issue #149)");
        assert_eq!(
            cli_exit.0, 2,
            "unknown config action is misuse → exit 2, not the generic exit 1"
        );
    }
    /// Build an isolated config dir containing `config.toml` with `body`.
    fn config_home_with(body: &str) -> (tempfile::TempDir, camino::Utf8PathBuf) {
        let td = tempfile::TempDir::new().expect("tempdir");
        let root = camino::Utf8PathBuf::try_from(td.path().to_path_buf()).expect("utf-8 tempdir");
        std::fs::create_dir_all(root.join("doiget").as_std_path()).expect("mkdir");
        std::fs::write(root.join("doiget").join("config.toml").as_std_path(), body)
            .expect("write config");
        (td, root)
    }

    /// #441: the rung that was missing. `[store] root` must beat the cwd
    /// default.
    ///
    /// The assertion that matters is the NEGATIVE one. `store_root ==
    /// <configured>` alone would also pass if the config were ignored and
    /// the test happened to run from the configured directory — which is
    /// exactly how the bug hid: a user testing from `$HOME` with
    /// `root = "$HOME/papers"` sees the right answer for the wrong reason.
    #[test]
    #[serial_test::serial]
    fn store_root_in_config_beats_the_cwd_default() {
        let _g = unset_all_doiget_config_env();
        let lib_td = tempfile::TempDir::new().expect("tempdir");
        let library = camino::Utf8PathBuf::try_from(lib_td.path().to_path_buf())
            .expect("utf-8 tempdir")
            .as_str()
            .replace('\u{5c}', "/");
        let (_cfg_td, cfg_root) = config_home_with(&format!("[store]\nroot = \"{library}\"\n"));

        let _x = EnvGuard::set("XDG_CONFIG_HOME", cfg_root.as_str());
        let cfg = ResolvedConfig::from_env().expect("config resolves");

        let cwd_default = camino::Utf8PathBuf::try_from(std::env::current_dir().expect("cwd"))
            .expect("utf-8 cwd")
            .join("papers");
        assert_ne!(
            cfg.store_root, cwd_default,
            "the config value was ignored and the cwd default answered instead"
        );
        assert_eq!(
            cfg.store_root.as_str().replace('\u{5c}', "/"),
            library,
            "[store] root must win over the cwd default (ADR-0036 rung 2)"
        );
        assert_eq!(
            cfg.store_root_source,
            super::super::StoreRootSource::ConfigFile.label(),
            "doctor must attribute it to the config file"
        );
    }

    /// The rung ABOVE it still wins. Adding rung 2 must not demote the env
    /// var, which is also how `--store-root` is applied.
    #[test]
    #[serial_test::serial]
    fn env_beats_store_root_in_config() {
        let _g = unset_all_doiget_config_env();
        let (_cfg_td, cfg_root) = config_home_with("[store]\nroot = \"/from/config\"\n");

        let _x = EnvGuard::set("XDG_CONFIG_HOME", cfg_root.as_str());
        let _e = EnvGuard::set("DOIGET_STORE_ROOT", "/from/env");
        let cfg = ResolvedConfig::from_env().expect("config resolves");

        assert_eq!(cfg.store_root.as_str(), "/from/env");
        assert_eq!(
            cfg.store_root_source,
            super::super::StoreRootSource::Env.label()
        );
    }

    /// A blank value means "unset", not "the empty path" — otherwise it
    /// would resolve to the filesystem root.
    #[test]
    #[serial_test::serial]
    fn blank_store_root_in_config_falls_through_to_the_default() {
        let _g = unset_all_doiget_config_env();
        let (_cfg_td, cfg_root) = config_home_with("[store]\nroot = \"   \"\n");

        let _x = EnvGuard::set("XDG_CONFIG_HOME", cfg_root.as_str());
        let cfg = ResolvedConfig::from_env().expect("config resolves");

        assert_eq!(
            cfg.store_root_source,
            super::super::StoreRootSource::CwdDefault.label(),
            "a blank root must not be treated as a configured value"
        );
    }
}
