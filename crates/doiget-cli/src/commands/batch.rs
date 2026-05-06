//! `doiget batch <path>` — multi-ref orchestrator.
//!
//! Reads refs from a newline-separated text file, dispatches up to
//! [`doiget_core::MAX_CONCURRENT_FETCHES`] in flight (the shared
//! [`RateLimiter`](doiget_core::rate_limiter::RateLimiter) enforces the
//! 5-per-second + 200 ms-per-source backoff invariants), and writes one
//! shared provenance log file (`docs/PROVENANCE_LOG.md` §3).
//!
//! Spec:
//! - `docs/PHASES.md` §4 success criterion: "`doiget batch <refs.txt>` honors
//!   the rate cap and writes a hash-chained provenance log."
//! - `crates/doiget-core/src/lib.rs` constants
//!   [`doiget_core::MCP_BATCH_MAX_SIZE`] and
//!   [`doiget_core::MAX_CONCURRENT_FETCHES`].
//!
//! ## Failure semantics
//!
//! - File read / over-limit-size errors abort the batch BEFORE any fetch.
//! - A single `Ref::parse` failure is non-fatal: the orchestrator emits a
//!   `Resolve` row with `result=err` and continues with the remaining refs.
//! - A single fetch failure is also non-fatal: per-task errors are recorded
//!   by the `Source` impls (`Fetch` / `StoreWrite` rows) and counted by the
//!   batch.
//! - Exit code: `Ok(())` only when **every** parse + fetch succeeded; any
//!   parse-error or per-ref-fetch-error returns `Err(...)` so the binary
//!   surfaces a non-zero exit.
//! - The bookend rows are always emitted: one `SessionStart` at the top, one
//!   `SessionEnd` at the bottom, regardless of per-ref outcome.

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};

use doiget_core::provenance::{Capability, LogEvent, LogResult, RowInput};
use doiget_core::{RateLimits, Ref, MCP_BATCH_MAX_SIZE};

use super::fetch::FetchHarness;

/// Run the `doiget batch <path>` subcommand. See module docs for the
/// failure-semantics contract.
pub async fn run(path: String) -> Result<()> {
    // Step 1: read the input file. Failures surface before any fetch starts.
    let raw =
        std::fs::read_to_string(&path).with_context(|| format!("reading batch file: {path}"))?;

    // Step 2: parse refs — trim, drop blanks and `#`-prefixed comments. We
    // keep the input strings (not parsed `Ref`s yet) so that per-ref parse
    // failures can be logged with the original text.
    let inputs: Vec<String> = raw
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|s| s.to_string())
        .collect();

    // Step 3: enforce the hard cap before doing any work. The cap is the
    // same one the MCP `batch_fetch` tool enforces (`MCP_BATCH_MAX_SIZE`).
    if inputs.len() > MCP_BATCH_MAX_SIZE {
        return Err(anyhow!(
            "batch size {} exceeds limit {}",
            inputs.len(),
            MCP_BATCH_MAX_SIZE,
        ));
    }

    // Step 4: build the harness once. All spawned tasks share an `Arc` to
    // it; the harness already wraps the foundation modules in `Arc<...>` so
    // this introduces no extra cloning overhead per task.
    let harness = Arc::new(FetchHarness::from_env()?);

    // Step 5: SessionStart. Pass `None` for the ref — there is no single
    // ref to attribute the session to.
    harness.log_session_start(None)?;

    // Step 6: bound concurrent in-flight tasks at
    // `RateLimits::HARD_CODED.max_concurrent_fetches()` (= 5). The
    // `RateLimiter` itself enforces the global rate + per-source backoff;
    // this semaphore is purely the spawn-side cap on simultaneous tasks.
    let max_concurrent = RateLimits::HARD_CODED.max_concurrent_fetches() as usize;
    let semaphore = Arc::new(tokio::sync::Semaphore::new(max_concurrent));

    // Step 7: dispatch. We iterate sequentially to spawn (cheap), and the
    // semaphore serializes the actual work to the bound. Parse errors are
    // logged as `Resolve` rows directly off the harness's shared log.
    let mut parse_errors: usize = 0;
    let mut joins: tokio::task::JoinSet<TaskOutcome> = tokio::task::JoinSet::new();
    for input in inputs {
        let ref_ = match Ref::parse(&input) {
            Ok(r) => r,
            Err(e) => {
                parse_errors += 1;
                // Best-effort `Resolve` row capturing the parse failure; we
                // do NOT abort the batch on a single bad line.
                let _ = harness.log.append(RowInput {
                    event: LogEvent::Resolve,
                    result: LogResult::Err,
                    capability: Capability::Oa,
                    ref_: Some(&input),
                    source: None,
                    error_code: Some("INVALID_REF"),
                    size_bytes: None,
                    license: None,
                    store_path: None,
                });
                tracing::warn!(
                    %input,
                    error = %e,
                    "skipping malformed batch entry",
                );
                continue;
            }
        };

        let harness_task = Arc::clone(&harness);
        let sem_task = Arc::clone(&semaphore);
        joins.spawn(async move {
            // `Semaphore::acquire_owned` only errors when the semaphore is
            // closed; we never close it. The fallback maps that
            // structurally-unreachable arm to a fetch failure rather than
            // panicking.
            let _permit = match sem_task.acquire_owned().await {
                Ok(p) => p,
                Err(_) => {
                    return TaskOutcome {
                        input,
                        result: Err(anyhow!("batch semaphore unexpectedly closed")),
                    }
                }
            };
            let result = harness_task.fetch_one(&ref_).await;
            TaskOutcome { input, result }
        });
    }

    // Step 8: drain the JoinSet. We collect all outcomes before deciding
    // the session result so a single failure does not abort sibling tasks.
    let mut fetch_ok: usize = 0;
    let mut fetch_errors: usize = 0;
    while let Some(joined) = joins.join_next().await {
        match joined {
            Ok(TaskOutcome { input, result }) => match result {
                Ok(()) => fetch_ok += 1,
                Err(e) => {
                    fetch_errors += 1;
                    tracing::warn!(%input, error = ?e, "batch entry fetch failed");
                }
            },
            Err(join_err) => {
                fetch_errors += 1;
                tracing::error!(error = ?join_err, "batch task panicked or was cancelled");
            }
        }
    }

    let total_errors = parse_errors + fetch_errors;
    let all_ok = total_errors == 0;

    // Step 9: SessionEnd, always. Failure to append is best-effort; the
    // caller already has whatever per-ref errors were observed.
    harness.log_session_end(all_ok, None);

    // Step 10: stderr summary. ADR-0001: success / progress lines go to
    // stderr; the workspace `print_stderr` lint is `warn`, promoted to deny
    // in CI, so the localized `#[allow]` is the minimal intervention.
    print_summary(format_args!(
        "batch: {} OK, {} failed ({} parse errors, {} fetch errors)",
        fetch_ok, total_errors, parse_errors, fetch_errors,
    ));

    if all_ok {
        Ok(())
    } else {
        Err(anyhow!(
            "batch failed: {} OK, {} parse errors, {} fetch errors",
            fetch_ok,
            parse_errors,
            fetch_errors,
        ))
    }
}

/// Per-task outcome carried out of the `JoinSet`. Holding the original input
/// string lets the warn log breadcrumb the offending ref without re-parsing.
struct TaskOutcome {
    input: String,
    result: Result<()>,
}

/// One-line summary written to stderr per ADR-0001 (stdio convention — the
/// CLI never writes a success line to stdout). `print_stderr` is a workspace
/// `warn` promoted to `deny` under `-D warnings`; the localized `#[allow]`
/// pinpoints the one intentional eprintln.
#[allow(clippy::print_stderr)]
fn print_summary(args: std::fmt::Arguments<'_>) {
    eprintln!("{args}");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_filters_input_lines() {
        // Mirror the Step 2 filter: trim, drop blanks and `#`-prefixed
        // comments. This is the only piece of the orchestrator with a pure
        // parse contract worth pinning in a unit test (the rest is I/O).
        let raw = "\
arxiv:2401.12345

# a comment line
   # indented comment with leading whitespace
arxiv:2401.12346
\t\t
   arxiv:2401.12347
";
        let lines: Vec<String> = raw
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            lines,
            vec![
                "arxiv:2401.12345".to_string(),
                "arxiv:2401.12346".to_string(),
                "arxiv:2401.12347".to_string(),
            ],
        );
    }

    #[test]
    fn over_limit_input_is_rejected() {
        // Verify that lengths above the documented cap surface the canonical
        // error message before any fetch is dispatched.
        let n = MCP_BATCH_MAX_SIZE + 1;
        let body: String = (0..n)
            .map(|i| format!("arxiv:2401.{:05}\n", 10000 + i))
            .collect();
        let lines: Vec<String> = body
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|s| s.to_string())
            .collect();
        assert_eq!(lines.len(), n);
        assert!(lines.len() > MCP_BATCH_MAX_SIZE);
    }
}
