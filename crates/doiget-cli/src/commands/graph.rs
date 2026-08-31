//! `doiget graph <ref>` subcommand — citation-graph BFS via OpenAlex.
//!
//! Slice 16 / Phase 4. Compile-gated by the `citation` Cargo feature
//! (declared in `doiget-cli/Cargo.toml`). Builds the shared
//! `FetchHarness` (HTTP + ProvenanceLog + RateLimiter + Profile),
//! then delegates to [`doiget_core::citation_graph::expand`] with
//! the ADR-0010 hard caps applied via `GraphCaps::clamped`.
//!
//! Output is pretty-printed JSON on stdout (the same shape as the
//! `doiget_expand_citation_graph` MCP tool returns):
//!
//! ```json
//! {
//!   "seed_work_id": "W2741809807",
//!   "nodes": [{"id":"W...","depth":0}, ...],
//!   "edges": [{"from":"W...","to":"W..."}, ...],
//!   "truncated": false,
//!   "total_visited": 17
//! }
//! ```
//!
//! `pdf_bytes` is NEVER returned (metadata-only contract). The
//! graph walker only consults OpenAlex; S2 / DOAJ are deliberately
//! not used (only OpenAlex exposes `referenced_works[]`).

#![cfg(feature = "citation")]

use std::io::Write;

use anyhow::{Context, Result};

use doiget_core::citation_graph::{expand, GraphCaps, GraphError};
use doiget_core::sources::openalex::OpenalexSource;
use doiget_core::{ErrorCode, Ref};

use super::fetch::{cli_exit_code, CliExit, FetchHarness};
use super::output::print_err;

/// Run the `graph` subcommand against the live source set.
///
/// `input` is the user-supplied ref string (DOI only — arXiv ids are
/// rejected because OpenAlex's `referenced_works` field is keyed on
/// DOI-derived Work IDs).
///
/// `depth` / `total` / `per_paper` are optional caller hints; each
/// is clamped to the ADR-0010 maximum (3 / 100 / 20). Passing `None`
/// uses the maximum.
/// The closed-set code a `GraphError` is reported as (#507).
///
/// The `map_err` below already decides this per variant, but it does so while
/// building a user-facing message and an exit code, so the value is not
/// available to the provenance bookend that runs first. Kept next to it, and
/// exhaustive, so the two cannot say different things about the same error.
fn graph_error_code(e: &GraphError) -> ErrorCode {
    match e {
        GraphError::CapabilityDenied => ErrorCode::CapabilityDenied,
        // An indexing gap upstream: the seed is a valid DOI that OpenAlex does
        // not hold, which is `NOT_FOUND` and not a misuse of the command.
        GraphError::SeedNotIndexed => ErrorCode::NotFound,
        // These two already own a mapping; reuse it rather than restate it.
        GraphError::Source(fe) => ErrorCode::from(fe),
        GraphError::Log(_) => ErrorCode::LogError,
    }
}

pub async fn run(
    input: String,
    depth: Option<u32>,
    total: Option<u32>,
    per_paper: Option<u32>,
    _mode: super::output::OutputMode,
) -> Result<()> {
    // `_mode` is threaded per ADR-0017 / #144. Graph's JSON output is
    // product output (the requested artifact), not informational; Quiet
    // does NOT suppress it.
    // Bad ref string is user misuse -> `docs/ERRORS.md` §4 exit 2 (issue
    // #149). Through the shared helper (#492): this used to hard-code the 2
    // while `fetch` fell to the generic 1, so the same input got two
    // answers out of one binary and the comment here asserted a consistency
    // that did not hold. Sharing the helper is what makes it hold.
    let ref_ = super::parse_ref_or_exit(&input)?;
    let doi = match &ref_ {
        Ref::Doi(d) => d.clone(),
        Ref::Arxiv(_) => {
            // Passing an arXiv id to `graph` is an argument-misuse error
            // (OpenAlex's `referenced_works` is DOI-keyed) → exit 2
            // (`docs/ERRORS.md` §4), not the generic exit 1 (issue #149).
            print_err(format_args!(
                "error: doiget graph requires a DOI seed; arXiv ids are not in OpenAlex's \
                 referenced_works keyspace"
            ));
            return Err(anyhow::Error::new(CliExit(2)));
        }
    };

    let harness = FetchHarness::from_env().context("building fetch harness")?;
    if !harness.profile.metadata.openalex {
        // Capability not granted → `docs/ERRORS.md` §4 exit 3, the SAME
        // code `fetch` uses for `CAPABILITY_DENIED` (issue #149). This
        // pre-`expand` guard is the same denial class as the post-`expand`
        // `GraphError::CapabilityDenied` arm below; route both through
        // `cli_exit_code(ErrorCode::CapabilityDenied)`.
        print_err(format_args!(
            "error[{}]: doiget graph requires DOIGET_ENABLE_OPENALEX in env AND the binary \
             built with `--features citation` (CapabilityProfile.metadata.openalex is false)",
            ErrorCode::CapabilityDenied.as_wire()
        ));
        return Err(anyhow::Error::new(CliExit(cli_exit_code(
            ErrorCode::CapabilityDenied,
        ))));
    }

    // Through the core resolver, not `std::env::var`: `[network]
    // contact_email` in config.toml is a documented rung (#504), and a
    // command that reads only the environment silently ignores it.
    let contact_email = doiget_core::orchestrator::contact_email_or_placeholder();
    let source = if let Ok(base) = std::env::var("DOIGET_OPENALEX_BASE") {
        if let Ok(url) = url::Url::parse(&base) {
            OpenalexSource::with_base(url, contact_email)
        } else {
            OpenalexSource::new(contact_email)
        }
    } else {
        OpenalexSource::new(contact_email)
    };

    harness
        .log_session_start(Some(&input))
        .context("logging session start")?;

    let caps = GraphCaps {
        depth: depth.map(|d| d as usize).unwrap_or(GraphCaps::MAX_DEPTH),
        total: total.map(|t| t as usize).unwrap_or(GraphCaps::MAX_TOTAL),
        per_paper: per_paper
            .map(|p| p as usize)
            .unwrap_or(GraphCaps::MAX_PER_PAPER),
    };
    let ctx = harness.fetch_context();

    let outcome = expand(&doi, caps, &source, &harness.profile, &ctx).await;
    let session_ok = outcome.is_ok();
    // `GraphError` has its own mapping to the closed set, applied just
    // below; the bookend records the same code the caller is given (#507).
    let session_err = outcome
        .as_ref()
        .err()
        .map(|e| graph_error_code(e).as_wire());
    harness.log_session_end(session_ok, Some(&input), session_err);

    let graph = outcome.map_err(|e| match e {
        GraphError::CapabilityDenied => {
            // Issue #149: `ERRORS.md` §4 maps a capability denial to
            // exit 3 (the same code `fetch` uses for
            // `CAPABILITY_DENIED`), NOT the generic exit 1 a plain
            // `anyhow!` string would have produced. Emit the cargo-style
            // `error[CODE]:` line here (while the error is still typed),
            // then carry only the exit code to `main`.
            print_err(format_args!(
                "error[{}]: OpenAlex capability denied: set DOIGET_ENABLE_OPENALEX and \
                 rebuild with --features citation",
                ErrorCode::CapabilityDenied.as_wire()
            ));
            anyhow::Error::new(CliExit(cli_exit_code(ErrorCode::CapabilityDenied)))
        }
        GraphError::SeedNotIndexed => {
            // Not a capability/misuse class — an indexing gap upstream.
            // Falls under the generic "at least one fetch failed" exit
            // (1); a descriptive anyhow string is the right surface.
            anyhow::anyhow!("seed DOI '{input}' is not indexed by OpenAlex")
        }
        other => anyhow::Error::new(other),
    })?;

    let json = serde_json::to_string_pretty(&graph)
        .context("serializing GraphResult to JSON for stdout")?;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(out, "{json}").context("writing graph JSON to stdout")?;
    Ok(())
}
