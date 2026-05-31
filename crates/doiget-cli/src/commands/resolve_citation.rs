//! `doiget resolve-citation` and `doiget batch-resolve-citations` subcommand.

use std::io::{BufRead, Write};
use std::sync::Arc;

use anyhow::{Context, Result};

use doiget_core::source::FetchContext;
use doiget_core::sources::crossref::CrossrefSource;
use doiget_core::{RateLimits, ResolveResult};

use super::output::OutputMode;

/// Build the unified `FetchContext` using the HTTP client, rate limiter, and
/// provenance log paths resolved from the environment.
fn build_context() -> Result<FetchContext> {
    let session_id = crate::commands::fetch::new_session_id();
    let log_path = crate::commands::fetch::resolve_log_path()?;
    
    let http = Arc::new(crate::commands::fetch::build_http_client()?);
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
    })
}

/// Helper to get polite-pool email from environment or default.
fn contact_email() -> String {
    std::env::var("DOIGET_CONTACT_EMAIL").unwrap_or_else(|_| "doiget@localhost".to_string())
}

/// Run a single citation lookup.
pub async fn run(query: String, limit: u8, mode: OutputMode) -> Result<()> {
    if mode == OutputMode::Quiet {
        return Ok(());
    }

    let ctx = build_context()?;
    let crossref_base = std::env::var("DOIGET_CROSSREF_BASE").ok();
    
    let source = if let Some(base_str) = crossref_base {
        let base = url::Url::parse(&base_str).context("invalid DOIGET_CROSSREF_BASE")?;
        CrossrefSource::with_base(base, contact_email())
    } else {
        CrossrefSource::new(contact_email())
    };

    let candidates = source
        .resolve_citation(&query, limit, &ctx)
        .await
        .context("failed to resolve citation via Crossref")?;

    let result = ResolveResult {
        query,
        candidates,
    };

    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    let s = if mode == OutputMode::Json {
        serde_json::to_string_pretty(&result)?
    } else {
        // Human mode: pretty printed JSON is also perfect for bibliographic data,
        // or we can print a human-readable list. Let's print pretty JSON to stay
        // fully conformant and informative.
        serde_json::to_string_pretty(&result)?
    };

    writeln!(out, "{s}").context("failed to write resolution result to stdout")?;
    Ok(())
}

/// Run batch citation lookup by reading queries line-by-line from stdin.
pub async fn run_batch(limit: u8, mode: OutputMode) -> Result<()> {
    let ctx = build_context()?;
    let crossref_base = std::env::var("DOIGET_CROSSREF_BASE").ok();
    
    let source = if let Some(base_str) = crossref_base {
        let base = url::Url::parse(&base_str).context("invalid DOIGET_CROSSREF_BASE")?;
        CrossrefSource::with_base(base, contact_email())
    } else {
        CrossrefSource::new(contact_email())
    };

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    for line in stdin.lock().lines() {
        let query = line.context("failed to read line from stdin")?;
        let query = query.trim();
        if query.is_empty() {
            continue;
        }

        let candidates = source
            .resolve_citation(query, limit, &ctx)
            .await
            .context("failed to resolve citation in batch via Crossref")?;

        let result = ResolveResult {
            query: query.to_string(),
            candidates,
        };

        if mode != OutputMode::Quiet {
            // In batch/JSONL mode, we emit each result as a single line of minified JSON.
            let s = serde_json::to_string(&result)?;
            writeln!(out, "{s}").context("failed to write batch resolution result to stdout")?;
            out.flush().context("failed to flush stdout")?;
        }
    }

    Ok(())
}
