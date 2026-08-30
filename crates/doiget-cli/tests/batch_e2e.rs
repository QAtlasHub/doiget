//! End-to-end wiremock-driven test for `doiget batch <path>`.
//!
//! Phase 1 success criterion (per `docs/PHASES.md` §4): the orchestrator
//! "honors the rate cap and writes a hash-chained provenance log". This test
//! pins both invariants by:
//!
//! 1. Fetching three arXiv refs through one shared `ProvenanceLog`, asserting
//!    every row links via `prev_hash` -> `this_hash` and the bookend rows
//!    (`SessionStart`, `SessionEnd`) are emitted exactly once each.
//! 2. Pointing the orchestrator at a wiremock `MockServer` so no outbound
//!    network call is made (per the network-purity guard).
//!
//! The second case (mixed parse-error + good ref) verifies the failure-mode
//! contract from `commands/batch.rs`: malformed lines emit a `Resolve` row
//! with `result=err`, sibling fetches still complete, and the surrounding
//! `run` returns `Err` so the binary surfaces a non-zero exit.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use camino::{Utf8Path, Utf8PathBuf};
use doiget_cli::commands::batch;
use doiget_cli::commands::output::OutputMode;
use doiget_core::provenance::{LogEvent, LogResult, LogRow};
use doiget_core::MCP_BATCH_MAX_SIZE;
use serial_test::serial;
use tempfile::TempDir;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;
use common::env_guard::EnvGuard;

/// Read every JSONL row from the on-disk provenance log.
fn read_log_rows(path: &Utf8PathBuf) -> Vec<LogRow> {
    let raw = std::fs::read_to_string(path.as_std_path()).expect("read log");
    raw.lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str::<LogRow>(l).expect("valid LogRow"))
        .collect()
}

/// Verify the SHA-256 hash chain links rows in file order.
fn assert_chain_intact(rows: &[LogRow]) {
    assert_eq!(
        rows[0].prev_hash, "GENESIS",
        "first row must chain to GENESIS"
    );
    for i in 1..rows.len() {
        assert_eq!(
            rows[i].prev_hash,
            rows[i - 1].this_hash,
            "hash chain break at row {i}"
        );
    }
}

/// Build a temp-dir-backed env (store + log) and the standard set of
/// `DOIGET_*` env vars cleared. Returns the temp dir, the resolved store
/// root, the resolved log path, and the live env guard.
fn stage_env() -> (TempDir, Utf8PathBuf, Utf8PathBuf, EnvGuard) {
    let td = TempDir::new().expect("tempdir");
    let temp_root: Utf8PathBuf = Utf8Path::from_path(td.path())
        .expect("temp dir is utf-8")
        .to_path_buf();
    let store_root = temp_root.join("papers");
    let log_path = temp_root.join("log.jsonl");

    let env = EnvGuard::new(&[
        "DOIGET_STORE_ROOT",
        "DOIGET_LOG_PATH",
        "DOIGET_ARXIV_BASE",
        "DOIGET_CROSSREF_BASE",
        "DOIGET_UNPAYWALL_BASE",
        "DOIGET_CONTACT_EMAIL",
        "DOIGET_UNPAYWALL_EMAIL",
    ]);
    (td, store_root, log_path, env)
}

/// Mount the canonical arXiv PDF mock for one specific id at the given
/// server. The body passes the `%PDF-` magic-byte check enforced by
/// `HttpClient::fetch_pdf`.
async fn mount_arxiv_pdf(server: &MockServer, arxiv_id: &str, body: &[u8]) {
    Mock::given(method("GET"))
        .and(path(format!("/pdf/{arxiv_id}.pdf")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.to_vec()))
        .mount(server)
        .await;
}

#[tokio::test]
#[serial]
async fn batch_three_arxiv_refs_succeeds_end_to_end() {
    // Step 1: spin up wiremock and register three independent paths so each
    // ref hits its own mock. Same body for all three keeps the assertions
    // simple; the orchestrator-level invariants we care about are concurrency
    // bound, hash chain, and bookend rows.
    let server = MockServer::start().await;
    let body = b"%PDF-1.7\n%batch-fixture-bytes\n".to_vec();
    let ids = ["2401.12345", "2401.12346", "2401.12347"];
    for id in &ids {
        mount_arxiv_pdf(&server, id, &body).await;
    }

    // Step 2: stage env vars + temp dir for store + log artifacts.
    let (td, store_root, log_path, env) = stage_env();
    env.set("DOIGET_STORE_ROOT", store_root.as_str());
    env.set("DOIGET_LOG_PATH", log_path.as_str());
    env.set("DOIGET_ARXIV_BASE", &server.uri());

    // Step 3: write a refs file and run the orchestrator end-to-end. The
    // input intentionally includes a blank line, a comment line, and a
    // leading-whitespace comment to exercise the Step 2 parse filter from
    // `commands/batch.rs`.
    let refs_path = store_root.parent().unwrap().join("refs.txt");
    std::fs::write(
        refs_path.as_std_path(),
        format!(
            "# batch test fixture\n\
             arxiv:{}\n\
             \n\
             arxiv:{}\n\
             # mid-file comment\n\
             arxiv:{}\n",
            ids[0], ids[1], ids[2]
        ),
    )
    .expect("write refs file");

    batch::run_with_options(
        refs_path.as_str().to_string(),
        false,
        false,
        None,
        None,
        OutputMode::Human,
    )
    .await
    .expect("batch::run_with_options succeeds");

    // Step 4: assert all three PDFs landed in the store under the expected
    // safekey-derived names.
    for id in &ids {
        let pdf_path = store_root.join(format!("arxiv_{id}.pdf"));
        assert!(
            pdf_path.exists(),
            "expected PDF at {pdf_path}; refs.txt was {refs_path}"
        );
        let bytes = std::fs::read(pdf_path.as_std_path()).expect("read pdf");
        assert_eq!(bytes, body, "stored PDF must match wiremock body for {id}");
    }

    // Step 5: assert the provenance-log row sequence. We expect:
    //   1× SessionStart (orchestrator, ref=None)
    //   3× Fetch ok       (one per arxiv source dispatch)
    //   3× StoreWrite ok  (one per per-ref write_to_store)
    //   1× SessionEnd ok  (orchestrator, ref=None)
    // Concurrent-task ordering means the inter-ref interleaving of Fetch
    // and StoreWrite is NOT pinned; we assert counts and bookends only.
    let rows = read_log_rows(&log_path);
    assert!(
        !rows.is_empty(),
        "log must have at least one row; got: {rows:?}"
    );

    // Bookends: first and last rows must be SessionStart / SessionEnd.
    assert_eq!(rows.first().unwrap().event, LogEvent::SessionStart);
    assert_eq!(rows.first().unwrap().result, LogResult::Ok);
    assert!(
        rows.first().unwrap().ref_.is_none(),
        "batch SessionStart must have ref=None, got {:?}",
        rows.first().unwrap().ref_
    );
    assert_eq!(rows.last().unwrap().event, LogEvent::SessionEnd);
    assert_eq!(rows.last().unwrap().result, LogResult::Ok);
    assert!(rows.last().unwrap().ref_.is_none());

    // Counts.
    let count_event = |evt: LogEvent| rows.iter().filter(|r| r.event == evt).count();
    assert_eq!(
        count_event(LogEvent::SessionStart),
        1,
        "exactly one SessionStart"
    );
    assert_eq!(
        count_event(LogEvent::SessionEnd),
        1,
        "exactly one SessionEnd"
    );
    assert_eq!(count_event(LogEvent::Fetch), 3, "one Fetch per ref");
    assert_eq!(
        count_event(LogEvent::StoreWrite),
        3,
        "one StoreWrite per ref"
    );

    // Every Fetch row should be `result=ok` with `source="arxiv"`.
    for r in rows.iter().filter(|r| r.event == LogEvent::Fetch) {
        assert_eq!(r.result, LogResult::Ok);
        assert_eq!(r.source.as_deref(), Some("arxiv"));
        assert_eq!(r.size_bytes, Some(body.len() as u64));
    }
    for r in rows.iter().filter(|r| r.event == LogEvent::StoreWrite) {
        assert_eq!(r.result, LogResult::Ok);
        assert_eq!(r.source.as_deref(), Some("arxiv"));
    }

    // Hash chain: every row's prev_hash must equal the previous row's
    // this_hash, with row 0 anchored at GENESIS.
    assert_chain_intact(&rows);

    // All rows share the same session_id (one ProvenanceLog, one ULID).
    let sid = &rows[0].session_id;
    for r in &rows {
        assert_eq!(&r.session_id, sid, "all rows must share session_id");
    }

    drop(env);
    drop(td);
}

#[tokio::test]
#[serial]
async fn batch_with_malformed_ref_continues_and_returns_err() {
    // Step 1: same shape as above, but with one good ref + one malformed
    // entry. The malformed entry should produce a `Resolve` row with
    // `result=err`; the sibling good ref should still get fetched; the
    // overall `run` MUST return `Err` so the binary exits non-zero.
    let server = MockServer::start().await;
    let body = b"%PDF-1.7\n%batch-mixed-fixture\n".to_vec();
    let good_id = "2401.99999";
    mount_arxiv_pdf(&server, good_id, &body).await;

    let (td, store_root, log_path, env) = stage_env();
    env.set("DOIGET_STORE_ROOT", store_root.as_str());
    env.set("DOIGET_LOG_PATH", log_path.as_str());
    env.set("DOIGET_ARXIV_BASE", &server.uri());

    let refs_path = store_root.parent().unwrap().join("refs.txt");
    std::fs::write(
        refs_path.as_std_path(),
        format!(
            "not-a-ref\n\
             arxiv:{good_id}\n",
        ),
    )
    .expect("write refs file");

    let result = batch::run_with_options(
        refs_path.as_str().to_string(),
        false,
        false,
        None,
        None,
        OutputMode::Human,
    )
    .await;
    let err = result.expect_err("batch with a malformed ref must surface an error to the binary");
    // Issue #143 / `docs/ERRORS.md` §4: the batch exit code is the number
    // of failures (capped at 255), NOT a blanket 1. Exactly one entry
    // failed to parse here, so the carried `CliExit` must be 1.
    let cli_exit = err
        .downcast_ref::<doiget_cli::commands::fetch::CliExit>()
        .expect("batch failure must carry a CliExit so main maps it to the §4 exit code");
    assert_eq!(
        cli_exit.0, 1,
        "one parse failure must yield exit code 1 (failure count), not a generic anyhow exit"
    );

    // Step 2: the good ref's PDF must still be on disk.
    let pdf_path = store_root.join(format!("arxiv_{good_id}.pdf"));
    assert!(
        pdf_path.exists(),
        "the good ref must still be fetched alongside a malformed sibling"
    );

    // Step 3: assert the log has exactly one Resolve(err) for the bad
    // entry, one Fetch(ok) + one StoreWrite(ok) for the good entry, and
    // the bookends frame everything.
    let rows = read_log_rows(&log_path);
    assert_eq!(rows.first().unwrap().event, LogEvent::SessionStart);
    assert_eq!(rows.last().unwrap().event, LogEvent::SessionEnd);
    // SessionEnd must surface the failure as `result=err`.
    assert_eq!(rows.last().unwrap().result, LogResult::Err);

    let resolve_err: Vec<&LogRow> = rows
        .iter()
        .filter(|r| r.event == LogEvent::Resolve && r.result == LogResult::Err)
        .collect();
    assert_eq!(
        resolve_err.len(),
        1,
        "exactly one Resolve(err) for the malformed entry"
    );
    assert_eq!(resolve_err[0].ref_.as_deref(), Some("not-a-ref"));
    assert_eq!(resolve_err[0].error_code.as_deref(), Some("INVALID_REF"));

    let fetch_ok: usize = rows
        .iter()
        .filter(|r| r.event == LogEvent::Fetch && r.result == LogResult::Ok)
        .count();
    assert_eq!(fetch_ok, 1, "one Fetch(ok) for the good ref");
    let store_ok: usize = rows
        .iter()
        .filter(|r| r.event == LogEvent::StoreWrite && r.result == LogResult::Ok)
        .count();
    assert_eq!(store_ok, 1, "one StoreWrite(ok) for the good ref");

    // Hash chain still intact across the mixed-outcome session.
    assert_chain_intact(&rows);

    drop(env);
    drop(td);
}

#[tokio::test]
#[serial]
#[ignore = "612 s at arXiv's published 3 s/request rate; run by the `test (slow)` CI job via --ignored"]
async fn batch_above_window_size_fetches_every_ref() {
    // Issue #304: a refs file LARGER than `MCP_BATCH_MAX_SIZE` must no longer
    // abort fetching nothing — every ref is processed across multiple
    // dispatch windows, under one session, with a single SessionStart /
    // SessionEnd. This exercises the real multi-window dispatch loop (not
    // just the windowing arithmetic), so it confirms counters accumulate
    // across windows and the bookends are emitted exactly once.
    //
    // SLOW BY CONSTRUCTION, and far slower than this comment used to claim.
    // It said "~20 s", counting only the global 5-per-second cap. Two things
    // it missed:
    //
    //   * `SOURCE_RATE_OVERRIDES` puts 3 s between arXiv requests, because
    //     arXiv's Terms of Use do (2cc32ab, 2026-08-25 -- after this test was
    //     written, which is why the estimate was right when it was made);
    //   * one arXiv attempt issues TWO requests, the Atom feed then the PDF,
    //     and #493 paced the second one too.
    //
    // So the real cost is 102 * 2 * 3 s = 612 s, and CI measured 609 s. That
    // is not waste -- it is arXiv's published rate, faithfully obeyed -- but
    // it is a property of the dispatch loop, which varies with neither OS nor
    // Cargo feature. `#[ignore]` keeps it off the five broad `test` jobs; the
    // `test (slow)` job runs it once with `--ignored`.
    //
    // Kept `#[serial]` so it never contends with the other batch fixtures.
    let server = MockServer::start().await;
    let body = b"%PDF-1.7\n%batch-window-fixture\n".to_vec();
    // One regex mock serves every generated arXiv id rather than mounting one
    // mock per ref.
    Mock::given(method("GET"))
        .and(path_regex(r"^/pdf/2401\.\d{5}\.pdf$"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
        .mount(&server)
        .await;

    let (td, store_root, log_path, env) = stage_env();
    env.set("DOIGET_STORE_ROOT", store_root.as_str());
    env.set("DOIGET_LOG_PATH", log_path.as_str());
    env.set("DOIGET_ARXIV_BASE", &server.uri());

    // Two past the window boundary → the second window carries the remaining
    // 2 refs (the old hard cap aborted the whole run at this size).
    let n = MCP_BATCH_MAX_SIZE + 2;
    let ids: Vec<String> = (0..n).map(|i| format!("2401.{:05}", 10000 + i)).collect();
    let refs_body: String = ids.iter().map(|id| format!("arxiv:{id}\n")).collect();
    let refs_path = store_root.parent().unwrap().join("refs.txt");
    std::fs::write(refs_path.as_std_path(), refs_body).expect("write refs file");

    batch::run_with_options(
        refs_path.as_str().to_string(),
        false,
        false,
        None,
        None,
        OutputMode::Human,
    )
    .await
    .expect("a batch above the window size must succeed, fetching every ref");

    // Every ref's PDF landed — nothing was dropped by an over-limit abort.
    for id in &ids {
        let pdf_path = store_root.join(format!("arxiv_{id}.pdf"));
        assert!(
            pdf_path.exists(),
            "missing PDF for {id} in a {n}-ref (multi-window) batch"
        );
    }

    // Provenance: one Fetch(ok) per ref across all windows, framed by exactly
    // one session bookend pair.
    let rows = read_log_rows(&log_path);
    let count_event = |evt: LogEvent| rows.iter().filter(|r| r.event == evt).count();
    assert_eq!(
        count_event(LogEvent::Fetch),
        n,
        "one Fetch row per ref, summed across every window"
    );
    assert_eq!(
        count_event(LogEvent::SessionStart),
        1,
        "exactly one SessionStart for the whole multi-window batch"
    );
    assert_eq!(
        count_event(LogEvent::SessionEnd),
        1,
        "exactly one SessionEnd for the whole multi-window batch"
    );
    assert_eq!(
        rows.last().unwrap().result,
        LogResult::Ok,
        "all refs fetched → SessionEnd ok"
    );
    assert_chain_intact(&rows);

    drop(env);
    drop(td);
}
