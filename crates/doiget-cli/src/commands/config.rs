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

use anyhow::{bail, Result};
use camino::Utf8PathBuf;

/// Snapshot of the env-var + default-fallback config that `doiget` would
/// use on the current machine.
///
/// Phase 1 surface: env vars only (`DOIGET_STORE_ROOT`, `DOIGET_LOG_DIR`,
/// `DOIGET_CONTACT_EMAIL`, `DOIGET_UNPAYWALL_EMAIL`) layered over
/// XDG / known-folder defaults. Phase 2 will layer the user config.toml
/// underneath the env vars per `docs/CONFIG.md` §1.
#[derive(Debug, serde::Serialize)]
pub struct ResolvedConfig {
    /// Root of the on-disk paper store. Default: `$HOME/papers`.
    pub store_root: Utf8PathBuf,
    /// Directory holding doiget's append-only logs.
    pub log_dir: Utf8PathBuf,
    /// JSON-Lines provenance log file path. Always `<log_dir>/access.jsonl`.
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
        let log_dir = std::env::var("DOIGET_LOG_DIR")
            .map(Utf8PathBuf::from)
            .unwrap_or_else(|_| cfg.join("doiget"));

        let log_path = log_dir.join("access.jsonl");
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
pub fn run(action: String) -> Result<()> {
    let cfg = ResolvedConfig::from_env()?;
    match action.as_str() {
        "show" => {
            let s = toml::to_string_pretty(&cfg)?;
            print!("{s}");
        }
        "path" => {
            println!("{}", cfg.config_path);
        }
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
            // Trying to actually create the dirs would have side-effects;
            // keep doctor read-only and just check existence of parents.
            if !all_ok {
                bail!("config doctor: one or more checks failed");
            }
        }
        other => bail!("unknown config action: {other}; expected `show` / `path` / `doctor`"),
    }
    Ok(())
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
            "DOIGET_LOG_DIR",
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

    #[test]
    #[serial_test::serial]
    fn doctor_fails_without_contact_email() {
        let _g = unset_all_doiget_config_env();
        let err = run("doctor".into())
            .expect_err("doctor should fail when DOIGET_CONTACT_EMAIL is unset");
        let msg = format!("{err}");
        assert!(
            msg.contains("config doctor"),
            "unexpected error message: {msg}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn doctor_passes_with_contact_email() {
        let _g = unset_all_doiget_config_env();
        let _email = EnvGuard::set("DOIGET_CONTACT_EMAIL", "alice@example.org");
        // home_dir() / config_dir() resolve to real, existing parents on
        // every supported test host (CI runners always have $HOME).
        run("doctor".into()).expect("doctor should pass with contact email + real home dir");
    }

    #[test]
    #[serial_test::serial]
    fn unknown_action_errors() {
        let _g = unset_all_doiget_config_env();
        let err = run("bogus".into()).expect_err("bogus action should error");
        let msg = format!("{err}");
        assert!(
            msg.contains("unknown config action"),
            "unexpected error message: {msg}"
        );
    }
}
