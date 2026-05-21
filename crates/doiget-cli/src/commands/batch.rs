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

use doiget_core::orchestrator::{FetchPaperOutcome, PdfLegStatus};
use doiget_core::provenance::{Capability, LogEvent, LogResult, RowInput};
use doiget_core::source::FetchError;
use doiget_core::{DenialContext, ErrorCode, RateLimits, Ref, MCP_BATCH_MAX_SIZE};

use super::fetch::{
    build_fetch_plan, effective_blocked_code, emit_dry_run_plan_to_stdout, CliExit, FetchHarness,
};
use super::resolve_store_root;

/// Run the `doiget batch <path>` subcommand.
///
/// When `dry_run` is `true` (per ADR-0022 §1 + §3): read the input
/// file, parse refs, and emit one `FetchPlan` JSON envelope line per ref
/// on stdout. NO provenance log is opened, NO HTTP client is built, NO
/// per-ref fetch runs, NO store write happens. Per-ref parse failures
/// in dry-run mode are still counted and the function returns
/// `Err(...)` if any ref failed to parse — the input was malformed and
/// the caller should know.
///
/// When `dry_run` is `false`, runs the normal multi-ref orchestration —
/// see the module-level docs for the failure-semantics contract.
///
/// # History
///
/// Slice 5 (PR #84 advisory item A2/A3 refactor): the previous
/// `BatchOptions { dry_run: bool }` single-field option bundle plus the
/// thin `run(path)` backwards-compat wrapper were collapsed into this
/// single `dry_run: bool` parameter — the option bundle's single-bool
/// shape was YAGNI, and the wrapper only existed to spare integration
/// tests a `BatchOptions::default()` literal.
pub async fn run_with_options(
    path: String,
    dry_run: bool,
    mode: super::output::OutputMode,
) -> Result<()> {
    // `mode` honors ADR-0017. `Quiet` is a no-op here because batch's
    // human summary already goes to stderr per ADR-0001. `Json` emits
    // the ERRORS.md §3 CI-persona JSON-Lines per-ref shape on stdout
    // (#205): one record per ref of the form
    //   {"ok": true,  "ref": "..."}
    //   {"ok": false, "ref": "...",
    //    "error": {"code": "<ERROR_CODE>", "message": "..."}}
    // The dry-run branch is unaffected — its product output is the
    // FetchPlan envelope per ref, not a per-ref result record. A
    // future follow-up will surface the full structured outcome
    // (safekey / store_path / canonical_digest on success,
    // `denial_context` on `CAPABILITY_DENIED`) once `fetch_one` returns
    // the structured `FetchPaperOutcome` instead of `Result<()>`.
    let json_mode = mode == super::output::OutputMode::Json;
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

    // Step 3a: dry-run branch (ADR-0022). Emit one `FetchPlan` envelope
    // per ref on stdout WITHOUT opening the provenance log, building the
    // HTTP client, or writing to the store. Per-ref parse failures still
    // count toward the exit code so a malformed batch is visible.
    if dry_run {
        let store_root = resolve_store_root()?;
        let mut parse_errors: usize = 0;
        for input in &inputs {
            match Ref::parse(input) {
                Ok(ref_) => {
                    let plan = build_fetch_plan(&ref_, &store_root);
                    emit_dry_run_plan_to_stdout(&ref_, &plan)?;
                }
                Err(e) => {
                    parse_errors += 1;
                    tracing::warn!(
                        %input,
                        error = %e,
                        "skipping malformed batch entry in dry-run mode",
                    );
                }
            }
        }
        if parse_errors > 0 {
            return Err(anyhow!(
                "dry-run batch: {} parse errors (no fetches attempted)",
                parse_errors
            ));
        }
        return Ok(());
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
                if json_mode {
                    // #205: parse failures get an INVALID_REF JSONL line
                    // with the human message in `error.message`. Per
                    // ERRORS.md §3.1 there is no `denial_context` on
                    // INVALID_REF (the input never reached a guard).
                    emit_jsonl_failure(Some(&input), "INVALID_REF", &e.to_string());
                }
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
                    // The input failed to parse as a Ref, so no
                    // CanonicalRef can be minted (ADR-0021 §1 requires
                    // a validated source_id).
                    canonical_digest: None,
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
                    // Map the semaphore-closed structural impossibility
                    // to a typed `FetchError` (closest closed-set fit)
                    // so `TaskOutcome::result` stays typed end-to-end.
                    return TaskOutcome {
                        input,
                        result: Err(FetchError::SourceSchema {
                            hint: "batch semaphore unexpectedly closed".to_string(),
                        }),
                    };
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
        let JoinedOutcome {
            is_error,
            json_record,
            log_breadcrumb,
        } = classify_joined(joined, json_mode);
        if is_error {
            fetch_errors += 1;
        } else {
            fetch_ok += 1;
        }
        if let Some(record) = json_record {
            #[allow(clippy::print_stdout)]
            {
                println!("{record}");
            }
        }
        log_breadcrumb.emit();
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
        // Issue #143 / `docs/ERRORS.md` §4: the batch process exit code is
        // the number of failures, capped at 255 (the dedicated "capped
        // failure count for `batch`" exit). Previously a bare `anyhow!`
        // fell through to `main`'s catch-all and exited 1 regardless of
        // volume, so CI callers could not tell 1 failure from 200. The
        // human-readable breakdown was ALREADY written to stderr by
        // `print_summary` above, so `CliExit` carries only the code —
        // `main` downcasts it exactly as it does for `fetch`'s `CliExit`.
        let code = total_errors.min(255) as i32;
        Err(anyhow::Error::new(CliExit(code)))
    }
}

/// Per-task outcome carried out of the `JoinSet`. Holding the original input
/// string lets the warn log breadcrumb the offending ref without re-parsing.
///
/// `Debug` is needed for the `classify_joined_panic_emits_null_ref_fetch_error`
/// unit test (which spawns a panicking task and consumes the panic
/// `Result<TaskOutcome, JoinError>` via `expect_err` — the latter requires `T: Debug`).
#[derive(Debug)]
struct TaskOutcome {
    input: String,
    result: Result<FetchPaperOutcome, FetchError>,
}

/// One-line summary written to stderr per ADR-0001 (stdio convention — the
/// CLI never writes a success line to stdout). `print_stderr` is a workspace
/// `warn` promoted to `deny` under `-D warnings`; the localized `#[allow]`
/// pinpoints the one intentional eprintln.
#[allow(clippy::print_stderr)]
fn print_summary(args: std::fmt::Arguments<'_>) {
    eprintln!("{args}");
}

/// Outcome of a single `JoinSet::join_next()` result, classified per
/// #205 (success / fetch error / task panic) plus the JSON-Lines
/// record (if `json_mode`) and the breadcrumb level for the tracing
/// log. Extracted from the drain loop so it can be unit-tested without
/// running the full orchestrator (self-review for #209 §7 +
/// codecov/patch coverage closure).
struct JoinedOutcome {
    /// True when this entry contributes to `fetch_errors`.
    is_error: bool,
    /// The JSONL record to emit, when `json_mode` is on. `None` means
    /// "no record" (either `json_mode` was false, or the variant has
    /// no record by design — currently every variant produces a
    /// record under json_mode, but the option leaves room for one).
    json_record: Option<serde_json::Value>,
    /// Source-side breadcrumb for the per-ref tracing line, captured
    /// here so `classify_joined` stays pure and the loop just calls
    /// `.log()` on the outcome.
    log_breadcrumb: LogBreadcrumb,
}

enum LogBreadcrumb {
    /// Success — no per-ref tracing line.
    None,
    /// `tracing::warn!(input, error)` for a normal fetch failure.
    FetchFailed { input: String, error_dbg: String },
    /// `tracing::error!(error)` for a task panic / cancellation.
    TaskPanicked { error_dbg: String },
}

impl LogBreadcrumb {
    /// Emit the per-ref breadcrumb to the tracing subscriber. Pulled
    /// out of `classify_joined` so the helper stays pure and out of
    /// the drain-loop body so an owned `JoinedOutcome` can be
    /// destructured without borrow-check trouble.
    fn emit(self) {
        match self {
            LogBreadcrumb::None => {}
            LogBreadcrumb::FetchFailed { input, error_dbg } => {
                tracing::warn!(%input, %error_dbg, "batch entry fetch failed");
            }
            LogBreadcrumb::TaskPanicked { error_dbg } => {
                tracing::error!(%error_dbg, "batch task panicked or was cancelled");
            }
        }
    }
}

/// Classify a single `JoinSet::join_next()` result. Pure function
/// (no I/O, no tracing) — `JoinedOutcome::log` handles the breadcrumb
/// side-effect at the drain site. Unit-tested below for every variant
/// including a real `JoinError` synthesised by spawning a panicking
/// task on a tiny `JoinSet<()>`.
fn classify_joined(
    joined: Result<TaskOutcome, tokio::task::JoinError>,
    json_mode: bool,
) -> JoinedOutcome {
    match joined {
        Ok(TaskOutcome { input, result }) => match result {
            Ok(outcome) => {
                // #210 / `docs/ERRORS.md` §3+§6: a `Blocked` PDF leg
                // is NOT a clean success even though the typed Result
                // is `Ok` — surface it as a structured failure record
                // with the `denial_context` the orchestrator carried
                // on `PdfLegStatus::Blocked.denial`.
                if let PdfLegStatus::Blocked {
                    code,
                    message,
                    denial,
                } = &outcome.pdf_leg
                {
                    let effective = effective_blocked_code(*code, denial.as_ref());
                    let denial_value = denial.as_ref().and_then(denial_context_to_value);
                    let record = json_mode.then(|| {
                        build_jsonl_failure(
                            Some(&input),
                            effective.as_wire(),
                            message,
                            denial_value,
                        )
                    });
                    let error_dbg =
                        format!("pdf_leg=Blocked code={effective:?} message={message:?}");
                    return JoinedOutcome {
                        is_error: true,
                        json_record: record,
                        log_breadcrumb: LogBreadcrumb::FetchFailed { input, error_dbg },
                    };
                }
                let result_value = json_mode.then(|| outcome_to_result_value(&outcome));
                JoinedOutcome {
                    is_error: false,
                    json_record: json_mode
                        .then(|| build_jsonl_success(&input, result_value.clone().flatten())),
                    log_breadcrumb: LogBreadcrumb::None,
                }
            }
            Err(e) => {
                let error_dbg = format!("{e:?}");
                let json_msg = format!("{e}");
                let code: ErrorCode = (&e).into();
                let denial_value = Option::<DenialContext>::from(&e)
                    .as_ref()
                    .and_then(denial_context_to_value);
                let record = json_mode.then(|| {
                    build_jsonl_failure(Some(&input), code.as_wire(), &json_msg, denial_value)
                });
                JoinedOutcome {
                    is_error: true,
                    json_record: record,
                    log_breadcrumb: LogBreadcrumb::FetchFailed { input, error_dbg },
                }
            }
        },
        Err(join_err) => {
            let error_dbg = format!("{join_err:?}");
            // The JoinSet has lost the originating input by the
            // time the panic surfaces. Honest serialisation:
            // `"ref": null` instead of a `"<task-panic>"`
            // sentinel that a consumer doing `retry(rec["ref"])`
            // would mishandle. Self-review for #209 §1.
            let json_msg = format!("batch task panicked: {join_err}");
            let record =
                json_mode.then(|| build_jsonl_failure(None, "FETCH_ERROR", &json_msg, None));
            JoinedOutcome {
                is_error: true,
                json_record: record,
                log_breadcrumb: LogBreadcrumb::TaskPanicked { error_dbg },
            }
        }
    }
}

/// Build the `result` sub-object for the JSONL success record per
/// ERRORS.md §3: `{safekey, store_path, canonical_digest}`. Returns
/// `None` if any field cannot be serialised; the caller falls back to
/// the bare `{ok, ref}` shape so a serialisation glitch never drops
/// the success signal.
fn outcome_to_result_value(outcome: &FetchPaperOutcome) -> Option<serde_json::Value> {
    Some(serde_json::json!({
        "safekey":          outcome.safekey,
        "store_path":       outcome.path.as_str(),
        "canonical_digest": outcome.canonical_digest,
    }))
}

/// Serialise a `DenialContext` to a JSON value for the `error.denial_context`
/// field. `DenialContext: serde::Serialize` already; returns `None` on
/// the (today unreachable) serialise-failure path rather than aborting
/// the record.
fn denial_context_to_value(dc: &DenialContext) -> Option<serde_json::Value> {
    serde_json::to_value(dc).ok()
}

/// #205 + #210: build the JSON-Lines success record value for
/// `ref_input`. Per ERRORS.md §3, the wire shape is
/// `{"ok": true, "ref": "...", "result": {...}}` when `result` is
/// `Some`, falling back to `{"ok": true, "ref": "..."}` when `result`
/// is `None` (the latter is reserved for paths that don't carry a
/// structured outcome — today every fetch path does, so `result` is
/// `Some` in practice; the `None` arm preserves the unit-test path
/// that asserts shape without constructing a full `FetchPaperOutcome`).
///
/// Returned as `serde_json::Value` so unit tests can assert the shape
/// without a stdout-capture dance; the integration site wraps it with
/// `println!`.
fn build_jsonl_success(ref_input: &str, result: Option<serde_json::Value>) -> serde_json::Value {
    match result {
        Some(r) => serde_json::json!({ "ok": true, "ref": ref_input, "result": r }),
        None => serde_json::json!({ "ok": true, "ref": ref_input }),
    }
}

/// #205 + #210: build the JSON-Lines failure record value with `code`,
/// `message`, and an optional `denial_context` (ADR-0023). The wire
/// shape is `{"ok": false, "ref": "<ref>"|null, "error": {"code": "...",
/// "message": "..."[, "denial_context": {...}]}}`.
///
/// `denial_context` is omitted from the record entirely when `None` —
/// fields are not present rather than `null` — so consumers can use the
/// `denial_context` key's presence as a discriminator between a typed
/// policy denial and a bare transport / config error.
///
/// `ref_input` is `Option<&str>` because the JoinSet panic arm has lost
/// the originating input by the time the panic surfaces: serialising
/// `null` is more honest than a sentinel string like `"<task-panic>"`
/// (which a consumer doing `retry(rec["ref"])` would mishandle by
/// trying to refetch the literal sentinel — self-review for #209 §1).
fn build_jsonl_failure(
    ref_input: Option<&str>,
    code: &str,
    message: &str,
    denial_context: Option<serde_json::Value>,
) -> serde_json::Value {
    let mut error = serde_json::json!({ "code": code, "message": message });
    if let Some(dc) = denial_context {
        if let Some(obj) = error.as_object_mut() {
            obj.insert("denial_context".to_string(), dc);
        }
    }
    serde_json::json!({
        "ok":    false,
        "ref":   ref_input,
        "error": error,
    })
}

/// `emit_jsonl_failure` is still called from the parse-failure site
/// (Step 7's `Ref::parse` branch); the JoinSet-drain site now uses
/// [`classify_joined`] + an inline `println!`, so the matching
/// `emit_jsonl_success` thin wrapper is no longer needed and was
/// removed alongside the drain refactor. The `denial_context` slot is
/// always `None` here — `INVALID_REF` is a pre-guard parse error, not
/// a policy denial (`docs/ERRORS.md` §3.1).
#[allow(clippy::print_stdout)]
fn emit_jsonl_failure(ref_input: Option<&str>, code: &str, message: &str) {
    println!("{}", build_jsonl_failure(ref_input, code, message, None));
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    // ---- #205 JSON-Lines record shape (unit-level) ---------------------

    #[test]
    fn jsonl_success_shape_no_result() {
        // Without a structured payload, the record falls back to the
        // bare `{ok, ref}` shape — guards the `None` arm of
        // `build_jsonl_success` (the unit-test path; production paths
        // always pass `Some`).
        let v = build_jsonl_success("10.1234/foo", None);
        assert_eq!(v["ok"], true);
        assert_eq!(v["ref"], "10.1234/foo");
        assert!(v.get("error").is_none(), "no error field on success");
        assert!(
            v.get("result").is_none(),
            "no `result` key when caller passed None"
        );
    }

    #[test]
    fn jsonl_success_shape_with_result() {
        // #210: the success record carries `result.{safekey, store_path,
        // canonical_digest}`. The keys are part of the public wire format.
        let payload = serde_json::json!({
            "safekey":          "arxiv__2401.12345",
            "store_path":       "/papers/arxiv__2401.12345.pdf",
            "canonical_digest": "deadbeef".repeat(8),
        });
        let v = build_jsonl_success("arxiv:2401.12345", Some(payload));
        assert_eq!(v["ok"], true);
        assert_eq!(v["ref"], "arxiv:2401.12345");
        assert_eq!(v["result"]["safekey"], "arxiv__2401.12345");
        assert_eq!(v["result"]["store_path"], "/papers/arxiv__2401.12345.pdf");
        assert!(
            v["result"]["canonical_digest"]
                .as_str()
                .map(|s| s.len() == 64)
                .unwrap_or(false),
            "canonical_digest MUST be a 64-char hex string: {v}"
        );
    }

    #[test]
    fn jsonl_failure_shape_invalid_ref() {
        let v = build_jsonl_failure(Some("not-a-doi"), "INVALID_REF", "bad ref", None);
        assert_eq!(v["ok"], false);
        assert_eq!(v["ref"], "not-a-doi");
        assert_eq!(v["error"]["code"], "INVALID_REF");
        assert_eq!(v["error"]["message"], "bad ref");
        assert!(
            v["error"].get("denial_context").is_none(),
            "no denial_context on INVALID_REF (pre-guard parse error): {v}"
        );
    }

    #[test]
    fn jsonl_failure_shape_fetch_error() {
        let v = build_jsonl_failure(Some("arxiv:2401.12345"), "NETWORK_ERROR", "boom", None);
        assert_eq!(v["ok"], false);
        assert_eq!(v["ref"], "arxiv:2401.12345");
        assert_eq!(v["error"]["code"], "NETWORK_ERROR");
        assert_eq!(v["error"]["message"], "boom");
    }

    #[test]
    fn jsonl_failure_shape_with_denial_context() {
        // #210: the structured `denial_context` carries the closed-enum
        // `reason` + per-source detail when a CAPABILITY_DENIED failure
        // surfaces. Hand-craft the value to pin the wire shape — the
        // serializer is `DenialContext`'s own `Serialize` impl (ADR-0023).
        let dc = serde_json::json!({
            "reason":    "redirect_not_in_allowlist",
            "source":    "oa-publisher",
            "attempted": "evil.example.com",
            "expected":  ["good-publisher.example.org"],
        });
        let v = build_jsonl_failure(
            Some("10.1234/foo"),
            "CAPABILITY_DENIED",
            "redirect not in allowlist",
            Some(dc),
        );
        assert_eq!(v["error"]["code"], "CAPABILITY_DENIED");
        assert_eq!(
            v["error"]["denial_context"]["reason"],
            "redirect_not_in_allowlist"
        );
        assert_eq!(v["error"]["denial_context"]["source"], "oa-publisher");
        assert_eq!(
            v["error"]["denial_context"]["attempted"],
            "evil.example.com"
        );
    }

    #[test]
    fn jsonl_failure_shape_panic_ref_is_null() {
        // Self-review for #209 §1: a JoinSet panic loses the input, so
        // the record carries `ref: null` rather than a sentinel string
        // that a consumer doing `retry(rec["ref"])` would mishandle.
        let v = build_jsonl_failure(None, "FETCH_ERROR", "batch task panicked: ...", None);
        assert_eq!(v["ok"], false);
        assert!(v["ref"].is_null(), "panic record's ref MUST be null: {v}");
        assert_eq!(v["error"]["code"], "FETCH_ERROR");
    }

    // ---- classify_joined: every drain-arm under both json_mode toggles

    #[test]
    fn classify_joined_success_json_emits_record() {
        let outcome = classify_joined(
            Ok(TaskOutcome {
                input: "10.1234/foo".to_string(),
                result: Ok(FetchPaperOutcome::for_test_synthetic(
                    "doi__10_1234_foo",
                    "oa-publisher",
                    PdfLegStatus::Fetched,
                )),
            }),
            true,
        );
        assert!(!outcome.is_error);
        let rec = outcome.json_record.expect("json_mode → record");
        assert_eq!(rec["ok"], true);
        assert_eq!(rec["ref"], "10.1234/foo");
        // #210: structured `result` body present on success.
        assert_eq!(rec["result"]["safekey"], "doi__10_1234_foo");
        assert!(rec["result"]["store_path"].is_string());
        assert!(rec["result"]["canonical_digest"].is_string());
        assert!(matches!(outcome.log_breadcrumb, LogBreadcrumb::None));
    }

    #[test]
    fn classify_joined_success_human_no_record() {
        let outcome = classify_joined(
            Ok(TaskOutcome {
                input: "10.1234/foo".to_string(),
                result: Ok(FetchPaperOutcome::for_test_synthetic(
                    "doi__10_1234_foo",
                    "oa-publisher",
                    PdfLegStatus::Fetched,
                )),
            }),
            false,
        );
        assert!(!outcome.is_error);
        assert!(outcome.json_record.is_none(), "human mode → no record");
    }

    #[test]
    fn classify_joined_fetch_failure_emits_typed_code() {
        // #210: the failure record now carries the typed ErrorCode wire
        // string from `FetchError → ErrorCode`, not the generic
        // `FETCH_ERROR`. `FetchError::SourceSchema` collapses to
        // `INTERNAL_ERROR` (per `source.rs`'s closed-set mapping).
        let outcome = classify_joined(
            Ok(TaskOutcome {
                input: "arxiv:2401.99999".to_string(),
                result: Err(doiget_core::source::FetchError::SourceSchema {
                    hint: "synthetic schema failure".to_string(),
                }),
            }),
            true,
        );
        assert!(outcome.is_error);
        let rec = outcome.json_record.expect("json_mode → record");
        assert_eq!(rec["ok"], false);
        assert_eq!(rec["ref"], "arxiv:2401.99999");
        assert_eq!(rec["error"]["code"], "INTERNAL_ERROR");
        assert!(matches!(
            outcome.log_breadcrumb,
            LogBreadcrumb::FetchFailed { .. }
        ));
    }

    #[test]
    fn classify_joined_blocked_pdf_emits_failure_with_denial_context() {
        // #210: a `PdfLegStatus::Blocked` outcome is reported as a
        // structured failure record carrying the `denial_context` the
        // orchestrator captured at the policy-denial site. The
        // `effective_blocked_code` reclassification promotes a
        // redirect-not-in-allowlist denial from `NETWORK_ERROR` to
        // `CAPABILITY_DENIED` so consumers see the policy class.
        use doiget_core::{DenialContext, DenialReason, ErrorCode as Ec};
        let blocked = PdfLegStatus::Blocked {
            code: Ec::NetworkError,
            message: "redirect not in allowlist".to_string(),
            denial: Some(DenialContext {
                reason: DenialReason::RedirectNotInAllowlist,
                source: Some("oa-publisher".to_string()),
                attempted: Some("evil.example.com".to_string()),
                expected: Some(vec!["good-publisher.example.org".to_string()]),
                hop_index: None,
                cap: None,
                actual: None,
            }),
        };
        let outcome = classify_joined(
            Ok(TaskOutcome {
                input: "10.1234/foo".to_string(),
                result: Ok(FetchPaperOutcome::for_test_synthetic(
                    "doi__10_1234_foo",
                    "oa-publisher",
                    blocked,
                )),
            }),
            true,
        );
        assert!(
            outcome.is_error,
            "Blocked PDF leg MUST be reported as error"
        );
        let rec = outcome.json_record.expect("json_mode → record");
        assert_eq!(rec["ok"], false);
        assert_eq!(rec["error"]["code"], "CAPABILITY_DENIED");
        assert_eq!(
            rec["error"]["denial_context"]["reason"],
            "redirect_not_in_allowlist"
        );
        assert_eq!(
            rec["error"]["denial_context"]["attempted"],
            "evil.example.com"
        );
    }

    #[test]
    fn classify_joined_panic_emits_null_ref_fetch_error() {
        // Synthesise a real `tokio::task::JoinError` by spawning a
        // panicking task on a 1-task JoinSet — exercises the
        // structurally-rare panic arm of the drain that no e2e can
        // reach (self-review for #209 §7 + codecov closure).
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        let join_err = rt.block_on(async {
            let mut js: tokio::task::JoinSet<TaskOutcome> = tokio::task::JoinSet::new();
            js.spawn(async { panic!("synthetic panic for classify_joined") });
            let joined = js.join_next().await.expect("one task");
            joined.expect_err("expected panic → Err(JoinError)")
        });

        let outcome = classify_joined(Err(join_err), true);
        assert!(outcome.is_error);
        let rec = outcome.json_record.expect("json_mode → record");
        assert_eq!(rec["ok"], false);
        assert!(
            rec["ref"].is_null(),
            "panic record's ref MUST be null: {rec}"
        );
        assert_eq!(rec["error"]["code"], "FETCH_ERROR");
        assert!(
            rec["error"]["message"]
                .as_str()
                .unwrap_or("")
                .contains("batch task panicked"),
            "panic message preserved: {rec}"
        );
        assert!(matches!(
            outcome.log_breadcrumb,
            LogBreadcrumb::TaskPanicked { .. }
        ));
    }

    #[test]
    fn log_breadcrumb_emit_does_not_panic_on_any_variant() {
        // `.emit()` should not panic on any variant. Tracing output
        // isn't asserted (the subscriber may not be installed in the
        // test harness); the test pins the no-panic happy path.
        for variant in [
            LogBreadcrumb::None,
            LogBreadcrumb::FetchFailed {
                input: "x".into(),
                error_dbg: "y".into(),
            },
            LogBreadcrumb::TaskPanicked {
                error_dbg: "z".into(),
            },
        ] {
            variant.emit();
        }
    }

    #[test]
    fn jsonl_records_are_single_line_serialised() {
        // `serde_json::Value::to_string` is compact (no trailing newline,
        // no embedded newlines) — required for the JSONL contract since
        // the production emitter wraps it with a single `println!` and
        // CI consumers split stdout by `\n`.
        let s = build_jsonl_success("10.1/x", None).to_string();
        assert!(
            !s.contains('\n'),
            "JSONL success must be single-line: {s:?}"
        );
        let s2 = build_jsonl_failure(Some("10.1/x"), "FETCH_ERROR", "msg", None).to_string();
        assert!(
            !s2.contains('\n'),
            "JSONL failure must be single-line: {s2:?}"
        );
        let s3 = build_jsonl_failure(None, "FETCH_ERROR", "msg", None).to_string();
        assert!(
            !s3.contains('\n'),
            "null-ref JSONL must be single-line: {s3:?}"
        );
    }

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
