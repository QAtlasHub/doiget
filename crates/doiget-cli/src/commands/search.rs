//! `doiget search <query>` subcommand.
//!
//! Two scopes, one command (ADR-0031 D5):
//!
//! - **external** (default) — discovery search over OpenAlex
//!   `/works?search=` via [`doiget_core::discovery::paper_search`]. Turns
//!   a topic into ranked candidate papers (title / abstract / year /
//!   venue / citations / OA status / DOI) for triage *before* any PDF is
//!   fetched. Tier-1 OA metadata, always-on, ships in the default
//!   `oa-only` binary; no `DOIGET_ENABLE_OPENALEX` gate.
//! - **local** (`--local`) — the legacy substring scan over
//!   `<store-root>/.metadata/*.toml` via
//!   [`FsStore::search`](doiget_core::store::FsStore). Re-finds papers
//!   already in the store; offline.
//!
//! `--local` and `--external` are mutually exclusive; omitting both means
//! external. Both scopes share one `--mode json` envelope —
//! `{ "scope": "external" | "local", "query": "...", "count": N,
//! "results": [...] }` — with a scope-dependent `results[]` element
//! schema. The external scope additionally carries `"total_results"` (the
//! upstream OpenAlex match count, which may exceed `count`).

use std::io::Write;

use anyhow::{Context, Result};

use doiget_core::discovery::{paper_search, PaperSearchQuery, PaperSearchResults, SearchSort};
use doiget_core::store::{EntryInfo, FsStore, Store};
use doiget_core::ErrorCode;

use super::fetch::{cli_exit_code, CliExit, FetchHarness};
use super::output::OutputMode;
use super::resolve_store_root;

/// Phase 1 default cap on the number of returned **local** rows. Picked to
/// match the "small CLI table" feel — large enough to be useful for an
/// ad-hoc `doiget search foo --local`, small enough that an unbounded scan
/// over a pathological store still terminates promptly.
const LOCAL_DEFAULT_LIMIT: usize = 50;

/// Format string for [`chrono::DateTime`] columns. RFC3339-shaped, UTC, no
/// fractional seconds — identical to the [`list_recent`](super::list_recent)
/// table so downstream pipelines can treat both outputs uniformly.
const FETCHED_AT_FMT: &str = "%Y-%m-%dT%H:%M:%SZ";

/// Production OpenAlex API base. Overridable via `DOIGET_OPENALEX_BASE`
/// (test wiremock origin), mirroring the `graph` subcommand.
const OPENALEX_DEFAULT_BASE: &str = "https://api.openalex.org";

/// `--sort` choices for external discovery. Maps 1:1 onto
/// [`SearchSort`]; kept CLI-local so `doiget-core` carries no `clap` dep.
#[derive(Clone, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum SortArg {
    /// Best textual match first (OpenAlex `relevance_score:desc`).
    ///
    /// The only sort: `cited` / `recent` were removed (#290) — over
    /// OpenAlex's loose free-text match they float off-topic papers to the
    /// top. Use `--min-fwci` / `--min-percentile` / `--from-year` to
    /// surface "important / recent" results as FILTERS instead.
    #[default]
    Relevance,
}

impl From<SortArg> for SearchSort {
    fn from(s: SortArg) -> Self {
        match s {
            SortArg::Relevance => SearchSort::Relevance,
        }
    }
}

/// External-discovery flag bundle (everything except `query` / `mode`).
/// Bundled so the `main.rs` dispatch arm and [`run`] stay readable.
#[derive(Debug, Clone)]
pub struct ExternalArgs {
    /// Max results; validated to `1..=200` (OpenAlex `per-page` ceiling) by
    /// `PaperSearchQuery::validate` — an out-of-range value is rejected,
    /// not silently clamped.
    pub limit: usize,
    /// Inclusive lower publication-year bound.
    pub from_year: Option<i32>,
    /// Inclusive upper publication-year bound.
    pub to_year: Option<i32>,
    /// Restrict to open-access works.
    pub oa_only: bool,
    /// Only works cited strictly more than this many times.
    pub min_citations: Option<u64>,
    /// Minimum field-and-year-normalized impact (FWCI) floor (#290).
    pub min_fwci: Option<f64>,
    /// Minimum within-cohort citation percentile, 0–100 (#290).
    pub min_percentile: Option<u8>,
    /// Author name to filter by (resolved to an OpenAlex author ID).
    pub author: Option<String>,
    /// Venue / journal name to filter by (resolved to an OpenAlex source ID).
    pub venue: Option<String>,
    /// Publisher name to filter by (resolved to an OpenAlex publisher ID).
    pub publisher: Option<String>,
    /// Result ordering.
    pub sort: SortArg,
}

/// Stderr sink for `docs/ERRORS.md` §3 human-error lines (mirrors the
/// `print_err` helper in `commands::fetch` / `commands::graph`).
#[allow(clippy::print_stderr)]
fn print_err(args: std::fmt::Arguments<'_>) {
    eprintln!("{args}");
}

/// Run the `search` subcommand.
///
/// `local` selects the store scan; otherwise external discovery runs
/// (the default; `--external` is its explicit form and is already
/// resolved away by clap's `conflicts_with`). `ext` carries the
/// external-only flags and is ignored on the local path.
///
/// # Errors
///
/// Propagates store-open / scan failures (local) or surfaces a typed
/// [`ErrorCode`] as a process exit code (external); an empty query is a
/// usage error.
pub async fn run(
    query: String,
    local: bool,
    ext: ExternalArgs,
    mode: OutputMode,
    quiet_was_explicit: bool,
) -> Result<()> {
    if query.trim().is_empty() {
        anyhow::bail!("search query is empty");
    }
    if local {
        run_local(&query, mode, quiet_was_explicit)
    } else {
        run_external(&query, ext, mode, quiet_was_explicit).await
    }
}

/// Local-store substring scan (legacy behaviour, now behind `--local`).
fn run_local(query: &str, mode: OutputMode, quiet_was_explicit: bool) -> Result<()> {
    let store_root = resolve_store_root()?;
    let store = FsStore::new(store_root)?;
    let entries = store
        .search(query, LOCAL_DEFAULT_LIMIT)
        .with_context(|| format!("search failed for query {query:?}"))?;

    // Artifact-class (ADR-0017 Amendment 2 / #301): suppress only on
    // explicit Quiet; the non-TTY implicit fallback still emits.
    if mode == OutputMode::Quiet && quiet_was_explicit {
        return Ok(());
    }

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if mode == OutputMode::Json {
        write_json(&mut out, &local_envelope(query, &entries))?;
        return Ok(());
    }
    writeln!(out, "safekey\tyear\ttitle\tfetched_at")
        .context("failed to write search header to stdout")?;
    for e in entries {
        let year = dash_or(e.year);
        let fetched = e
            .fetched_at
            .map(|t| t.format(FETCHED_AT_FMT).to_string())
            .unwrap_or_else(|| "-".into());
        writeln!(
            out,
            "{}\t{}\t{}\t{}",
            e.safekey.as_str(),
            year,
            e.title,
            fetched
        )
        .context("failed to write search row to stdout")?;
    }
    Ok(())
}

/// External OpenAlex discovery search (the default scope).
async fn run_external(
    query: &str,
    ext: ExternalArgs,
    mode: OutputMode,
    quiet_was_explicit: bool,
) -> Result<()> {
    let q = PaperSearchQuery {
        query: query.to_string(),
        limit: ext.limit,
        from_year: ext.from_year,
        to_year: ext.to_year,
        oa_only: ext.oa_only,
        min_citations: ext.min_citations,
        min_fwci: ext.min_fwci,
        min_percentile: ext.min_percentile,
        author: ext.author,
        venue: ext.venue,
        publisher: ext.publisher,
        sort: ext.sort.into(),
    };
    // Boundary validation (limit range, inverted year range) lives in
    // `PaperSearchQuery::validate` so the CLI and the MCP tool cannot drift.
    q.validate().map_err(|m| anyhow::anyhow!("{m}"))?;

    let base = resolve_openalex_base()?;
    // Leave `mailto` unset when no contact email is configured: send a real
    // address (polite pool) or none, never a non-routable placeholder. The
    // empty string is skipped by `build_search_url` / `resolve_entity_id`.
    let contact_email = std::env::var("DOIGET_CONTACT_EMAIL").unwrap_or_default();

    let harness = FetchHarness::from_env().context("building fetch harness")?;
    harness
        .log_session_start(Some(query))
        .context("logging session start")?;
    let ctx = harness.fetch_context();

    let outcome = paper_search(&base, &contact_email, &q, &ctx).await;
    harness.log_session_end(outcome.is_ok(), Some(query));

    let results = match outcome {
        Ok(r) => r,
        Err(e) => {
            let code = ErrorCode::from(&e);
            print_err(format_args!("error[{}]: {e}", code.as_wire()));
            return Err(anyhow::Error::new(CliExit(cli_exit_code(code))));
        }
    };

    // Artifact-class (ADR-0017 Amendment 2 / #301): suppress only on
    // explicit Quiet; the non-TTY implicit fallback still emits.
    if mode == OutputMode::Quiet && quiet_was_explicit {
        return Ok(());
    }

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if mode == OutputMode::Json {
        write_json(&mut out, &external_envelope(query, &results))?;
        return Ok(());
    }

    // Human table: surface "interesting" signals (citations) first, then
    // year / OA / DOI / title. Tab-separated, `cut(1)`-compatible.
    writeln!(out, "cited_by\tyear\toa\tdoi\ttitle")
        .context("failed to write search header to stdout")?;
    for hit in &results.results {
        let year = dash_or(hit.year);
        let oa = hit.oa_status.as_deref().unwrap_or("-");
        let doi = hit.doi.as_deref().unwrap_or("-");
        writeln!(
            out,
            "{}\t{}\t{}\t{}\t{}",
            hit.cited_by_count, year, oa, doi, hit.title
        )
        .context("failed to write search row to stdout")?;
    }
    Ok(())
}

/// Resolve the OpenAlex base URL: `DOIGET_OPENALEX_BASE` override (tests)
/// or the production default.
fn resolve_openalex_base() -> Result<url::Url> {
    let raw =
        std::env::var("DOIGET_OPENALEX_BASE").unwrap_or_else(|_| OPENALEX_DEFAULT_BASE.to_string());
    url::Url::parse(&raw).with_context(|| format!("DOIGET_OPENALEX_BASE is not a URL: {raw}"))
}

/// Build the local-scan `--mode json` envelope (ADR-0031 D5):
/// `{ scope: "local", query, count, results }`. The `results[]` element is
/// the legacy `EntryInfo` shape, unchanged.
fn local_envelope(query: &str, entries: &[EntryInfo]) -> serde_json::Value {
    serde_json::json!({
        "scope": "local",
        "query": query,
        "count": entries.len(),
        "results": entries,
    })
}

/// Build the external-discovery `--mode json` envelope (ADR-0031 D5):
/// `{ scope, query, total_results, count, results }`. Extracted as a pure
/// function so the wire shape is unit-testable without capturing stdout.
fn external_envelope(query: &str, results: &PaperSearchResults) -> serde_json::Value {
    serde_json::json!({
        "scope": "external",
        "query": query,
        "total_results": results.total_results,
        "count": results.results.len(),
        "results": results.results,
    })
}

/// Pretty-serialize a JSON value and write it as one line to `out`. Shared
/// by the local and external `--mode json` paths.
fn write_json(out: &mut impl Write, value: &serde_json::Value) -> Result<()> {
    let s = serde_json::to_string_pretty(value).context("failed to serialize search JSON")?;
    writeln!(out, "{s}").context("failed to write search JSON to stdout")
}

/// Render an optional value for a human-table cell: its `Display`, or `-`.
fn dash_or<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map(|x| x.to_string()).unwrap_or_else(|| "-".into())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use doiget_core::discovery::{DiscoverySource, PaperHit};

    fn hit() -> PaperHit {
        PaperHit {
            doi: Some("10.1234/x".to_string()),
            openalex_id: "W1".to_string(),
            arxiv: None,
            title: "T".to_string(),
            authors: vec!["A".to_string()],
            year: Some(2024),
            venue: Some("V".to_string()),
            abstract_: Some("abs".to_string()),
            cited_by_count: 3,
            oa_status: Some("gold".to_string()),
            source: DiscoverySource::OpenAlex,
        }
    }

    #[test]
    fn external_envelope_has_scope_total_and_results() {
        let results = PaperSearchResults {
            results: vec![hit()],
            total_results: Some(4012),
        };
        let v = external_envelope("spin glass", &results);
        assert_eq!(v["scope"], "external");
        assert_eq!(v["query"], "spin glass");
        assert_eq!(v["total_results"], 4012);
        assert_eq!(v["count"], 1);
        assert_eq!(v["results"][0]["openalex_id"], "W1");
        assert_eq!(v["results"][0]["abstract"], "abs");
    }

    #[test]
    fn sort_arg_lowers_to_core() {
        // Relevance is the only sort (#290); `cited` / `recent` were removed.
        assert_eq!(SearchSort::from(SortArg::Relevance), SearchSort::Relevance);
    }

    #[test]
    fn local_envelope_has_local_scope_and_count() {
        let v = local_envelope("quantum", &[]);
        assert_eq!(v["scope"], "local");
        assert_eq!(v["query"], "quantum");
        assert_eq!(v["count"], 0);
        assert!(v["results"].as_array().expect("results array").is_empty());
        // The local envelope must NOT carry the external-only field.
        assert!(v.get("total_results").is_none());
    }

    /// `ExternalArgs` with defaults; override per test.
    fn ext(limit: usize, from_year: Option<i32>, to_year: Option<i32>) -> ExternalArgs {
        ExternalArgs {
            limit,
            from_year,
            to_year,
            oa_only: false,
            min_citations: None,
            min_fwci: None,
            min_percentile: None,
            author: None,
            venue: None,
            publisher: None,
            sort: SortArg::Relevance,
        }
    }

    // These validations fire BEFORE any network / harness construction, so
    // the calls error without touching the filesystem or OpenAlex.

    #[tokio::test]
    async fn external_rejects_limit_below_1() {
        let err = run(
            "q".into(),
            false,
            ext(0, None, None),
            OutputMode::Quiet,
            true,
        )
        .await
        .expect_err("limit 0 must be rejected");
        assert!(err.to_string().contains("limit"), "got: {err}");
    }

    #[tokio::test]
    async fn external_rejects_limit_above_200() {
        let err = run(
            "q".into(),
            false,
            ext(201, None, None),
            OutputMode::Quiet,
            true,
        )
        .await
        .expect_err("limit 201 must be rejected");
        assert!(err.to_string().contains("limit"), "got: {err}");
    }

    #[tokio::test]
    async fn external_rejects_inverted_year_range() {
        let err = run(
            "q".into(),
            false,
            ext(25, Some(2025), Some(2010)),
            OutputMode::Quiet,
            true,
        )
        .await
        .expect_err("from_year > to_year must be rejected");
        assert!(err.to_string().contains("is after"), "got: {err}");
    }
}
