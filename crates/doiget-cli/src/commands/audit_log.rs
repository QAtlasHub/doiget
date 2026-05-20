//! `doiget audit-log --verify` — re-validate the SHA-256 hash chain.
//!
//! The provenance log on disk is JSON Lines + a SHA-256 hash chain
//! (`docs/PROVENANCE_LOG.md` §4). This subcommand recomputes the chain via
//! [`doiget_core::provenance::verify`] and reports any mismatches. It is the
//! ONLY tool that detects log tampering; per `docs/SECURITY.md` §1.8 the
//! hash chain is "best-effort tamper-evident" — it does not prevent
//! rewriting, only makes rewrites detectable.
//!
//! # Phase 1 surface
//!
//! Only `--verify` is implemented in Phase 1. `--since`, `--source`, and
//! `--session` (per `docs/PROVENANCE_LOG.md` §7) ship in later phases. The
//! current entry point bails out with a clear message if `--verify` is
//! omitted.
//!
//! # Output (stdout)
//!
//! Three header lines followed by zero or more issue lines:
//!
//! ```text
//! audit-log verify: 42 rows
//!   ok:     41
//!   issues: 1
//!   line 17: this-hash — this_hash mismatch: stored=…, recomputed=…
//! ```
//!
//! On a clean log the issue list is empty and the process exits zero. With
//! one or more issues the process exits non-zero so shell pipelines can
//! treat tampering as a hard failure.
//!
//! `print_stdout` is denied workspace-wide for MCP stdio safety (ADR-0001 /
//! `docs/SECURITY.md` §3). `audit-log` is a human-facing CLI surface, never
//! invoked from inside an MCP session, so we use `writeln!` against an
//! explicit `stdout().lock()` — the sanctioned escape hatch.

use std::io::Write;

use anyhow::{bail, Context, Result};
use camino::Utf8PathBuf;

use doiget_core::provenance::{verify_all, VerifyIssueKind};

use super::fetch::CliExit;

/// Stderr sink for the `docs/ERRORS.md` §3 human-error lines. Mirrors
/// the `print_err` helper in `commands::fetch`; the localized `#[allow]`
/// is the minimal intervention for the workspace `clippy::print_stderr`
/// lint.
#[allow(clippy::print_stderr)]
fn print_err(args: std::fmt::Arguments<'_>) {
    eprintln!("{args}");
}

/// Run the `audit-log` subcommand.
///
/// `verify_flag` corresponds to the `--verify` clap flag. Phase 1 requires
/// it: any other invocation bails with an explanatory error.
///
/// Returns `Ok(())` on a clean log (zero issues), or an error whose Display
/// summarizes the failure when one or more chain issues are detected. The
/// per-issue breakdown is always written to stdout BEFORE returning, so a
/// caller scripting this subcommand can inspect both the structured stdout
/// and the non-zero exit code.
pub fn run(verify_flag: bool, mode: super::output::OutputMode) -> Result<()> {
    // `mode` honors ADR-0017: `Quiet` suppresses the informational stdout
    // (header + per-segment summary + per-issue lines). The verification
    // itself still runs and the non-zero exit on issues is still raised,
    // so quiet pipelines see the failure via exit code (#203). Rich Json
    // body is tracked in #204.
    if !verify_flag {
        // Issue #149: a missing required flag is argument misuse →
        // `docs/ERRORS.md` §4 exit 2, NOT the generic exit 1 a bare
        // `bail!` produced. The human-readable line is emitted here (so
        // `main` does not reprint it) and only the exit code is carried.
        print_err(format_args!(
            "error: doiget audit-log: --verify is required (Phase 1 ships only \
             --verify; --since / --source / --session land later)"
        ));
        return Err(anyhow::Error::new(CliExit(2)));
    }

    let log_path = resolve_log_path()?;
    // §6: verify the full history — every rotated `.gz` segment
    // (oldest→newest) plus the current `access.log`. Each segment is its
    // own GENESIS-rooted chain; they are verified independently.
    let segments = verify_all(&log_path)
        .with_context(|| format!("failed to read provenance log at {log_path}"))?;

    let total_rows: usize = segments.iter().map(|(_, r)| r.total_rows).sum();
    let total_ok: usize = segments.iter().map(|(_, r)| r.ok_rows).sum();
    let total_issues: usize = segments.iter().map(|(_, r)| r.errors.len()).sum();
    let multi = segments.len() > 1;

    // `print_stdout` is workspace-deny for MCP stdio safety — see module docs.
    // The `audit-log` CLI is the explicit human-facing channel; locking
    // `stdout()` and using `writeln!` is the sanctioned way to emit lines.
    // `Quiet` mode short-circuits the emit block but keeps the bail!() at
    // the bottom so failure-on-issues exit codes still fire (#203).
    if mode == super::output::OutputMode::Json {
        // #204: structured report. Schema:
        //   {"total_rows", "total_ok", "total_issues",
        //    "segments": [{"name", "rows", "ok", "issues"}],
        //    "issues":   [{"segment", "line", "kind", "message"}]}
        // Single value (NOT JSON-Lines) so the whole report parses with
        // one `JSON.parse` call. Sibling commands' shapes are sibling
        // schemas — see commands/{list_recent,search,config,info,provenance}.
        #[derive(serde::Serialize)]
        struct SegmentSummary<'a> {
            name: &'a str,
            rows: usize,
            ok: usize,
            issues: usize,
        }
        #[derive(serde::Serialize)]
        struct IssueRecord<'a> {
            segment: &'a str,
            line: usize,
            kind: &'static str,
            message: &'a str,
        }
        #[derive(serde::Serialize)]
        struct Report<'a> {
            total_rows: usize,
            total_ok: usize,
            total_issues: usize,
            segments: Vec<SegmentSummary<'a>>,
            issues: Vec<IssueRecord<'a>>,
        }

        let mut segs: Vec<SegmentSummary> = Vec::with_capacity(segments.len());
        let mut issues: Vec<IssueRecord> = Vec::new();
        for (path, report) in &segments {
            let seg = path.file_name().unwrap_or(path.as_str());
            segs.push(SegmentSummary {
                name: seg,
                rows: report.total_rows,
                ok: report.ok_rows,
                issues: report.errors.len(),
            });
            for issue in &report.errors {
                let kind = match issue.kind {
                    VerifyIssueKind::ParseError => "parse",
                    VerifyIssueKind::PrevHashMismatch => "prev-hash",
                    VerifyIssueKind::ThisHashMismatch => "this-hash",
                    VerifyIssueKind::SequenceJump => "sequence",
                    _ => "other",
                };
                issues.push(IssueRecord {
                    segment: seg,
                    line: issue.line,
                    kind,
                    message: &issue.message,
                });
            }
        }
        let report = Report {
            total_rows,
            total_ok,
            total_issues,
            segments: segs,
            issues,
        };
        let s =
            serde_json::to_string_pretty(&report).context("serialize audit-log report to JSON")?;
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        writeln!(out, "{s}").context("failed to write audit-log JSON to stdout")?;
    } else if mode != super::output::OutputMode::Quiet {
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        // Aggregate header — byte-identical to the pre-rotation output when
        // there is a single segment (back-compat with audit_log_e2e.rs).
        writeln!(out, "audit-log verify: {total_rows} rows")
            .context("failed to write header to stdout")?;
        writeln!(out, "  ok:     {total_ok}").context("failed to write ok-row count to stdout")?;
        writeln!(out, "  issues: {total_issues}")
            .context("failed to write issue count to stdout")?;

        for (path, report) in &segments {
            let seg = path.file_name().unwrap_or(path.as_str());
            if multi {
                writeln!(
                    out,
                    "  segment {}: {} rows, {} ok, {} issues",
                    seg,
                    report.total_rows,
                    report.ok_rows,
                    report.errors.len()
                )
                .context("failed to write segment summary to stdout")?;
            }
            for issue in &report.errors {
                // `VerifyIssueKind` is `#[non_exhaustive]` (forward-compat for
                // future variants like `SessionIdChange`); the wildcard arm uses
                // a generic label so older CLI builds keep producing well-formed
                // output even when run against a newer core that adds variants.
                let kind = match issue.kind {
                    VerifyIssueKind::ParseError => "parse",
                    VerifyIssueKind::PrevHashMismatch => "prev-hash",
                    VerifyIssueKind::ThisHashMismatch => "this-hash",
                    VerifyIssueKind::SequenceJump => "sequence",
                    _ => "other",
                };
                if multi {
                    writeln!(
                        out,
                        "  [{}] line {}: {} — {}",
                        seg, issue.line, kind, issue.message
                    )
                } else {
                    writeln!(out, "  line {}: {} — {}", issue.line, kind, issue.message)
                }
                .context("failed to write issue line to stdout")?;
            }
        }
    }

    if total_issues == 0 {
        Ok(())
    } else {
        bail!(
            "audit-log: {} chain issue(s) detected across {} segment(s) — see stdout for details",
            total_issues,
            segments.len()
        )
    }
}

/// Resolve the on-disk provenance-log path.
///
/// Resolution order (subset of `docs/CONFIG.md` §4 — full CLI-flag /
/// config-file resolution lands with the `config` subcommand):
///
/// 1. `DOIGET_LOG_PATH` env var, if set and non-empty.
/// 2. `<config_dir>/doiget/access.jsonl` (cross-platform via `dirs::config_dir`).
///
/// This resolution agrees with the provenance-log *writer*
/// (`commands::fetch::resolve_log_path` / `commands::config::ResolvedConfig`):
/// since issue #142 all of them key off `DOIGET_LOG_PATH` (the only log env
/// var `docs/CONFIG.md` §4 / `docs/PROVENANCE_LOG.md` §1 documents), falling
/// back to `<config_dir>/doiget/access.jsonl`. The previously read,
/// undocumented `DOIGET_LOG_DIR` was removed in #142, so reader and writer
/// can never disagree. Tests rely on `DOIGET_LOG_PATH` to point at a
/// per-test tempdir.
fn resolve_log_path() -> Result<Utf8PathBuf> {
    if let Ok(s) = std::env::var("DOIGET_LOG_PATH") {
        if !s.is_empty() {
            return Ok(Utf8PathBuf::from(s));
        }
    }
    // Fall back to the same convention as `ResolvedConfig::from_env`:
    // <config_dir>/doiget/access.jsonl.
    let cfg = Utf8PathBuf::try_from(
        dirs::config_dir().ok_or_else(|| anyhow::anyhow!("no config dir on this platform"))?,
    )
    .context("config directory path is not valid UTF-8")?;
    Ok(cfg.join("doiget").join("access.jsonl"))
}

// ---------------------------------------------------------------------------
// Tests — env-mutating, serialized via serial_test (DOIGET_LOG_PATH is process
// global). Each test scopes its env mutation to a TempDir-backed log file.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

    use super::*;

    use camino::Utf8PathBuf;
    use tempfile::TempDir;

    use doiget_core::provenance::{Capability, LogEvent, LogResult, ProvenanceLog, RowInput};

    /// RAII guard that captures the prior value of an env var on
    /// construction and restores it on drop. Mirrors the convention in
    /// `crates/doiget-cli/src/commands/config.rs::tests`.
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

    fn tmp_dir_utf8(dir: &TempDir) -> Utf8PathBuf {
        Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("temp dir path must be UTF-8")
    }

    #[test]
    #[serial_test::serial]
    fn run_without_verify_flag_errors() {
        // Even with no log file at all, the absence of --verify is a
        // user-error guard that fires before we touch the disk.
        //
        // Issue #149: missing-required-flag is argument misuse →
        // `docs/ERRORS.md` §4 exit 2. The human-readable line is now
        // written to stderr (not into the error's Display), and the
        // returned error carries a `CliExit(2)` so `main` exits 2
        // instead of the old generic 1.
        let _g = EnvGuard::unset("DOIGET_LOG_PATH");
        let err = run(false, crate::commands::output::OutputMode::Human)
            .expect_err("--verify must be required in Phase 1");
        let cli_exit = err
            .downcast_ref::<CliExit>()
            .expect("missing --verify must carry a CliExit (issue #149)");
        assert_eq!(
            cli_exit.0, 2,
            "missing required flag is misuse → exit 2, not the generic exit 1"
        );
    }

    #[test]
    #[serial_test::serial]
    fn run_verifies_clean_log() {
        // Build a small valid log via the real writer, point
        // DOIGET_LOG_PATH at it, run --verify; expect success.
        let dir = TempDir::new().expect("tmp");
        let path = tmp_dir_utf8(&dir).join("access.jsonl");

        let log = ProvenanceLog::open(path.clone(), "01JCKZ7Q0000000000000000AB".to_string())
            .expect("open log");
        for _ in 0..3 {
            log.append(RowInput {
                event: LogEvent::Fetch,
                result: LogResult::Ok,
                capability: Capability::Oa,
                ref_: None,
                source: None,
                error_code: None,
                size_bytes: None,
                license: None,
                store_path: None,
                canonical_digest: None,
            })
            .expect("append");
        }
        drop(log);

        let _g = EnvGuard::set("DOIGET_LOG_PATH", path.as_str());
        run(true, crate::commands::output::OutputMode::Human)
            .expect("verify must pass on a clean log");
    }

    #[test]
    #[serial_test::serial]
    fn run_verifies_missing_log_as_clean() {
        // Spec contract: missing log is treated as empty/clean — the bytes
        // that don't exist cannot have been tampered with.
        let dir = TempDir::new().expect("tmp");
        let path = tmp_dir_utf8(&dir).join("never-created.jsonl");
        assert!(!path.exists(), "precondition: log must not exist");

        let _g = EnvGuard::set("DOIGET_LOG_PATH", path.as_str());
        run(true, crate::commands::output::OutputMode::Human)
            .expect("verify must succeed on missing log");
    }
}
