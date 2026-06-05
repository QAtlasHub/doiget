//! External literature **discovery search** over OpenAlex `/works?search=`.
//!
//! This is the front half of the #281 research loop (`search → triage →
//! expand → fetch → read → map`). Unlike [`FsStore::search`](crate::store::FsStore)
//! (which re-finds papers already in the local store) and unlike the
//! citation `graph` walker, this module turns a free-text *topic* into a
//! ranked list of candidate papers — each carrying enough metadata
//! (title / abstract / year / venue / citation count / OA status / DOI)
//! for an agent to triage *before* any PDF is fetched.
//!
//! ## Capability tier (ADR-0031)
//!
//! Discovery search is **Tier 1 OA metadata, always-on**: there is no
//! `DOIGET_ENABLE_OPENALEX` gate and no Cargo-feature gate. It ships in
//! the default `oa-only` binary. The justification (ADR-0031 D1) is that
//! a bounded OpenAlex query is the same network-surface risk class as the
//! Crossref / Unpaywall calls Tier 1 already makes on every fetch:
//! read-only OA metadata, never paywalled, never a PDF.
//!
//! This is deliberately **distinct** from `crate::sources::openalex`
//! (the `#[cfg(feature = "metadata")]` enrichment / `referenced_works[]`
//! source used by `graph`, which stays Tier 2 behind
//! `DOIGET_ENABLE_OPENALEX`). The `Source` trait is `ref → FetchResult`;
//! search is `query → list`, so it does not fit that trait and lives here
//! as a free function reusing only the shared [`HttpClient`], rate
//! limiter, and provenance log via [`FetchContext`].
//!
//! ## Author / venue / publisher filters (ADR-0031 D5)
//!
//! OpenAlex filters authors / sources (venues) / publishers by **entity
//! ID**, not free text. So `paper_search` first resolves a supplied
//! `--author` / `--venue` / `--publisher` *name* to its OpenAlex ID via a
//! `?search=` lookup against `/authors`, `/sources`, `/publishers`, then
//! filters `/works` by `authorships.author.id` /
//! `primary_location.source.id` /
//! `primary_location.source.publisher_lineage`. The top hit is NOT taken
//! blindly: [`select_entity`] resolves only an unambiguous name (a single
//! hit, an exact case-insensitive name match, or a top hit that clearly
//! out-scores the runner-up); a name matching several entities with no
//! clear winner is a typed [`FetchError::Ambiguous`] listing the
//! candidates, and a name matching nothing is [`FetchError::NotFound`].
//! The filter is never silently dropped.
//!
//! ## Metadata-only contract (ADR-0031 D3)
//!
//! Every call here uses [`HttpClient::fetch_bytes`] (a JSON body),
//! **never** `fetch_pdf`, and never follows an OA URL. The abstract is
//! reconstructed from OpenAlex's `abstract_inverted_index`.
//!
//! [`HttpClient`]: crate::http::HttpClient
//! [`HttpClient::fetch_bytes`]: crate::http::HttpClient::fetch_bytes

use serde::Serialize;
use url::Url;

use crate::provenance::{Capability, LogEvent, LogResult, RowInput};
use crate::source::{FetchContext, FetchError};

/// Source key used for the per-source HTTP client + redirect allowlist.
///
/// Shares the `"openalex"` key with `crate::sources::openalex` so that
/// `crate::http::discovery_allowlist` (always compiled) and
/// `tier_2_allowlist` (always compiled, but only *called* by the CLI
/// under `#[cfg(feature = "citation")]`) register the same
/// `api.openalex.org` host under one key (an idempotent overwrite — see
/// ADR-0031 D2).
const SOURCE_KEY: &str = "openalex";

/// OpenAlex `select=` field list. Bounds the response payload to exactly
/// the top-level fields [`PaperHit`] needs; every entry here is a
/// top-level Work field (nested selection is not used).
const SELECT_FIELDS: &str = "id,doi,title,display_name,publication_year,\
cited_by_count,abstract_inverted_index,authorships,primary_location,\
open_access,locations";

/// OpenAlex caps `per-page` at 200; requests above that are rejected.
const MAX_PER_PAGE: usize = 200;

/// Default page size when the caller does not specify `--limit`.
pub const DEFAULT_LIMIT: usize = 25;

/// Ordering applied to the discovery result set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchSort {
    /// OpenAlex `relevance_score:desc` — best textual match to `query`
    /// first. The default; only meaningful because a `search` term is
    /// always present.
    Relevance,
    /// `cited_by_count:desc` — most-cited first (surface the canonical /
    /// high-impact papers in a topic).
    Cited,
    /// `publication_date:desc` — newest first (surface the frontier).
    Recent,
}

impl SearchSort {
    /// The OpenAlex `sort=` parameter value for this ordering.
    #[must_use]
    pub fn as_openalex(self) -> &'static str {
        match self {
            SearchSort::Relevance => "relevance_score:desc",
            SearchSort::Cited => "cited_by_count:desc",
            SearchSort::Recent => "publication_date:desc",
        }
    }
}

/// A discovery-search request: the free-text query plus triage filters.
///
/// Construct directly (all fields are public); the CLI maps its flags
/// onto this.
#[derive(Debug, Clone)]
pub struct PaperSearchQuery {
    /// Free-text topic query (e.g. "tropical tensor networks for spin
    /// glasses"). Must be non-empty; the caller is expected to reject
    /// empty input.
    pub query: String,
    /// Maximum number of results to return. Clamped to `1..=200`
    /// (OpenAlex `per-page` ceiling).
    pub limit: usize,
    /// Inclusive lower bound on publication year (maps to OpenAlex
    /// `from_publication_date:<year>-01-01`).
    pub from_year: Option<i32>,
    /// Inclusive upper bound on publication year (maps to OpenAlex
    /// `to_publication_date:<year>-12-31`).
    pub to_year: Option<i32>,
    /// When `true`, restrict to open-access works (`is_oa:true`).
    pub oa_only: bool,
    /// Minimum citation count. Maps to OpenAlex `cited_by_count:>{n}`
    /// ("more than n"); the off-by-one versus "at least n" is documented
    /// on the CLI flag.
    pub min_citations: Option<u64>,
    /// Author name to filter by. Resolved to an OpenAlex author ID via
    /// `/authors?search=` then applied as `authorships.author.id`.
    pub author: Option<String>,
    /// Venue / journal name to filter by. Resolved to an OpenAlex source
    /// ID via `/sources?search=` then applied as
    /// `primary_location.source.id`.
    pub venue: Option<String>,
    /// Publisher name to filter by. Resolved to an OpenAlex publisher ID
    /// via `/publishers?search=` then applied as
    /// `primary_location.source.publisher_lineage`.
    pub publisher: Option<String>,
    /// Result ordering.
    pub sort: SearchSort,
}

impl PaperSearchQuery {
    /// A bare query with [`DEFAULT_LIMIT`], no filters, relevance sort.
    #[must_use]
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            limit: DEFAULT_LIMIT,
            from_year: None,
            to_year: None,
            oa_only: false,
            min_citations: None,
            author: None,
            venue: None,
            publisher: None,
            sort: SearchSort::Relevance,
        }
    }
}

/// One candidate paper returned by discovery search.
///
/// All fields except `openalex_id` / `title` / `cited_by_count` /
/// `source` are `Option` because OpenAlex omits them for some records
/// (e.g. no DOI for a dataset, no abstract for an Elsevier-gated
/// abstract). Absent fields serialize to JSON `null` (not skipped) so
/// the wire shape is stable for agents.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PaperHit {
    /// Bare DOI (lower-cased, `https://doi.org/` prefix stripped), or
    /// `None` when the record has no DOI.
    pub doi: Option<String>,
    /// OpenAlex Work ID (`W…`, `https://openalex.org/` prefix stripped).
    pub openalex_id: String,
    /// arXiv id, best-effort extracted from a `locations[].*url`
    /// containing `arxiv.org/abs/<id>`; `None` if no arXiv location.
    pub arxiv: Option<String>,
    /// Work title.
    pub title: String,
    /// Author display names, in OpenAlex authorship order.
    pub authors: Vec<String>,
    /// Publication year, or `None` if absent.
    pub year: Option<i32>,
    /// Primary venue display name (journal / repository), or `None`.
    pub venue: Option<String>,
    /// Reconstructed abstract text, or `None` when OpenAlex has no
    /// `abstract_inverted_index` for the record.
    #[serde(rename = "abstract")]
    pub abstract_: Option<String>,
    /// OpenAlex `cited_by_count`.
    pub cited_by_count: u64,
    /// OpenAlex open-access status (`gold` / `green` / `hybrid` /
    /// `bronze` / `closed`), or `None`.
    pub oa_status: Option<String>,
    /// Provenance of the record. Always `"openalex"` in PR1.
    pub source: &'static str,
}

/// The result of a discovery search: the hits plus the upstream total.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PaperSearchResults {
    /// The candidate papers (length ≤ `query.limit`).
    pub results: Vec<PaperHit>,
    /// OpenAlex `meta.count` — the total number of matching works
    /// upstream (usually far larger than `results.len()`), or `None` if
    /// the response omitted it. Lets an agent see "showing 25 of 4012".
    pub total_results: Option<u64>,
}

/// OpenAlex entity IDs resolved from the `--author` / `--venue` /
/// `--publisher` name filters (each `None` when the filter is unset).
#[derive(Debug, Default)]
struct ResolvedIds {
    /// Author ID (`A…`) for `authorships.author.id`.
    author: Option<String>,
    /// Source ID (`S…`) for `primary_location.source.id`.
    source: Option<String>,
    /// Publisher ID (`P…`) for `primary_location.source.publisher_lineage`.
    publisher: Option<String>,
}

/// Run a discovery search against OpenAlex and return ranked candidates.
///
/// `base` is the OpenAlex API base URL (production
/// `https://api.openalex.org`; tests inject a wiremock origin, mirroring
/// the `DOIGET_OPENALEX_BASE` override the CLI honors). `contact_email`
/// opts into the polite pool via `?mailto=` when non-empty.
///
/// When `query.author` / `query.venue` / `query.publisher` are set, this
/// first issues one `?search=` lookup each against `/authors` /
/// `/sources` / `/publishers` to resolve the name to an OpenAlex ID, then
/// filters `/works` by that ID. Every call reuses `ctx.http` (allowlisted,
/// HTTPS-only in production), `ctx.rate_limiter`, and `ctx.log` (one
/// `Metadata`/`Fetch` provenance row per request). Never fetches a PDF
/// (ADR-0031 D3).
///
/// # Errors
///
/// Returns [`FetchError::Http`] for transport / allowlist failures,
/// [`FetchError::NotFound`] when an author/venue/publisher name resolves
/// to nothing, [`FetchError::Ambiguous`] when such a name matches several
/// entities with no clear winner (carries a candidate listing),
/// [`FetchError::SourceSchema`] when a response is not a JSON object
/// carrying a `results` array, and propagates a provenance-log append
/// failure (fail-closed).
pub async fn paper_search(
    base: &Url,
    contact_email: &str,
    query: &PaperSearchQuery,
    ctx: &FetchContext,
) -> Result<PaperSearchResults, FetchError> {
    // Resolve the name → ID filters first (one OpenAlex lookup each).
    let ids = ResolvedIds {
        author: resolve_optional(base, contact_email, "authors", &query.author, ctx).await?,
        source: resolve_optional(base, contact_email, "sources", &query.venue, ctx).await?,
        publisher: resolve_optional(base, contact_email, "publishers", &query.publisher, ctx)
            .await?,
    };

    let url = build_search_url(base, contact_email, query, &ids)?;
    let (value, _bytes) = openalex_get(&url, ctx).await?;

    let results_array = value
        .get("results")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| FetchError::SourceSchema {
            hint: format!(
                "openalex search response missing `results` array — likely an \
                 error payload (got: {})",
                truncate_for_hint(value.to_string().as_bytes())
            ),
        })?;

    let results: Vec<PaperHit> = results_array.iter().map(work_to_hit).collect();
    let total_results = value
        .get("meta")
        .and_then(|m| m.get("count"))
        .and_then(serde_json::Value::as_u64);

    Ok(PaperSearchResults {
        results,
        total_results,
    })
}

/// Issue one OpenAlex GET: rate-limit, fetch the JSON body, parse it, and
/// append the `Metadata`/`Fetch` provenance row. Returns the parsed value
/// plus the byte length (the caller needs neither beyond the value, but
/// the length keeps the provenance accounting in one place).
async fn openalex_get(
    url: &Url,
    ctx: &FetchContext,
) -> Result<(serde_json::Value, usize), FetchError> {
    // Step 1: rate limiter (politeness — same channel every source uses).
    let _permit = ctx.rate_limiter.acquire(SOURCE_KEY).await;

    // Step 2: HTTP fetch (JSON; `select=`/`per-page=` keep it small).
    let (body, _final_url) = ctx.http.fetch_bytes(SOURCE_KEY, url.clone()).await?;

    // Step 3: parse.
    let value: serde_json::Value =
        serde_json::from_slice(&body).map_err(|e| FetchError::SourceSchema {
            hint: format!("openalex returned non-JSON: {e}"),
        })?;

    // Step 4: provenance. Tier-1 metadata read; no single ref (it is a
    // query), so `ref_` / `canonical_digest` are null per
    // docs/PROVENANCE_LOG.md.
    ctx.log.append(RowInput {
        event: LogEvent::Fetch,
        result: LogResult::Ok,
        capability: Capability::Metadata,
        ref_: None,
        source: Some(SOURCE_KEY),
        error_code: None,
        size_bytes: Some(body.len() as u64),
        license: None,
        store_path: None,
        canonical_digest: None,
    })?;

    Ok((value, body.len()))
}

/// Resolve an optional name filter to an OpenAlex entity ID, or `None`
/// when the name is unset / blank.
async fn resolve_optional(
    base: &Url,
    contact_email: &str,
    entity_path: &str,
    name: &Option<String>,
    ctx: &FetchContext,
) -> Result<Option<String>, FetchError> {
    match name {
        Some(n) if !n.trim().is_empty() => Ok(Some(
            resolve_entity_id(base, contact_email, entity_path, n, ctx).await?,
        )),
        _ => Ok(None),
    }
}

/// Resolve a name to a single OpenAlex entity ID for `entity_path`
/// (`authors` / `sources` / `publishers`) via `?search=`.
///
/// OpenAlex `?search=` is partial / fuzzy and relevance-ranked, so a
/// vague name still matches. To avoid silently filtering by the wrong
/// entity, this fetches the top few candidates and applies
/// [`select_entity`]: an unambiguous name (single hit, an exact-name
/// match, or a clearly-dominant top hit) resolves; an ambiguous one is a
/// typed [`FetchError::Ambiguous`] that lists the candidates so the
/// caller can narrow the name. A name that matches nothing is
/// [`FetchError::NotFound`]. The filter is never silently dropped.
async fn resolve_entity_id(
    base: &Url,
    contact_email: &str,
    entity_path: &str,
    name: &str,
    ctx: &FetchContext,
) -> Result<String, FetchError> {
    let mut url = base
        .join(&format!("/{entity_path}"))
        .map_err(|e| FetchError::SourceSchema {
            hint: format!("openalex {entity_path} URL construction failed: {e}"),
        })?;
    {
        let mut qp = url.query_pairs_mut();
        qp.append_pair("search", name);
        // Top few candidates so an ambiguous name can be reported with
        // alternatives instead of silently resolving to the first hit.
        // No `select=` so OpenAlex returns `relevance_score` (only present
        // on search responses) alongside `display_name` / `works_count`.
        qp.append_pair("per-page", "5");
        if !contact_email.is_empty() {
            qp.append_pair("mailto", contact_email);
        }
    }

    let (value, _len) = openalex_get(&url, ctx).await?;
    // A valid JSON object with no `results` array is a schema failure
    // (e.g. an OpenAlex error envelope: rate limit / bad filter), NOT an
    // empty match set — mirror the `/works` path. Collapsing it to an
    // empty Vec here would surface a misleading "no <entity> matched"
    // NotFound and silently drop the user's filter.
    let results_arr = value
        .get("results")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| FetchError::SourceSchema {
            hint: format!(
                "openalex /{entity_path} response missing `results` array — likely an \
                 error payload (got: {})",
                truncate_for_hint(value.to_string().as_bytes())
            ),
        })?;
    let mut candidates: Vec<Candidate> = results_arr
        .iter()
        .filter_map(Candidate::from_value)
        .collect();
    // OpenAlex returns search hits relevance-sorted, but make the
    // dominance check order-independent.
    candidates.sort_by(|a, b| {
        b.relevance
            .partial_cmp(&a.relevance)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    select_entity(entity_path, name, &candidates)
}

/// One OpenAlex entity-search candidate (author / source / publisher).
struct Candidate {
    /// Bare OpenAlex ID (`A…` / `S…` / `P…`).
    id: String,
    /// Entity display name (used for the exact-match check + listings).
    display_name: String,
    /// Number of works attributed to the entity (shown in the ambiguity
    /// listing so the caller can spot the prolific / canonical match).
    works_count: u64,
    /// OpenAlex `relevance_score` for the search query (0.0 if absent).
    relevance: f64,
}

impl Candidate {
    fn from_value(v: &serde_json::Value) -> Option<Self> {
        let id = v
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map(strip_openalex_prefix)?;
        Some(Self {
            id,
            display_name: v
                .get("display_name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string(),
            works_count: v
                .get("works_count")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
            relevance: v
                .get("relevance_score")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.0),
        })
    }
}

/// Relevance-dominance ratio: with no exact-name match, the top hit must
/// out-score the runner-up by at least this factor to be auto-selected;
/// otherwise the name is treated as ambiguous.
const DOMINANCE_RATIO: f64 = 2.0;

/// Pick a single entity from relevance-sorted search `candidates`, or
/// report ambiguity.
///
/// Resolution order: empty → [`FetchError::NotFound`]; single candidate →
/// it; exactly one case-insensitive exact display-name match → it; else
/// the top hit when it out-scores the runner-up by [`DOMINANCE_RATIO`];
/// otherwise [`FetchError::Ambiguous`] listing the candidates.
fn select_entity(
    entity_path: &str,
    name: &str,
    candidates: &[Candidate],
) -> Result<String, FetchError> {
    let label = entity_label(entity_path);
    if candidates.is_empty() {
        return Err(FetchError::NotFound {
            hint: format!("no OpenAlex {label} matched '{name}'"),
        });
    }
    if candidates.len() == 1 {
        return Ok(candidates[0].id.clone());
    }

    let exact: Vec<&Candidate> = candidates
        .iter()
        .filter(|c| c.display_name.trim().eq_ignore_ascii_case(name.trim()))
        .collect();
    if exact.len() == 1 {
        return Ok(exact[0].id.clone());
    }

    if exact.is_empty() {
        let top = &candidates[0];
        let second = &candidates[1];
        // Both scores must be present (> 0.0): a runner-up with an absent
        // `relevance_score` (defaulted to 0.0) would otherwise make
        // `top >= RATIO * 0.0` trivially true and silently auto-select the
        // top hit, defeating the ambiguity guard. When the runner-up has
        // no score we cannot judge dominance — treat the name as ambiguous.
        if top.relevance > 0.0
            && second.relevance > 0.0
            && top.relevance >= DOMINANCE_RATIO * second.relevance
        {
            return Ok(top.id.clone());
        }
    }

    Err(FetchError::Ambiguous {
        hint: format_ambiguous(label, name, candidates),
    })
}

/// Singular human label for an OpenAlex entity path.
fn entity_label(entity_path: &str) -> &str {
    match entity_path {
        "authors" => "author",
        "sources" => "venue",
        "publishers" => "publisher",
        other => other,
    }
}

/// Render the ambiguity error: the query plus the candidate listing
/// (display name, id, works count) so the caller can narrow the name.
fn format_ambiguous(label: &str, name: &str, candidates: &[Candidate]) -> String {
    let mut s = format!(
        "ambiguous {label} '{name}' — {} candidates; narrow the name \
         (add a first name / fuller title) and retry:",
        candidates.len()
    );
    for c in candidates.iter().take(5) {
        s.push_str(&format!(
            "\n  {} ({}, {} works)",
            c.display_name, c.id, c.works_count
        ));
    }
    s
}

/// Build the `/works?search=&filter=&sort=&select=&per-page=&mailto=` URL.
fn build_search_url(
    base: &Url,
    contact_email: &str,
    query: &PaperSearchQuery,
    ids: &ResolvedIds,
) -> Result<Url, FetchError> {
    let mut url = base.join("/works").map_err(|e| FetchError::SourceSchema {
        hint: format!("openalex search URL construction failed: {e}"),
    })?;

    let per_page = query.limit.clamp(1, MAX_PER_PAGE);

    // Compose the comma-joined `filter=` value. OpenAlex treats commas as
    // an AND of clauses within a single `filter` parameter.
    let mut filters: Vec<String> = Vec::new();
    if let Some(from) = query.from_year {
        filters.push(format!("from_publication_date:{from}-01-01"));
    }
    if let Some(to) = query.to_year {
        filters.push(format!("to_publication_date:{to}-12-31"));
    }
    if query.oa_only {
        filters.push("is_oa:true".to_string());
    }
    if let Some(min) = query.min_citations {
        // `cited_by_count:>{n}` matches works cited strictly more than
        // `n` times. The off-by-one versus "at least n" is documented on
        // the CLI flag.
        filters.push(format!("cited_by_count:>{min}"));
    }
    if let Some(author_id) = &ids.author {
        filters.push(format!("authorships.author.id:{author_id}"));
    }
    if let Some(source_id) = &ids.source {
        filters.push(format!("primary_location.source.id:{source_id}"));
    }
    if let Some(publisher_id) = &ids.publisher {
        filters.push(format!(
            "primary_location.source.publisher_lineage:{publisher_id}"
        ));
    }

    {
        let mut qp = url.query_pairs_mut();
        qp.append_pair("search", &query.query);
        qp.append_pair("per-page", &per_page.to_string());
        qp.append_pair("sort", query.sort.as_openalex());
        qp.append_pair("select", SELECT_FIELDS);
        if !filters.is_empty() {
            qp.append_pair("filter", &filters.join(","));
        }
        if !contact_email.is_empty() {
            qp.append_pair("mailto", contact_email);
        }
    }

    Ok(url)
}

/// Map one OpenAlex Work JSON object to a [`PaperHit`].
///
/// Tolerant of missing fields: anything absent becomes `None` / empty
/// rather than failing the whole search (one malformed record should not
/// sink the page).
fn work_to_hit(work: &serde_json::Value) -> PaperHit {
    let openalex_id = work
        .get("id")
        .and_then(serde_json::Value::as_str)
        .map(strip_openalex_prefix)
        .unwrap_or_default();

    let doi = work
        .get("doi")
        .and_then(serde_json::Value::as_str)
        .map(strip_doi_prefix);

    let title = work
        .get("title")
        .and_then(serde_json::Value::as_str)
        .or_else(|| work.get("display_name").and_then(serde_json::Value::as_str))
        .unwrap_or("")
        .to_string();

    let authors = work
        .get("authorships")
        .and_then(serde_json::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|a| {
                    a.get("author")
                        .and_then(|au| au.get("display_name"))
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default();

    let year = work
        .get("publication_year")
        .and_then(serde_json::Value::as_i64)
        .and_then(|y| i32::try_from(y).ok());

    let venue = work
        .get("primary_location")
        .and_then(|loc| loc.get("source"))
        .and_then(|src| src.get("display_name"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);

    let abstract_ = work
        .get("abstract_inverted_index")
        .and_then(reconstruct_abstract);

    let cited_by_count = work
        .get("cited_by_count")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);

    let oa_status = work
        .get("open_access")
        .and_then(|oa| oa.get("oa_status"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);

    let arxiv = work
        .get("locations")
        .and_then(serde_json::Value::as_array)
        .and_then(|locs| locs.iter().find_map(extract_arxiv_from_location));

    PaperHit {
        doi,
        openalex_id,
        arxiv,
        title,
        authors,
        year,
        venue,
        abstract_,
        cited_by_count,
        oa_status,
        source: SOURCE_KEY,
    }
}

/// Reconstruct plain abstract text from OpenAlex's
/// `abstract_inverted_index` (`{ word: [positions...] }`). Returns `None`
/// for a null / empty / non-object value.
fn reconstruct_abstract(inv: &serde_json::Value) -> Option<String> {
    let map = inv.as_object()?;
    if map.is_empty() {
        return None;
    }
    let mut positioned: Vec<(u64, &str)> = Vec::new();
    for (word, positions) in map {
        if let Some(arr) = positions.as_array() {
            for p in arr {
                if let Some(pos) = p.as_u64() {
                    positioned.push((pos, word.as_str()));
                }
            }
        }
    }
    if positioned.is_empty() {
        return None;
    }
    positioned.sort_by_key(|(pos, _)| *pos);
    let words: Vec<&str> = positioned.into_iter().map(|(_, w)| w).collect();
    Some(words.join(" "))
}

/// Best-effort arXiv id extraction from a single OpenAlex location's
/// `landing_page_url` / `pdf_url`. Looks for `arxiv.org/abs/<id>` and
/// returns `<id>` (a trailing `vN` version is kept — the downstream
/// parser accepts it).
fn extract_arxiv_from_location(loc: &serde_json::Value) -> Option<String> {
    for key in ["landing_page_url", "pdf_url"] {
        if let Some(u) = loc.get(key).and_then(serde_json::Value::as_str) {
            if let Some(idx) = u.find("arxiv.org/abs/") {
                let after = &u[idx + "arxiv.org/abs/".len()..];
                let id: String = after
                    .chars()
                    .take_while(|c| !matches!(c, '?' | '#' | '/' | ' '))
                    .collect();
                if !id.is_empty() {
                    return Some(id);
                }
            }
        }
    }
    None
}

/// Strip the `https://openalex.org/` prefix from an entity id, yielding
/// the bare `W…` / `A…` / `S…` / `P…` form.
fn strip_openalex_prefix(id: &str) -> String {
    id.rsplit('/').next().unwrap_or(id).to_string()
}

/// Strip the `https://doi.org/` (or `http://…`) prefix from a DOI URL and
/// lower-case it (DOIs are case-insensitive; lower-case is the canonical
/// store form).
fn strip_doi_prefix(doi_url: &str) -> String {
    let lower = doi_url.to_ascii_lowercase();
    lower
        .strip_prefix("https://doi.org/")
        .or_else(|| lower.strip_prefix("http://doi.org/"))
        .unwrap_or(&lower)
        .to_string()
}

/// Truncate a response body to a short prefix for error hints, so a
/// multi-KB malformed payload does not flood a single log line.
///
/// Truncation is by `char` (not byte) so a multi-byte UTF-8 character
/// straddling the cap — common in OpenAlex error payloads, which embed
/// `…`/curly quotes — never panics on a non-char-boundary byte slice.
fn truncate_for_hint(body: &[u8]) -> String {
    const MAX: usize = 200;
    let s = String::from_utf8_lossy(body);
    if s.chars().count() <= MAX {
        s.into_owned()
    } else {
        let head: String = s.chars().take(MAX).collect();
        format!("{head}…")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    use std::sync::Arc;

    use camino::Utf8PathBuf;
    use tempfile::TempDir;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::http::HttpClient;
    use crate::provenance::ProvenanceLog;
    use crate::rate_limiter::RateLimiter;
    use crate::RateLimits;

    /// Hand-crafted (not a snapshot) OpenAlex `/works` search response.
    /// Synthetic to avoid third-party redistribution concerns; exercises
    /// every `PaperHit` field including abstract reconstruction, arXiv
    /// extraction, and the all-absent record.
    const SAMPLE_SEARCH: &str = r#"{
        "meta": { "count": 4012, "per_page": 25 },
        "results": [
            {
                "id": "https://openalex.org/W123",
                "doi": "https://doi.org/10.1234/Example",
                "title": "Tropical Tensor Networks",
                "display_name": "Tropical Tensor Networks",
                "publication_year": 2021,
                "cited_by_count": 42,
                "abstract_inverted_index": { "Tropical": [0], "tensor": [1], "networks": [2] },
                "authorships": [
                    { "author": { "display_name": "Ada Lovelace" } },
                    { "author": { "display_name": "Alan Turing" } }
                ],
                "primary_location": { "source": { "display_name": "Phys. Rev. B" } },
                "open_access": { "oa_status": "green", "is_oa": true },
                "locations": [
                    { "landing_page_url": "https://arxiv.org/abs/2101.12345v2" }
                ]
            },
            {
                "id": "https://openalex.org/W456",
                "doi": null,
                "title": "Second Paper",
                "publication_year": 2019,
                "cited_by_count": 7,
                "abstract_inverted_index": null,
                "authorships": [],
                "open_access": { "oa_status": "closed" }
            }
        ]
    }"#;

    fn build_test_context(wiremock_host: &str) -> (TempDir, FetchContext) {
        let td = TempDir::new().expect("tempdir");
        let log_dir =
            Utf8PathBuf::try_from(td.path().to_path_buf()).expect("temp dir path must be UTF-8");
        let log_path = log_dir.join("test.jsonl");

        let http = Arc::new(HttpClient::new_for_tests_allow_http(
            "openalex",
            wiremock_host,
        ));
        let rate_limiter = Arc::new(RateLimiter::new(RateLimits::HARD_CODED));
        let session_id = "01J0000000000000000000TEST".to_string();
        let log = Arc::new(
            ProvenanceLog::open(log_path, session_id.clone()).expect("provenance log opens"),
        );
        let ctx = FetchContext {
            http,
            rate_limiter,
            log,
            session_id,
            cache_root: None,
        };
        (td, ctx)
    }

    #[tokio::test]
    async fn search_maps_works_to_hits() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/works"))
            .and(query_param("search", "tropical tensor networks"))
            .and(query_param("mailto", "doiget@localhost"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_SEARCH))
            .mount(&server)
            .await;

        let (_td, ctx) = build_test_context(&server.uri());
        let base = Url::parse(&server.uri()).expect("wiremock URI parses");
        let q = PaperSearchQuery::new("tropical tensor networks");

        let out = paper_search(&base, "doiget@localhost", &q, &ctx)
            .await
            .expect("search ok");

        assert_eq!(out.total_results, Some(4012));
        assert_eq!(out.results.len(), 2);

        let first = &out.results[0];
        assert_eq!(first.openalex_id, "W123");
        assert_eq!(first.doi.as_deref(), Some("10.1234/example")); // lower-cased
        assert_eq!(first.title, "Tropical Tensor Networks");
        assert_eq!(first.year, Some(2021));
        assert_eq!(first.cited_by_count, 42);
        assert_eq!(first.abstract_.as_deref(), Some("Tropical tensor networks"));
        assert_eq!(first.authors, vec!["Ada Lovelace", "Alan Turing"]);
        assert_eq!(first.venue.as_deref(), Some("Phys. Rev. B"));
        assert_eq!(first.oa_status.as_deref(), Some("green"));
        assert_eq!(first.arxiv.as_deref(), Some("2101.12345v2"));
        assert_eq!(first.source, "openalex");

        let second = &out.results[1];
        assert_eq!(second.openalex_id, "W456");
        assert_eq!(second.doi, None);
        assert_eq!(second.abstract_, None);
        assert_eq!(second.venue, None);
        assert!(second.authors.is_empty());
        assert_eq!(second.oa_status.as_deref(), Some("closed"));
        assert_eq!(second.arxiv, None);
    }

    #[tokio::test]
    async fn search_filters_and_sort_land_on_the_url() {
        let server = MockServer::start().await;
        // Assert the composed filter + sort params reach the wire.
        Mock::given(method("GET"))
            .and(path("/works"))
            .and(query_param("sort", "cited_by_count:desc"))
            .and(query_param(
                "filter",
                "from_publication_date:2020-01-01,is_oa:true,cited_by_count:>10",
            ))
            .and(query_param("per-page", "5"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{ "meta": { "count": 0 }, "results": [] }"#),
            )
            .mount(&server)
            .await;

        let (_td, ctx) = build_test_context(&server.uri());
        let base = Url::parse(&server.uri()).expect("wiremock URI parses");
        let q = PaperSearchQuery {
            query: "spin glass".to_string(),
            limit: 5,
            from_year: Some(2020),
            to_year: None,
            oa_only: true,
            min_citations: Some(10),
            author: None,
            venue: None,
            publisher: None,
            sort: SearchSort::Cited,
        };

        let out = paper_search(&base, "doiget@localhost", &q, &ctx)
            .await
            .expect("search ok");
        assert_eq!(out.total_results, Some(0));
        assert!(out.results.is_empty());
    }

    #[tokio::test]
    async fn search_error_payload_is_source_schema() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/works"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"error":"Invalid query parameters"}"#),
            )
            .mount(&server)
            .await;

        let (_td, ctx) = build_test_context(&server.uri());
        let base = Url::parse(&server.uri()).expect("wiremock URI parses");
        let q = PaperSearchQuery::new("anything");

        let err = paper_search(&base, "", &q, &ctx)
            .await
            .expect_err("missing `results` must surface as SourceSchema");
        assert!(matches!(err, FetchError::SourceSchema { .. }));
    }

    #[test]
    fn name_filters_compose_into_resolved_ids() {
        let base = Url::parse("https://api.openalex.org").expect("base parses");
        let q = PaperSearchQuery::new("topic");
        let ids = ResolvedIds {
            author: Some("A1".to_string()),
            source: Some("S2".to_string()),
            publisher: Some("P3".to_string()),
        };
        let url = build_search_url(&base, "", &q, &ids).expect("url builds");
        let filter = url
            .query_pairs()
            .find(|(k, _)| k == "filter")
            .map(|(_, v)| v.into_owned())
            .expect("filter param present");
        assert!(filter.contains("authorships.author.id:A1"), "got {filter}");
        assert!(
            filter.contains("primary_location.source.id:S2"),
            "got {filter}"
        );
        assert!(
            filter.contains("primary_location.source.publisher_lineage:P3"),
            "got {filter}"
        );
    }

    #[tokio::test]
    async fn venue_name_resolves_to_source_id_then_filters_works() {
        let server = MockServer::start().await;
        // First leg: /sources?search=... → top hit S99.
        Mock::given(method("GET"))
            .and(path("/sources"))
            .and(query_param("search", "Physical Review B"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{ "results": [ { "id": "https://openalex.org/S99", "display_name": "Physical Review B" } ] }"#,
            ))
            .mount(&server)
            .await;
        // Second leg: /works filtered by the resolved source id.
        Mock::given(method("GET"))
            .and(path("/works"))
            .and(query_param("filter", "primary_location.source.id:S99"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{ "meta": { "count": 1 }, "results": [ { "id": "https://openalex.org/W1", "title": "In PRB" } ] }"#,
            ))
            .mount(&server)
            .await;

        let (_td, ctx) = build_test_context(&server.uri());
        let base = Url::parse(&server.uri()).expect("wiremock URI parses");
        let mut q = PaperSearchQuery::new("spin glass");
        q.venue = Some("Physical Review B".to_string());

        let out = paper_search(&base, "", &q, &ctx)
            .await
            .expect("venue-filtered search ok");
        assert_eq!(out.total_results, Some(1));
        assert_eq!(out.results.len(), 1);
        assert_eq!(out.results[0].openalex_id, "W1");
    }

    #[tokio::test]
    async fn unresolvable_venue_name_is_not_found() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/sources"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{ "results": [] }"#))
            .mount(&server)
            .await;

        let (_td, ctx) = build_test_context(&server.uri());
        let base = Url::parse(&server.uri()).expect("wiremock URI parses");
        let mut q = PaperSearchQuery::new("spin glass");
        q.venue = Some("No Such Journal".to_string());

        let err = paper_search(&base, "", &q, &ctx)
            .await
            .expect_err("an unresolvable venue name must error, not silently drop the filter");
        assert!(matches!(err, FetchError::NotFound { .. }), "got {err:?}");
    }

    #[tokio::test]
    async fn exact_name_match_resolves_amid_namesakes() {
        let server = MockServer::start().await;
        // Three sources match the search; only one is an exact name match.
        Mock::given(method("GET"))
            .and(path("/sources"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{ "results": [
                    { "id": "https://openalex.org/S1", "display_name": "Physical Review B", "works_count": 50000, "relevance_score": 80.0 },
                    { "id": "https://openalex.org/S2", "display_name": "Physical Review B: Condensed Matter", "works_count": 1000, "relevance_score": 78.0 },
                    { "id": "https://openalex.org/S3", "display_name": "Reviews of Physics", "works_count": 200, "relevance_score": 70.0 }
                ] }"#,
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/works"))
            .and(query_param("filter", "primary_location.source.id:S1"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{ "meta": { "count": 1 }, "results": [ { "id": "https://openalex.org/W1", "title": "x" } ] }"#,
            ))
            .mount(&server)
            .await;

        let (_td, ctx) = build_test_context(&server.uri());
        let base = Url::parse(&server.uri()).expect("wiremock URI parses");
        let mut q = PaperSearchQuery::new("spin glass");
        q.venue = Some("Physical Review B".to_string());

        let out = paper_search(&base, "", &q, &ctx)
            .await
            .expect("exact venue name must resolve to S1 amid namesakes");
        assert_eq!(out.results[0].openalex_id, "W1");
    }

    #[tokio::test]
    async fn dominant_top_hit_resolves_for_vague_name() {
        let server = MockServer::start().await;
        // No exact match for "parisi", but the top hit dominates (>=2x).
        Mock::given(method("GET"))
            .and(path("/authors"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{ "results": [
                    { "id": "https://openalex.org/A1", "display_name": "Giorgio Parisi", "works_count": 400, "relevance_score": 100.0 },
                    { "id": "https://openalex.org/A2", "display_name": "M. Parisi", "works_count": 10, "relevance_score": 20.0 }
                ] }"#,
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/works"))
            .and(query_param("filter", "authorships.author.id:A1"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{ "meta": { "count": 1 }, "results": [ { "id": "https://openalex.org/W9", "title": "y" } ] }"#,
            ))
            .mount(&server)
            .await;

        let (_td, ctx) = build_test_context(&server.uri());
        let base = Url::parse(&server.uri()).expect("wiremock URI parses");
        let mut q = PaperSearchQuery::new("replica symmetry breaking");
        q.author = Some("parisi".to_string());

        let out = paper_search(&base, "", &q, &ctx)
            .await
            .expect("a dominant top hit must resolve a vague name");
        assert_eq!(out.results[0].openalex_id, "W9");
    }

    #[tokio::test]
    async fn ambiguous_name_errors_with_candidate_listing() {
        let server = MockServer::start().await;
        // Two close, non-exact matches → ambiguous; no /works call.
        Mock::given(method("GET"))
            .and(path("/authors"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{ "results": [
                    { "id": "https://openalex.org/A1", "display_name": "John Smith", "works_count": 300, "relevance_score": 50.0 },
                    { "id": "https://openalex.org/A2", "display_name": "Jane Smith", "works_count": 280, "relevance_score": 45.0 }
                ] }"#,
            ))
            .mount(&server)
            .await;

        let (_td, ctx) = build_test_context(&server.uri());
        let base = Url::parse(&server.uri()).expect("wiremock URI parses");
        let mut q = PaperSearchQuery::new("electrons");
        q.author = Some("Smith".to_string());

        let err = paper_search(&base, "", &q, &ctx)
            .await
            .expect_err("a close, non-exact multi-match must be reported as ambiguous");
        match err {
            FetchError::Ambiguous { hint } => {
                assert!(hint.contains("John Smith"), "hint lists candidates: {hint}");
                assert!(hint.contains("Jane Smith"), "hint lists candidates: {hint}");
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn abstract_reconstruction_orders_by_position() {
        let inv = serde_json::json!({
            "world": [1],
            "hello": [0],
            "again": [3],
            "hello2": [2]
        });
        // positions: 0=hello, 1=world, 2=hello2, 3=again
        assert_eq!(
            reconstruct_abstract(&inv).as_deref(),
            Some("hello world hello2 again")
        );
        assert_eq!(reconstruct_abstract(&serde_json::Value::Null), None);
        assert_eq!(reconstruct_abstract(&serde_json::json!({})), None);
    }

    #[test]
    fn doi_and_openalex_prefixes_are_stripped() {
        assert_eq!(
            strip_doi_prefix("https://doi.org/10.1234/ABC"),
            "10.1234/abc"
        );
        assert_eq!(strip_openalex_prefix("https://openalex.org/W999"), "W999");
    }

    // ---- build_search_url branch coverage --------------------------------

    fn param(u: &Url, key: &str) -> Option<String> {
        u.query_pairs()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
    }

    #[test]
    fn per_page_clamps_to_floor_and_ceiling() {
        let base = Url::parse("https://api.openalex.org").expect("base");
        let mut q = PaperSearchQuery::new("x");
        q.limit = 0;
        let u = build_search_url(&base, "", &q, &ResolvedIds::default()).expect("url");
        assert_eq!(param(&u, "per-page").as_deref(), Some("1"), "limit 0 -> 1");
        q.limit = 201;
        let u = build_search_url(&base, "", &q, &ResolvedIds::default()).expect("url");
        assert_eq!(
            param(&u, "per-page").as_deref(),
            Some("200"),
            "limit 201 -> 200"
        );
    }

    #[test]
    fn to_year_and_recent_sort_land_on_url() {
        let base = Url::parse("https://api.openalex.org").expect("base");
        let mut q = PaperSearchQuery::new("x");
        q.to_year = Some(2023);
        q.sort = SearchSort::Recent;
        let u = build_search_url(&base, "", &q, &ResolvedIds::default()).expect("url");
        assert_eq!(param(&u, "sort").as_deref(), Some("publication_date:desc"));
        assert!(
            param(&u, "filter")
                .unwrap_or_default()
                .contains("to_publication_date:2023-12-31"),
            "to_year must map to to_publication_date:<y>-12-31"
        );
    }

    // ---- select_entity disambiguation boundaries -------------------------

    fn cand(id: &str, name: &str, works: u64, rel: f64) -> Candidate {
        Candidate {
            id: id.to_string(),
            display_name: name.to_string(),
            works_count: works,
            relevance: rel,
        }
    }

    #[test]
    fn dominance_at_exactly_2x_resolves_top() {
        let c = vec![cand("A1", "x", 1, 2.0), cand("A2", "y", 1, 1.0)];
        assert_eq!(select_entity("authors", "q", &c).expect("resolves"), "A1");
    }

    #[test]
    fn dominance_just_below_2x_is_ambiguous() {
        let c = vec![cand("A1", "x", 1, 1.9), cand("A2", "y", 1, 1.0)];
        assert!(matches!(
            select_entity("authors", "q", &c),
            Err(FetchError::Ambiguous { .. })
        ));
    }

    #[test]
    fn zero_relevance_runner_up_is_ambiguous_not_auto_top() {
        // Runner-up with an absent (0.0) relevance must NOT let the top
        // win by default — the dominance guard requires second > 0.0.
        let c = vec![cand("A1", "x", 1, 5.0), cand("A2", "y", 1, 0.0)];
        assert!(matches!(
            select_entity("authors", "q", &c),
            Err(FetchError::Ambiguous { .. })
        ));
    }

    #[test]
    fn multiple_exact_name_matches_are_ambiguous() {
        // Two entities share the exact display name -> ambiguous, even
        // though the first would otherwise dominate on relevance.
        let c = vec![cand("S1", "Dup", 9, 5.0), cand("S2", "Dup", 1, 1.0)];
        assert!(matches!(
            select_entity("sources", "Dup", &c),
            Err(FetchError::Ambiguous { .. })
        ));
    }

    // ---- extract_arxiv_from_location edge cases --------------------------

    #[test]
    fn arxiv_extracted_from_pdf_url_when_landing_absent() {
        let loc = serde_json::json!({ "pdf_url": "https://arxiv.org/abs/2302.00001v3" });
        assert_eq!(
            extract_arxiv_from_location(&loc).as_deref(),
            Some("2302.00001v3")
        );
    }

    #[test]
    fn arxiv_id_stops_at_query_string() {
        let loc =
            serde_json::json!({ "landing_page_url": "https://arxiv.org/abs/2101.12345?utm=x" });
        assert_eq!(
            extract_arxiv_from_location(&loc).as_deref(),
            Some("2101.12345")
        );
    }

    #[test]
    fn truncate_for_hint_is_char_boundary_safe() {
        // 300 multi-byte chars: must not panic on a byte-slice boundary.
        let body = "あ".repeat(300);
        let out = truncate_for_hint(body.as_bytes());
        assert!(out.ends_with('…'));
        assert_eq!(out.chars().filter(|&c| c == 'あ').count(), 200);
    }
}
