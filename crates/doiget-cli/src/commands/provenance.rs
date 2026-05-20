//! `doiget provenance migrate` — one-shot v1 → v2 migration tool
//! (ADR-0024, `docs/PROVENANCE_LOG.md` §"Schema migration").
//!
//! The migration is **one-shot** (the resulting v2 log re-reading as
//! v2 is a no-op on re-run), **idempotent**, and **dry-runnable** via
//! `--dry-run`. See [`doiget_core::provenance::migrate_v1_to_v2`] for
//! the binding implementation.
//!
//! Resolves the log path the same way [`super::audit_log::run`] does:
//! `DOIGET_LOG_PATH` env, falling back to
//! `<config_dir>/doiget/access.jsonl`.
//!
//! Output goes to stdout via `writeln!` against an explicit
//! `stdout().lock()` — the sanctioned escape hatch for human-facing
//! CLI surfaces (workspace lints deny `print_stdout` to keep MCP
//! stdio safe).

use std::io::Write;

use anyhow::{Context, Result};
use camino::Utf8PathBuf;

use doiget_core::provenance::{migrate_v1_to_v2, MigrationReport};

/// Run the `provenance migrate` subcommand.
///
/// When `dry_run` is `true`, [`migrate_v1_to_v2`] is asked to preview
/// the migration: it counts rows and computes the first-row
/// chain-hash delta without touching disk. When `false`, the live
/// rewrite path runs: the original log is preserved as
/// `<log_path>.v1-backup` and the migrated v2 log is atomically
/// swapped in.
pub fn migrate(dry_run: bool, _mode: super::output::OutputMode) -> Result<()> {
    // `_mode` is threaded per ADR-0017 / #144. Quiet-suppression and
    // a Json body for the migration report are tracked in #203 / #204.
    let log_path = resolve_log_path()?;
    let report = migrate_v1_to_v2(&log_path, dry_run)
        .with_context(|| format!("migrating provenance log at {log_path}"))?;

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    emit_summary(&mut out, &log_path, &report)?;
    Ok(())
}

/// Resolve the on-disk provenance-log path. Mirrors
/// [`super::audit_log::run`]'s resolver so the two subcommands agree
/// on what "the log" means.
fn resolve_log_path() -> Result<Utf8PathBuf> {
    if let Ok(s) = std::env::var("DOIGET_LOG_PATH") {
        if !s.is_empty() {
            return Ok(Utf8PathBuf::from(s));
        }
    }
    let cfg = Utf8PathBuf::try_from(
        dirs::config_dir().ok_or_else(|| anyhow::anyhow!("no config dir on this platform"))?,
    )
    .context("config directory path is not valid UTF-8")?;
    Ok(cfg.join("doiget").join("access.jsonl"))
}

/// Render a one-screen summary of the migration report.
fn emit_summary<W: Write>(
    out: &mut W,
    log_path: &camino::Utf8Path,
    report: &MigrationReport,
) -> Result<()> {
    let mode = if report.dry_run {
        "dry-run (no disk writes)"
    } else {
        "applied"
    };
    writeln!(out, "provenance migrate v1 -> v2: {mode}").context("write summary header")?;
    writeln!(out, "  log path:                 {log_path}").context("write log path")?;
    writeln!(out, "  rows rewritten:           {}", report.rows_rewritten)
        .context("write rows count")?;
    writeln!(
        out,
        "  first-row v1 chain hash:  {}",
        report.first_row_v1_chain_hash
    )
    .context("write v1 anchor")?;
    writeln!(
        out,
        "  first-row v2 chain hash:  {}",
        report.first_row_v2_chain_hash
    )
    .context("write v2 anchor")?;
    if !report.dry_run && report.rows_rewritten > 0 {
        writeln!(out, "  backup preserved at:      {log_path}.v1-backup")
            .context("write backup notice")?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    use camino::Utf8PathBuf;
    use tempfile::TempDir;

    /// RAII guard mirroring the convention used in
    /// `crates/doiget-cli/src/commands/audit_log.rs::tests`.
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
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.prior {
                Some(v) => std::env::set_var(self.var, v),
                None => std::env::remove_var(self.var),
            }
        }
    }

    fn tmp_dir_utf8(dir: &TempDir) -> Utf8PathBuf {
        Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("temp dir path must be UTF-8")
    }

    #[test]
    #[serial_test::serial]
    fn migrate_dry_run_on_missing_log_is_noop() {
        // Missing log + dry_run = empty report, no error.
        let dir = TempDir::new().expect("tmp");
        let path = tmp_dir_utf8(&dir).join("never-created.jsonl");
        assert!(!path.exists(), "precondition");

        let _g = EnvGuard::set("DOIGET_LOG_PATH", path.as_str());
        migrate(true, crate::commands::output::OutputMode::Human)
            .expect("dry-run on missing log must succeed");
    }
}
