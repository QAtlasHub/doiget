//! `doiget frontier <doi>` — gap-spotting frontier view (#295).
//!
//! Surfaces papers that **cite the seed DOI**, ranked by age-normalized
//! impact (`fwci` descending). Papers already present in the local store
//! are filtered out by default so only unread candidates are shown.
//!
//! ## Output
//!
//! - **Human mode**: tab-separated table `fwci | year | oa | doi | title`.
//! - **JSON mode**: `{ seed_doi, seed_title, seed_openalex_id, total_citing,
//!   count, results: [PaperHit...] }`.
//!
//! ## Store exclusion
//!
//! Each candidate DOI is mapped to its safekey and checked against
//! `<store_root>/<safekey>.pdf`. Matches are silently dropped (the purpose
//! of frontier is to surface the *unread* portion of the neighbourhood).

use std::io::Write;

use anyhow::{Context, Result};
use doiget_core::discovery::{frontier_view, FrontierQuery, FrontierResults};
use doiget_core::ErrorCode;

use super::fetch::{cli_exit_code, CliExit, FetchHarness};
use super::output::OutputMode;
use super::resolve_store_root;

/// OpenAlex API base; overridable via `DOIGET_OPENALEX_BASE` (tests).
const OPENALEX_DEFAULT_BASE: &str = "https://api.openalex.org";

/// Entry point for `doiget frontier <doi>`.
#[allow(clippy::print_stdout, clippy::print_stderr)]
pub async fn run(
    doi_str: String,
    limit: usize,
    from_year: Option<i32>,
    mode: OutputMode,
    quiet_was_explicit: bool,
) -> Result<()> {
    let seed_doi = doiget_core::Doi::parse(&doi_str)
        .map_err(|e| anyhow::anyhow!("invalid seed DOI {doi_str:?}: {e}"))?;

    let base = {
        let raw = std::env::var("DOIGET_OPENALEX_BASE")
            .unwrap_or_else(|_| OPENALEX_DEFAULT_BASE.to_string());
        url::Url::parse(&raw)
            .with_context(|| format!("DOIGET_OPENALEX_BASE is not a URL: {raw}"))?
    };
    let contact_email = std::env::var("DOIGET_CONTACT_EMAIL").unwrap_or_default();

    let harness = FetchHarness::from_env().context("building fetch harness")?;
    harness
        .log_session_start(Some(&doi_str))
        .context("logging session start")?;
    let ctx = harness.fetch_context();

    let query = FrontierQuery {
        seed_doi: seed_doi.clone(),
        limit: limit.clamp(1, 200),
        min_year: from_year,
    };

    let outcome = frontier_view(&query, &base, &contact_email, &ctx).await;
    harness.log_session_end(outcome.is_ok(), Some(&doi_str));

    let mut results = match outcome {
        Ok(r) => r,
        Err(e) => {
            let code = ErrorCode::from(&e);
            eprintln!("error[{}]: {e}", code.as_wire());
            return Err(anyhow::Error::new(CliExit(cli_exit_code(code))));
        }
    };

    // Filter out papers already in the local store.
    if let Ok(store_root) = resolve_store_root() {
        results.hits.retain(|hit| {
            let Some(ref doi_s) = hit.doi else {
                return true;
            };
            let Ok(d) = doiget_core::Doi::parse(doi_s) else {
                return true;
            };
            let safekey = doiget_core::Ref::Doi(d).safekey();
            !store_root
                .join(format!("{}.pdf", safekey.as_str()))
                .exists()
        });
    }

    if mode == OutputMode::Quiet && quiet_was_explicit {
        return Ok(());
    }

    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    if mode == OutputMode::Json {
        let envelope = json_envelope(&doi_str, &results);
        let s = serde_json::to_string(&envelope).context("serialise frontier results to JSON")?;
        writeln!(out, "{s}").context("write frontier JSON to stdout")?;
        return Ok(());
    }

    // Human table: fwci first (the age-normalized signal that matters most),
    // then year / oa / doi / title.
    writeln!(out, "fwci\tyear\toa\tdoi\ttitle").context("write frontier header")?;
    for hit in &results.hits {
        let fwci = hit
            .fwci
            .map(|f| format!("{f:.2}"))
            .unwrap_or_else(|| "-".into());
        let year = hit
            .year
            .map(|y| y.to_string())
            .unwrap_or_else(|| "-".into());
        let oa = hit.oa_status.as_deref().unwrap_or("-");
        let doi = hit.doi.as_deref().unwrap_or("-");
        writeln!(out, "{fwci}\t{year}\t{oa}\t{doi}\t{}", hit.title)
            .context("write frontier row")?;
    }
    Ok(())
}

fn json_envelope(seed_doi: &str, results: &FrontierResults) -> serde_json::Value {
    serde_json::json!({
        "seed_doi": seed_doi,
        "seed_title": results.seed_title,
        "seed_openalex_id": results.seed_openalex_id,
        "total_citing": results.total_citing,
        "count": results.hits.len(),
        "results": results.hits,
    })
}
