//! `doiget resolve-citation` and `doiget batch-resolve-citations` subcommand.

use std::io::{BufRead, Write};
use std::sync::Arc;

use anyhow::{Context, Result};

use doiget_core::source::FetchContext;
use doiget_core::sources::crossref::CrossrefSource;
use doiget_core::{RateLimits, ResolveResult};

use super::output::OutputMode;

fn build_context() -> Result<FetchContext> {
    let session_id = crate::commands::fetch::new_session_id();
    let log_path = crate::commands::fetch::resolve_log_path()?;

    let http = Arc::new(crate::commands::fetch::build_http_client(None)?);
    let rate_limiter = Arc::new(doiget_core::rate_limiter::RateLimiter::new(
        RateLimits::HARD_CODED,
    ));
    let log = Arc::new(
        doiget_core::provenance::ProvenanceLog::open(log_path, session_id.clone())
            .context("failed to open provenance log for citation resolution")?,
    );

    Ok(FetchContext {
        http,
        rate_limiter,
        log,
        session_id,
        cache_root: None,
    })
}

fn build_crossref_source() -> Result<CrossrefSource> {
    let contact_email =
        std::env::var("DOIGET_CONTACT_EMAIL").unwrap_or_else(|_| "doiget@localhost".to_string());
    match std::env::var("DOIGET_CROSSREF_BASE").ok() {
        Some(base_str) => {
            let base = url::Url::parse(&base_str).context("invalid DOIGET_CROSSREF_BASE")?;
            Ok(CrossrefSource::with_base(base, contact_email))
        }
        None => Ok(CrossrefSource::new(contact_email)),
    }
}

/// Run a single citation lookup.
pub async fn run(query: String, limit: u8, mode: OutputMode) -> Result<()> {
    if mode == OutputMode::Quiet || mode == OutputMode::Mcp {
        return Ok(());
    }

    let ctx = build_context()?;
    let source = build_crossref_source()?;

    let candidates = source
        .resolve_citation(&query, limit, &ctx)
        .await
        .context("failed to resolve citation via Crossref")?;

    let result = ResolveResult { query, candidates };
    let s = serde_json::to_string_pretty(&result)?;

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(out, "{s}").context("failed to write resolution result to stdout")?;
    Ok(())
}

/// Run batch citation lookup by reading queries line-by-line from stdin.
pub async fn run_batch(limit: u8, mode: OutputMode) -> Result<()> {
    // Collect all lines before entering the async loop — holding StdinLock
    // (a blocking OS primitive) across `.await` yield points would stall
    // the executor thread.
    let queries: Vec<String> = {
        let stdin = std::io::stdin();
        stdin
            .lock()
            .lines()
            .collect::<std::io::Result<_>>()
            .context("failed to read lines from stdin")?
    };

    let ctx = build_context()?;
    let source = build_crossref_source()?;

    for raw in queries {
        let query = raw.trim().to_string();
        if query.is_empty() {
            continue;
        }

        let candidates = source
            .resolve_citation(&query, limit, &ctx)
            .await
            .context("failed to resolve citation in batch via Crossref")?;

        if mode != OutputMode::Quiet {
            let result = ResolveResult { query, candidates };
            // In batch/JSONL mode, emit each result as a single line of minified JSON.
            let s = serde_json::to_string(&result)?;
            // Acquire and release stdout lock per iteration so it is never
            // held across an `.await` point.
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            writeln!(out, "{s}").context("failed to write batch resolution result to stdout")?;
            out.flush().context("failed to flush stdout")?;
        }
    }

    Ok(())
}
