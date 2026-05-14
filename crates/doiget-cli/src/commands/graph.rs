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

use anyhow::{bail, Context, Result};

use doiget_core::citation_graph::{expand, GraphCaps, GraphError};
use doiget_core::sources::openalex::OpenalexSource;
use doiget_core::Ref;

use super::fetch::FetchHarness;

/// Run the `graph` subcommand against the live source set.
///
/// `input` is the user-supplied ref string (DOI only — arXiv ids are
/// rejected because OpenAlex's `referenced_works` field is keyed on
/// DOI-derived Work IDs).
///
/// `depth` / `total` / `per_paper` are optional caller hints; each
/// is clamped to the ADR-0010 maximum (3 / 100 / 20). Passing `None`
/// uses the maximum.
pub async fn run(
    input: String,
    depth: Option<u32>,
    total: Option<u32>,
    per_paper: Option<u32>,
) -> Result<()> {
    let ref_ = Ref::parse(&input).with_context(|| format!("invalid ref: {input}"))?;
    let doi = match &ref_ {
        Ref::Doi(d) => d.clone(),
        Ref::Arxiv(_) => bail!(
            "doiget graph requires a DOI seed; arXiv ids are not in OpenAlex's referenced_works \
             keyspace"
        ),
    };

    let harness = FetchHarness::from_env().context("building fetch harness")?;
    if !harness.profile.metadata.openalex {
        bail!(
            "doiget graph requires DOIGET_ENABLE_OPENALEX in env AND the binary built with \
             `--features citation`. CapabilityProfile.metadata.openalex is currently false."
        );
    }

    let contact_email =
        std::env::var("DOIGET_CONTACT_EMAIL").unwrap_or_else(|_| "doiget@localhost".to_string());
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
    harness.log_session_end(session_ok, Some(&input));

    let graph = outcome.map_err(|e| match e {
        GraphError::CapabilityDenied => anyhow::anyhow!(
            "OpenAlex capability denied: set DOIGET_ENABLE_OPENALEX and rebuild with \
             --features citation"
        ),
        GraphError::SeedNotIndexed => {
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
