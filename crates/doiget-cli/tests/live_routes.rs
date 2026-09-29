//! The live use-case suite (#462): the real binary, the real services, a
//! fixed handful of refs -- each chosen because it exercises a different
//! route -- and an assertion on **which route produced the outcome**, not
//! merely that one occurred. Every "unreachable source" bug of the 0.8
//! cycle (#413, #442, #454, #458, #503, #516) passed its unit tests; the
//! component was correct and never reached. This is what notices that.
//!
//! Every test is `#[ignore]`d: the default suite stays hermetic (the
//! `network-purity` job exists for that). `.github/workflows/live.yml` runs
//! them nightly, one at a time, with a contact address and the built-in
//! rate limiter. A failure is an upstream change or a real regression, and
//! is triaged, not muted.
//!
//! Expectations were measured against the live services on 2026-09-29.
//! Run locally with:
//!
//! ```sh
//! DOIGET_CONTACT_EMAIL=you@example.org \
//!   cargo test -p doiget-cli --test live_routes -- --ignored --test-threads=1
//! ```
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tempfile::TempDir;

const IGNORE: &str = "live network; run by .github/workflows/live.yml";

/// How long one tool call may take. A fetch through a slow repository has
/// taken minutes; past this, the test fails naming the call instead of
/// hanging until the workflow's own timeout.
const REPLY_WITHIN: Duration = Duration::from_secs(240);

fn contact() -> String {
    std::env::var("DOIGET_CONTACT_EMAIL").unwrap_or_else(|_| "doiget-live@example.org".into())
}

fn base(td: &TempDir) -> Command {
    let root = td.path().to_str().unwrap();
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin("doiget"));
    cmd.current_dir(td.path())
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root);
    // A developer's own DOIGET_* settings -- base overrides, enabled
    // sources, keys, agreements, mode -- must not change the routes asserted
    // here: the suite asserts a default build's behaviour.
    for (k, _) in std::env::vars() {
        if k.starts_with("DOIGET_") {
            cmd.env_remove(k);
        }
    }
    cmd.env("DOIGET_STORE_ROOT", format!("{root}/papers"))
        .env("DOIGET_LOG_PATH", format!("{root}/log.jsonl"))
        .env("DOIGET_CONTACT_EMAIL", contact());
    cmd
}

/// `doiget serve` driven over stdio, the way an MCP host drives it.
struct Mcp {
    child: Child,
    stdin: ChildStdin,
    /// stdout lines, read on a thread so a reply can be waited for with a
    /// deadline.
    lines: Receiver<String>,
    /// Everything the server wrote to stderr, shown when a test fails.
    stderr: Arc<Mutex<String>>,
    next: u64,
}

impl Mcp {
    fn start(td: &TempDir) -> Self {
        let mut child = base(td)
            .arg("serve")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("doiget serve");
        let stdin = child.stdin.take().unwrap();
        let (tx, lines) = mpsc::channel();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        std::thread::spawn(move || {
            for line in stdout.lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let stderr = Arc::new(Mutex::new(String::new()));
        let sink = Arc::clone(&stderr);
        let mut err_pipe = child.stderr.take().unwrap();
        std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = err_pipe.read_to_string(&mut buf);
            if let Ok(mut s) = sink.lock() {
                s.push_str(&buf);
            }
        });
        let mut mcp = Self {
            child,
            stdin,
            lines,
            stderr,
            next: 1,
        };
        mcp.request(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": "doiget-live-suite", "version": "0"}}),
        );
        mcp.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        mcp
    }

    fn send(&mut self, msg: &Value) {
        writeln!(self.stdin, "{msg}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let line = match self.lines.recv_timeout(REPLY_WITHIN) {
                Ok(line) => line,
                Err(RecvTimeoutError::Timeout) => {
                    panic!("no reply to {method} within {REPLY_WITHIN:?}")
                }
                Err(RecvTimeoutError::Disconnected) => {
                    panic!("doiget serve closed stdout during {method}")
                }
            };
            let msg: Value = serde_json::from_str(&line).expect("a JSON-RPC frame");
            if msg["id"] == id {
                return msg;
            }
        }
    }

    fn fetch(&mut self, r: &str) -> Value {
        let reply = self.request(
            "tools/call",
            json!({"name": "doiget_fetch_paper", "arguments": {"ref": r}}),
        );
        reply["result"]["structuredContent"].clone()
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // On a failing test, the server's own account of what happened.
        #[allow(clippy::print_stderr)]
        if std::thread::panicking() {
            if let Ok(s) = self.stderr.lock() {
                eprintln!("--- doiget serve stderr ---\n{s}");
            }
        }
    }
}

/// Hybrid OA whose only copy is a university repository off the curated
/// allowlist: blocked, and the denial names the host that was refused.
#[test]
#[ignore = "live network; run by .github/workflows/live.yml"]
fn a_repository_copy_off_the_allowlist_is_blocked_and_named() {
    let td = TempDir::new().unwrap();
    let v = Mcp::start(&td).fetch("10.1109/TSP.2018.2812747");
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["oa_status"], "hybrid", "{v}");
    assert_eq!(v["pdf"]["status"], "blocked", "{v}");
    let host = v["pdf"]["denial_context"]["attempted"]
        .as_str()
        .unwrap_or("");
    assert!(
        host.ends_with("strath.ac.uk"),
        "the refused host is named: {v}"
    );
}

/// Bronze OA on a publisher that stays off `oa-publisher` (ADR-0039): the
/// AMS copy is refused -- and the author's own arXiv posting of the same
/// paper (2301.07880, journal_ref "Mathematics of Computation, 74(2004)")
/// is found by ADR-0062's title search and fetched instead. Both halves of
/// the route are asserted: where it was blocked, and what answered.
#[test]
#[ignore = "live network; run by .github/workflows/live.yml"]
fn a_bronze_copy_blocked_at_its_publisher_falls_back_to_the_authors_arxiv_posting() {
    let td = TempDir::new().unwrap();
    let v = Mcp::start(&td).fetch("10.1090/s0025-5718-04-01692-8");
    assert_eq!(v["oa_status"], "bronze", "{v}");
    assert_eq!(v["pdf"]["status"], "preprint_fallback", "{v}");
    assert!(
        v["pdf"]["original_block"]
            .as_str()
            .unwrap_or("")
            .contains("www.ams.org"),
        "the publisher block is still named: {v}"
    );
    assert_eq!(v["pdf"]["arxiv_id"], "2301.07880", "{v}");
    assert_eq!(v["pdf"]["found_by"], "arxiv_title_search", "{v}");
}

/// A closed DOI: metadata, no PDF, and still `ok` -- the outcome ADR-0052
/// specifies, pinned so it is not "fixed" by mistake.
#[test]
#[ignore = "live network; run by .github/workflows/live.yml"]
fn a_closed_doi_is_metadata_only_and_ok() {
    let td = TempDir::new().unwrap();
    let v = Mcp::start(&td).fetch("10.1103/PhysRevB.48.10345");
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["oa_status"], "closed", "{v}");
    assert_eq!(v["pdf"]["status"], "no_oa_url", "{v}");
}

/// The maintainer's own paper (Shimozono & Hotta, PRB 2026). Its DOI is
/// closed at APS and Unpaywall knows no copy -- but arXiv holds the preprint
/// (2512.07923). ADR-0062's arXiv title search finds it, and the DOI yields
/// the preprint; the arXiv id reaches it directly too.
#[test]
#[ignore = "live network; run by .github/workflows/live.yml"]
fn the_maintainers_paper_reaches_its_preprint_by_doi_and_by_arxiv_id() {
    let td = TempDir::new().unwrap();
    let mut mcp = Mcp::start(&td);
    let by_doi = mcp.fetch("10.1103/bbnt-brjz");
    assert_eq!(by_doi["ok"], true, "{by_doi}");
    assert_eq!(by_doi["oa_status"], "closed", "{by_doi}");
    assert_eq!(by_doi["pdf"]["status"], "preprint_fallback", "{by_doi}");
    assert_eq!(by_doi["pdf"]["arxiv_id"], "2512.07923", "{by_doi}");
    assert_eq!(by_doi["pdf"]["found_by"], "arxiv_title_search", "{by_doi}");
    let by_arxiv = mcp.fetch("arxiv:2512.07923");
    assert_eq!(by_arxiv["pdf"]["status"], "fetched", "{by_arxiv}");
    assert_eq!(by_arxiv["source"], "arxiv", "{by_arxiv}");
}

/// #640: a closed Wiley DOI whose Crossref record names its bioRxiv
/// preprint (10.1101/482166). No arXiv copy; the bioRxiv DOI is fetched
/// through the OA location Unpaywall reports for it -- in a default build,
/// with no new host.
#[test]
#[ignore = "live network; run by .github/workflows/live.yml"]
fn a_closed_doi_reaches_its_biorxiv_preprint_through_crossrefs_relation() {
    let td = TempDir::new().unwrap();
    let v = Mcp::start(&td).fetch("10.1111/1556-4029.14027");
    assert_eq!(v["oa_status"], "closed", "{v}");
    assert_eq!(v["pdf"]["status"], "preprint_doi_fallback", "{v}");
    assert_eq!(v["pdf"]["preprint_doi"], "10.1101/482166", "{v}");
    assert_eq!(v["pdf"]["found_by"], "crossref_relation", "{v}");
    assert!(v["size_bytes"].as_u64().unwrap_or(0) > 1_000, "{v}");
}

/// The same paper cited: the Crossref record, rendered.
#[test]
#[ignore = "live network; run by .github/workflows/live.yml"]
fn the_maintainers_paper_cites_from_crossref() {
    let td = TempDir::new().unwrap();
    let out = base(&td)
        .args(["cite", "10.1103/bbnt-brjz"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let bib = String::from_utf8_lossy(&out.stdout);
    assert!(bib.starts_with("@article{"), "{bib}");
    assert!(bib.contains("Shimozono"), "{bib}");
    assert!(bib.contains("Physical Review B"), "{bib}");
    assert!(bib.contains("year       = {2026}"), "{bib}");
}

/// Gold OA through the curated publisher allowlist: fetched, by
/// `oa-publisher`, with bytes on disk.
#[test]
#[ignore = "live network; run by .github/workflows/live.yml"]
fn a_gold_copy_is_fetched_through_the_publisher_allowlist() {
    let td = TempDir::new().unwrap();
    let v = Mcp::start(&td).fetch("10.3390/e22010001");
    assert_eq!(v["oa_status"], "gold", "{v}");
    assert_eq!(v["pdf"]["status"], "fetched", "{v}");
    assert_eq!(v["source"], "oa-publisher", "{v}");
    assert!(v["size_bytes"].as_u64().unwrap_or(0) > 1_000, "{v}");
}

/// A DataCite DOI in a default build: Crossref does not know it and the
/// DataCite source is not compiled in, so NOT_FOUND -- the default build
/// makes no request it was not built for.
#[test]
#[ignore = "live network; run by .github/workflows/live.yml"]
fn a_datacite_doi_is_not_found_in_a_default_build() {
    let td = TempDir::new().unwrap();
    let v = Mcp::start(&td).fetch("10.5281/zenodo.1234567");
    assert_eq!(v["ok"], false, "{v}");
    assert_eq!(v["error"]["code"], "NOT_FOUND", "{v}");
}

/// #500 end to end: NCBI turns the PMID into its DOI, Crossref cites it.
#[test]
#[ignore = "live network; run by .github/workflows/live.yml"]
fn a_pmid_is_cited_through_ncbi_and_crossref() {
    let td = TempDir::new().unwrap();
    let out = base(&td).args(["cite", "pmid:9659853"]).output().unwrap();
    assert!(out.status.success(), "{out:?}");
    let bib = String::from_utf8_lossy(&out.stdout);
    assert!(bib.contains("10.1176/ajp.155.7.895"), "{bib}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("PMID 9659853 is DOI"),
        "{out:?}"
    );
}

/// #614 end to end, on the maintainer's own software: a GitHub release is
/// cited as @software from api.github.com.
#[test]
#[ignore = "live network; run by .github/workflows/live.yml"]
fn doigets_own_release_is_cited_as_software() {
    let td = TempDir::new().unwrap();
    let out = base(&td)
        .args([
            "cite",
            "https://github.com/QAtlasHub/doiget/releases/tag/v0.8.13",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let bib = String::from_utf8_lossy(&out.stdout);
    assert!(bib.starts_with("@software{doiget_v0_8_13,"), "{bib}");
    assert!(bib.contains("version    = {v0.8.13}"), "{bib}");
    assert!(bib.contains("year       = {2026}"), "{bib}");
}

#[test]
fn the_ignore_reason_is_the_one_the_workflow_names() {
    // Keeps the attribute text and this constant from drifting apart; the
    // workflow greps nothing, but a reader follows the reason to the file.
    assert!(IGNORE.contains("live.yml"));
    assert!(std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../.github/workflows/live.yml"
    ))
    .exists());
}
