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

use anyhow::Result;
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
    /// Root of the on-disk paper store. Default: `$HOME/papers`.
    pub store_root: Utf8PathBuf,
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
    /// Errors only if neither a home directory nor a config directory can
    /// be determined for the current user (e.g. an unknown / locked-down
    /// platform); on every realistic POSIX or Windows host this returns
    /// `Ok` even with no `DOIGET_*` env vars set.
    pub fn from_env() -> Result<Self> {
        // `dirs::home_dir()` / `dirs::config_dir()` return `std::path::PathBuf`;
        // hoist them into `Utf8PathBuf` immediately at the OS boundary so the
        // rest of the function (and the public struct) stays UTF-8-only per
        // the workspace `disallowed-types` clippy rule. A non-UTF-8 home dir
        // is exotic and unsupported; surface it as an explicit error.
        let home =
            Utf8PathBuf::try_from(dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?)?;
        let cfg = Utf8PathBuf::try_from(
            dirs::config_dir().ok_or_else(|| anyhow::anyhow!("no config dir"))?,
        )?;

        let store_root = std::env::var("DOIGET_STORE_ROOT")
            .map(Utf8PathBuf::from)
            .unwrap_or_else(|_| home.join("papers"));

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
pub fn run(action: String, mode: super::output::OutputMode) -> Result<()> {
    // `mode` honors ADR-0017: `Quiet` suppresses the TOML dump (`show`)
    // and the path println! (`path`); `doctor` is unaffected because its
    // per-check output is on stderr and only the failure/success exit
    // code is the user-visible signal (#203). Json body for `show` is
    // tracked in #204.
    let cfg = ResolvedConfig::from_env()?;
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
            check(
                "store_root parent exists",
                cfg.store_root.parent().map(|p| p.exists()).unwrap_or(true),
                &mut all_ok,
            );
            check(
                "log_dir parent exists",
                cfg.log_dir.parent().map(|p| p.exists()).unwrap_or(true),
                &mut all_ok,
            );
            check(
                "contact_email set",
                cfg.contact_email.is_some(),
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
                Ok(cfg_ext) => check(
                    &format!(
                        "user-extension hosts loaded: {} (trust_academic_repos={})",
                        cfg_ext.additional_hosts.len(),
                        cfg_ext.trust_academic_repos
                    ),
                    true,
                    &mut all_ok,
                ),
                Err(e) => check(
                    &format!("user-extension config invalid: {e}"),
                    false,
                    &mut all_ok,
                ),
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
                "error: unknown config action: {other}; expected `show` / `path` / `doctor`"
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
#[allow(clippy::print_stderr)]
fn check(label: &str, ok: bool, all_ok: &mut bool) {
    let mark = if ok { "[ ok ]" } else { "[FAIL]" };
    eprintln!("{mark} {label}");
    if !ok {
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
    fn from_env_uses_home_default_when_unset() {
        let _g = unset_all_doiget_config_env();
        let cfg = ResolvedConfig::from_env().expect("home dir must resolve on test host");
        assert!(
            cfg.store_root.as_str().ends_with("papers"),
            "store_root should fall back to <home>/papers when DOIGET_STORE_ROOT is unset; got {}",
            cfg.store_root
        );
        assert_eq!(cfg.contact_email, None);
        assert_eq!(cfg.unpaywall_email, None);
    }

    #[test]
    #[serial_test::serial]
    fn from_env_overrides_via_env() {
        let _g = unset_all_doiget_config_env();
        // Use a platform-appropriate absolute path so Utf8PathBuf::try_from
        // succeeds on Windows too (where "/tmp/foo" is a relative path on
        // the current drive — still UTF-8, still fine for this assertion).
        let _override = EnvGuard::set("DOIGET_STORE_ROOT", "/tmp/foo");
        let cfg = ResolvedConfig::from_env().expect("home dir must resolve on test host");
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
        let cfg = ResolvedConfig::from_env().expect("home dir must resolve on test host");
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

    #[test]
    #[serial_test::serial]
    fn doctor_fails_without_contact_email() {
        // Issue #149: a failing doctor is "missing config" → exit 2.
        // The human-readable line moved to stderr; the error now carries
        // a `CliExit(2)` rather than a Display-formatted anyhow string.
        let _g = unset_all_doiget_config_env();
        let err = run("doctor".into(), crate::commands::output::OutputMode::Human)
            .expect_err("doctor should fail when DOIGET_CONTACT_EMAIL is unset");
        let cli_exit = err
            .downcast_ref::<CliExit>()
            .expect("failing doctor must carry a CliExit (issue #149)");
        assert_eq!(
            cli_exit.0, 2,
            "missing/invalid config is misuse → exit 2, not the generic exit 1"
        );
    }

    #[test]
    #[serial_test::serial]
    fn doctor_passes_with_contact_email() {
        let _g = unset_all_doiget_config_env();
        let _email = EnvGuard::set("DOIGET_CONTACT_EMAIL", "alice@example.org");
        // home_dir() / config_dir() resolve to real, existing parents on
        // every supported test host (CI runners always have $HOME).
        run("doctor".into(), crate::commands::output::OutputMode::Human)
            .expect("doctor should pass with contact email + real home dir");
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
    #[test]
    #[serial_test::serial]
    fn doctor_fails_with_malformed_user_extension_config() {
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

        // POSIX `dirs::config_dir()` honors `XDG_CONFIG_HOME` first,
        // so pointing it at our tempdir routes `cfg.config_path` to
        // our crafted file.
        let _x = EnvGuard::set("XDG_CONFIG_HOME", cfg_root.as_str());

        let err = run("doctor".into(), crate::commands::output::OutputMode::Human)
            .expect_err("doctor should fail when user-extension config is malformed");
        let cli_exit = err
            .downcast_ref::<CliExit>()
            .expect("failing doctor must carry a CliExit");
        assert_eq!(cli_exit.0, 2);
    }

    #[test]
    #[serial_test::serial]
    fn unknown_action_errors() {
        // Issue #149: an unknown action is clear argument misuse →
        // `docs/ERRORS.md` §4 exit 2. The descriptive line moved to
        // stderr; the error carries `CliExit(2)`.
        let _g = unset_all_doiget_config_env();
        let err = run("bogus".into(), crate::commands::output::OutputMode::Human)
            .expect_err("bogus action should error");
        let cli_exit = err
            .downcast_ref::<CliExit>()
            .expect("unknown config action must carry a CliExit (issue #149)");
        assert_eq!(
            cli_exit.0, 2,
            "unknown config action is misuse → exit 2, not the generic exit 1"
        );
    }
}
