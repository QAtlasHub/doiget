//! Finding a DOI's arXiv preprint when Unpaywall does not name one (#636,
//! ADR-0062).
//!
//! The #325 fallback fetches an arXiv preprint in place of a blocked or
//! missing publisher copy, but only one Unpaywall reports. A paper can be
//! closed at its publisher, unknown to Unpaywall, and on arXiv all the same
//! (measured on 10.1103/bbnt-brjz, arXiv:2512.07923). Three ways to find it,
//! tried in order, each stopping the search when it answers:
//!
//! 1. **Crossref `relation.has-preprint`** -- in the Crossref record the
//!    fetch already holds, so it costs no request. An `arxiv` id, or an
//!    arXiv DOI (`10.48550/arXiv.<id>`).
//! 2. **OpenAlex `locations[]`** -- a location whose landing page is
//!    `arxiv.org/abs/<id>`. One request to `api.openalex.org`, and only when
//!    the user enabled OpenAlex (`DOIGET_ENABLE_OPENALEX`), so a default
//!    fetch contacts no host it did not before.
//! 3. **arXiv search by title and first author** -- one request to arXiv's
//!    API, at arXiv's own rate. A hit counts only when its title is the
//!    record's title (compared letters and digits only, case-folded), its
//!    authors include the first author's surname, and it does not name a
//!    different published DOI.
//!
//! Each answer says which of the three found it; nothing here fetches.

use serde_json::Value;
use url::Url;

use crate::source::{FetchContext, FetchError};
use crate::{ArxivId, Doi};

/// Which method found a preprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoundBy {
    /// Crossref's `relation.has-preprint`.
    CrossrefRelation,
    /// An OpenAlex location on arXiv.
    OpenAlexLocation,
    /// arXiv's search, by title and first author.
    ArxivTitleSearch,
    /// bioRxiv / medRxiv's `pubs` endpoint (#640).
    BiorxivPubs,
    /// INSPIRE-HEP's `arxiv_eprints` for the DOI (#642).
    Inspire,
}

impl FoundBy {
    /// Wire token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CrossrefRelation => "crossref_relation",
            Self::OpenAlexLocation => "openalex_location",
            Self::ArxivTitleSearch => "arxiv_title_search",
            Self::BiorxivPubs => "biorxiv_pubs",
            Self::Inspire => "inspire",
        }
    }
}

/// A preprint found for a DOI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// The arXiv id, version suffix removed.
    pub arxiv_id: ArxivId,
    /// How it was found.
    pub found_by: FoundBy,
}

/// The arXiv id Crossref's `relation.has-preprint` names, if any.
#[must_use]
pub fn from_crossref(crossref_message: &Value) -> Option<ArxivId> {
    let rels = crossref_message
        .pointer("/relation/has-preprint")?
        .as_array()?;
    rels.iter().find_map(|r| {
        let id = r.get("id").and_then(Value::as_str)?.trim();
        match r.get("id-type").and_then(Value::as_str)? {
            "arxiv" => arxiv_id(id),
            "doi" => {
                let lower = id.to_ascii_lowercase();
                lower
                    .strip_prefix("10.48550/arxiv.")
                    .and_then(|_| arxiv_id(&id["10.48550/arxiv.".len()..]))
            }
            _ => None,
        }
    })
}

/// A non-arXiv preprint found for a DOI (#640): its own DOI, fetched through
/// the ordinary OA route (the location Unpaywall reports for it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundDoi {
    /// The preprint's DOI.
    pub doi: Doi,
    /// The platform, when the finder named one (`bioRxiv`, `medRxiv`).
    pub platform: Option<String>,
    /// How it was found.
    pub found_by: FoundBy,
}

/// A non-arXiv preprint DOI Crossref's `relation.has-preprint` names: any
/// DOI but an arXiv one (`10.48550`), which [`from_crossref`] handles.
#[must_use]
pub fn preprint_doi_from_crossref(crossref_message: &Value) -> Option<Doi> {
    let rels = crossref_message
        .pointer("/relation/has-preprint")?
        .as_array()?;
    rels.iter().find_map(|r| {
        (r.get("id-type").and_then(Value::as_str)? == "doi")
            .then(|| r.get("id").and_then(Value::as_str))
            .flatten()
            .map(str::trim)
            .filter(|id| !id.to_ascii_lowercase().starts_with("10.48550/"))
            .and_then(|id| Doi::parse(id).ok())
    })
}

/// The preprint DOI a bioRxiv / medRxiv `pubs` answer names.
fn from_pubs(answer: &Value) -> Option<(Doi, Option<String>)> {
    let first = answer.get("collection")?.as_array()?.first()?;
    let doi = Doi::parse(first.get("preprint_doi")?.as_str()?.trim()).ok()?;
    let platform = first
        .get("preprint_platform")
        .and_then(Value::as_str)
        .map(str::to_string);
    Some((doi, platform))
}

/// Look for a non-arXiv preprint of `doi` (#640): Crossref's relation, then
/// bioRxiv and medRxiv `pubs` when `biorxiv_enabled`.
///
/// # Errors
///
/// A provenance-log failure only; a request that fails is a finder that
/// found nothing.
pub async fn find_preprint_doi(
    doi: &Doi,
    crossref_message: &Value,
    biorxiv_enabled: bool,
    ctx: &FetchContext,
) -> Result<Option<FoundDoi>, FetchError> {
    if let Some(found) = preprint_doi_from_crossref(crossref_message) {
        return Ok(Some(FoundDoi {
            doi: found,
            platform: None,
            found_by: FoundBy::CrossrefRelation,
        }));
    }
    if !biorxiv_enabled {
        return Ok(None);
    }
    for server in ["biorxiv", "medrxiv"] {
        let mut url = base("DOIGET_BIORXIV_BASE", "https://api.biorxiv.org")?;
        url.set_path(&format!("/pubs/{server}/{}/na/json", doi.as_str()));
        match logged_get(doi, "biorxiv", url, ctx).await {
            Ok(Some(body)) => {
                if let Some((found, platform)) = serde_json::from_slice::<Value>(&body)
                    .ok()
                    .as_ref()
                    .and_then(from_pubs)
                {
                    return Ok(Some(FoundDoi {
                        doi: found,
                        platform,
                        found_by: FoundBy::BiorxivPubs,
                    }));
                }
            }
            Ok(None) => {}
            Err(FetchError::Log(e)) => return Err(FetchError::Log(e)),
            Err(e) => {
                tracing::info!(error = %e, server, "preprint lookup: bioRxiv pubs did not answer")
            }
        }
    }
    Ok(None)
}

/// The arXiv id of an OpenAlex work's arXiv location, if any.
#[must_use]
pub fn from_openalex_work(work: &Value) -> Option<ArxivId> {
    work.get("locations")?.as_array()?.iter().find_map(|l| {
        let url = l.get("landing_page_url").and_then(Value::as_str)?;
        let rest = url
            .strip_prefix("http://arxiv.org/abs/")
            .or_else(|| url.strip_prefix("https://arxiv.org/abs/"))?;
        arxiv_id(rest)
    })
}

/// The arXiv id INSPIRE-HEP's record gives (`metadata.arxiv_eprints`), if
/// any. Nothing else in the record is read (#642): its `documents` are files
/// whose provenance and licence are not stated per file.
#[must_use]
pub fn from_inspire_record(record: &Value) -> Option<ArxivId> {
    record
        .pointer("/metadata/arxiv_eprints")?
        .as_array()?
        .iter()
        .find_map(|e| e.get("value").and_then(Value::as_str).and_then(arxiv_id))
}

/// One arXiv search hit.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Hit {
    id: String,
    title: String,
    authors: Vec<String>,
    doi: Option<String>,
}

/// The first hit in an arXiv search feed that is this paper.
fn match_search(feed: &str, title: &str, first_author_family: &str, doi: &Doi) -> Option<ArxivId> {
    let want = normalise(title);
    let family = first_author_family.to_lowercase();
    parse_feed(feed).into_iter().find_map(|h| {
        let same_title = normalise(&h.title) == want;
        let has_author = h
            .authors
            .iter()
            .any(|a| a.to_lowercase().split_whitespace().any(|w| w == family));
        let doi_agrees = h
            .doi
            .as_deref()
            .is_none_or(|d| d.eq_ignore_ascii_case(doi.as_str()));
        (same_title && has_author && doi_agrees)
            .then(|| h.id.rsplit("/abs/").next().and_then(arxiv_id))
            .flatten()
    })
}

/// Entries of an arXiv Atom feed: id, title, author names, `arxiv:doi`.
fn parse_feed(feed: &str) -> Vec<Hit> {
    feed.split("<entry>")
        .skip(1)
        .map(|e| {
            let e = e.split("</entry>").next().unwrap_or(e);
            Hit {
                id: tag(e, "id").unwrap_or_default(),
                title: tag(e, "title").unwrap_or_default(),
                authors: e
                    .split("<name>")
                    .skip(1)
                    .filter_map(|n| n.split("</name>").next())
                    .map(unescape)
                    .collect(),
                doi: e
                    .split("<arxiv:doi")
                    .nth(1)
                    .and_then(|d| d.split_once('>'))
                    .and_then(|(_, rest)| rest.split("</arxiv:doi>").next())
                    .map(unescape),
            }
        })
        .collect()
}

fn tag(e: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = e.find(&open)? + open.len();
    let end = e[start..].find(&close)? + start;
    Some(unescape(&e[start..end]))
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
        .trim()
        .to_string()
}

/// Letters and digits only, case-folded: what two renderings of a title
/// share once punctuation, markup and line breaks are set aside.
fn normalise(title: &str) -> String {
    crate::markup::plain_title(title)
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Whether a title can identify a paper in a search: at least 20 letters
/// and digits (counted as characters, not bytes -- a CJK title is three
/// bytes a character) across at least three words. "Introduction" and
/// "Editorial" are not.
fn distinctive(title: &str) -> bool {
    let words = crate::markup::plain_title(title)
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .count();
    normalise(title).chars().count() >= 20 && words >= 3
}

/// An arXiv id without its version suffix.
fn arxiv_id(raw: &str) -> Option<ArxivId> {
    let raw = raw.trim().trim_end_matches('/');
    let unversioned = match raw.rsplit_once('v') {
        Some((head, tail)) if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) => head,
        _ => raw,
    };
    ArxivId::parse(unversioned).ok()
}

/// Look for a preprint of `doi`: Crossref's relation, then OpenAlex (when
/// `openalex_enabled`), then arXiv's search. `crossref_message` is the
/// record the fetch already holds.
///
/// # Errors
///
/// A provenance-log failure only; a request that fails is a method that
/// found nothing, and the next is tried.
pub async fn find(
    doi: &Doi,
    crossref_message: &Value,
    enabled: &crate::MetadataAccess,
    ctx: &FetchContext,
) -> Result<Option<Found>, FetchError> {
    let openalex_enabled = enabled.openalex;
    if let Some(arxiv_id) = from_crossref(crossref_message) {
        return Ok(Some(Found {
            arxiv_id,
            found_by: FoundBy::CrossrefRelation,
        }));
    }
    if openalex_enabled {
        match openalex_work(doi, ctx).await {
            Ok(Some(work)) => {
                if let Some(arxiv_id) = from_openalex_work(&work) {
                    return Ok(Some(Found {
                        arxiv_id,
                        found_by: FoundBy::OpenAlexLocation,
                    }));
                }
            }
            Ok(None) => {}
            Err(FetchError::Log(e)) => return Err(FetchError::Log(e)),
            Err(e) => tracing::info!(error = %e, "preprint lookup: OpenAlex did not answer"),
        }
    }
    if enabled.inspire {
        match inspire_record(doi, ctx).await {
            Ok(Some(record)) => {
                if let Some(arxiv_id) = from_inspire_record(&record) {
                    return Ok(Some(Found {
                        arxiv_id,
                        found_by: FoundBy::Inspire,
                    }));
                }
            }
            Ok(None) => {}
            Err(FetchError::Log(e)) => return Err(FetchError::Log(e)),
            Err(e) => tracing::info!(error = %e, "preprint lookup: INSPIRE did not answer"),
        }
    }
    let title = crossref_message
        .pointer("/title/0")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let family = crossref_message
        .pointer("/author/0/family")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !distinctive(title) || family.is_empty() {
        // A short or generic title, or no named first author, is not enough
        // to tell one paper from another.
        return Ok(None);
    }
    match arxiv_search(doi, title, family, ctx).await {
        Ok(feed) => Ok(
            match_search(&feed, title, family, doi).map(|arxiv_id| Found {
                arxiv_id,
                found_by: FoundBy::ArxivTitleSearch,
            }),
        ),
        Err(FetchError::Log(e)) => Err(FetchError::Log(e)),
        Err(e) => {
            tracing::info!(error = %e, "preprint lookup: arXiv search did not answer");
            Ok(None)
        }
    }
}

fn base(env: &str, default: &str) -> Result<Url, FetchError> {
    let raw = std::env::var(env).unwrap_or_else(|_| default.to_string());
    Url::parse(&raw).map_err(|e| FetchError::SourceSchema {
        hint: format!("{env}={raw:?} is not a URL: {e}"),
    })
}

async fn openalex_work(doi: &Doi, ctx: &FetchContext) -> Result<Option<Value>, FetchError> {
    let mut url = base("DOIGET_OPENALEX_BASE", "https://api.openalex.org")?;
    url.set_path(&format!("/works/doi:{}", doi.as_str()));
    if let Some(email) = crate::orchestrator::configured_contact_email() {
        url.query_pairs_mut().append_pair("mailto", &email);
    }
    let Some(body) = logged_get(doi, "openalex", url, ctx).await? else {
        return Ok(None);
    };
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| FetchError::SourceSchema {
            hint: format!("OpenAlex returned non-JSON: {e}"),
        })
}

async fn inspire_record(doi: &Doi, ctx: &FetchContext) -> Result<Option<Value>, FetchError> {
    let mut url = base("DOIGET_INSPIRE_BASE", "https://inspirehep.net")?;
    url.set_path(&format!("/api/doi/{}", doi.as_str()));
    let Some(body) = logged_get(doi, "inspire", url, ctx).await? else {
        return Ok(None);
    };
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| FetchError::SourceSchema {
            hint: format!("INSPIRE returned non-JSON: {e}"),
        })
}

async fn arxiv_search(
    doi: &Doi,
    title: &str,
    family: &str,
    ctx: &FetchContext,
) -> Result<String, FetchError> {
    let mut url = base("DOIGET_ARXIV_BASE", "https://export.arxiv.org")?;
    url.set_path("/api/query");
    // arXiv's query language: a quoted phrase for the title, the surname
    // for the author. Quotes inside the title would end the phrase early.
    let phrase: String = title.chars().filter(|c| *c != '"').collect();
    url.query_pairs_mut()
        .append_pair("search_query", &format!("ti:\"{phrase}\" AND au:{family}"))
        .append_pair("max_results", "5");
    Ok(logged_get(doi, "arxiv", url, ctx)
        .await?
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default())
}

/// One rate-limited request, recorded in the provenance log as a `resolve`
/// row for `doi` under `source` -- like every other request a fetch makes
/// (ADR-0006). `Ok(None)` for a 404.
async fn logged_get(
    doi: &Doi,
    source: &'static str,
    url: Url,
    ctx: &FetchContext,
) -> Result<Option<bytes::Bytes>, FetchError> {
    use crate::provenance::{Capability, LogEvent, LogResult, RowInput};
    let _permit = ctx.rate_limiter.acquire(source).await;
    let digest = crate::Ref::Doi(doi.clone())
        .promote(source, None)
        .digest_hex();
    let row = |result, size, error_code| RowInput {
        event: LogEvent::Resolve,
        result,
        capability: Capability::Metadata,
        ref_: Some(doi.as_str()),
        source: Some(source),
        error_code,
        size_bytes: size,
        license: None,
        store_path: None,
        canonical_digest: Some(&digest),
    };
    match ctx.http.fetch_bytes(source, url).await {
        Ok((body, _)) => {
            ctx.log
                .append(row(LogResult::Ok, Some(body.len() as u64), None))?;
            Ok(Some(body))
        }
        Err(crate::http::HttpError::HttpStatus { status: 404, .. }) => {
            ctx.log
                .append(row(LogResult::Err, None, Some("NOT_FOUND")))?;
            Ok(None)
        }
        Err(e) => {
            let e = FetchError::Http(e);
            let code = crate::ErrorCode::from(&e);
            ctx.log
                .append(row(LogResult::Err, None, Some(code.as_wire())))?;
            Err(e)
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    /// Shapes from live Crossref records (2026-09-29): both forms occur.
    #[test]
    fn crossref_has_preprint_names_an_arxiv_id_or_an_arxiv_doi() {
        let by_id = serde_json::json!({"relation": {"has-preprint": [
            {"id-type": "arxiv", "id": "2302.04668v2", "asserted-by": "subject"}]}});
        assert_eq!(from_crossref(&by_id).unwrap().as_str(), "2302.04668");
        let by_doi = serde_json::json!({"relation": {"has-preprint": [
            {"id-type": "doi", "id": "10.1101/2020.01.01.123456"},
            {"id-type": "doi", "id": "10.48550/arXiv.2512.07923"}]}});
        assert_eq!(from_crossref(&by_doi).unwrap().as_str(), "2512.07923");
        assert!(from_crossref(&serde_json::json!({"relation": {}})).is_none());
    }

    /// Shape from a live OpenAlex work (10.1038/s41586-019-1666-5).
    #[test]
    fn an_openalex_arxiv_location_gives_its_id() {
        let work = serde_json::json!({"locations": [
            {"landing_page_url": "https://doi.org/10.1038/s41586-019-1666-5"},
            {"landing_page_url": "http://arxiv.org/abs/1910.11333", "version": "publishedVersion"}]});
        assert_eq!(from_openalex_work(&work).unwrap().as_str(), "1910.11333");
        assert!(from_openalex_work(&serde_json::json!({"locations": []})).is_none());
    }

    const FEED: &str = r#"<feed><entry>
<id>http://arxiv.org/abs/2512.07923v1</id>
<title>Environment-matrix-product operator for boundary-free large-scale
  quantum many-body simulations</title>
<author><name>Souta Shimozono</name></author><author><name>Chisa Hotta</name></author>
</entry><entry>
<id>http://arxiv.org/abs/2401.00001v2</id>
<title>Something else entirely</title>
<author><name>Souta Shimozono</name></author>
<arxiv:doi xmlns:arxiv="http://arxiv.org/schemas/atom">10.1103/other</arxiv:doi>
</entry></feed>"#;

    /// Measured on the maintainer's own paper, 2026-09-29.
    #[test]
    fn a_search_hit_counts_on_the_same_title_and_first_author() {
        let doi = Doi::parse("10.1103/bbnt-brjz").unwrap();
        let title = "Environment-matrix-product operator for boundary-free large-scale quantum many-body simulations";
        assert_eq!(
            match_search(FEED, title, "Shimozono", &doi)
                .unwrap()
                .as_str(),
            "2512.07923"
        );
        assert!(
            match_search(FEED, title, "Hotta", &doi).is_some(),
            "any listed author"
        );
        assert!(
            match_search(FEED, title, "White", &doi).is_none(),
            "wrong author"
        );
        assert!(match_search(FEED, "A different title", "Shimozono", &doi).is_none());
    }

    #[test]
    fn a_hit_naming_a_different_published_doi_is_not_this_paper() {
        let feed = FEED.replace("Something else entirely", "Same Title Here Exactly");
        let doi = Doi::parse("10.1103/bbnt-brjz").unwrap();
        assert!(match_search(&feed, "Same title here, exactly", "Shimozono", &doi).is_none());
        let other = Doi::parse("10.1103/other").unwrap();
        assert_eq!(
            match_search(&feed, "Same title here, exactly", "Shimozono", &other)
                .unwrap()
                .as_str(),
            "2401.00001"
        );
    }

    #[test]
    fn a_generic_or_short_title_is_not_distinctive_counting_characters() {
        assert!(!distinctive("Introduction"));
        assert!(!distinctive("Editorial comment"));
        // Four CJK characters are twelve bytes; they are four characters.
        assert!(!distinctive("量子多体系"));
        assert!(distinctive(
            "Environment-matrix-product operator for boundary-free simulations"
        ));
    }

    mod live_shape {
        //! `find` through the real HTTP client and log, against mocks.
        use super::super::*;
        use std::sync::Arc;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        const TITLE: &str =
            "Environment-matrix-product operator for boundary-free large-scale quantum many-body simulations";

        async fn ctx(server: &MockServer) -> (FetchContext, tempfile::TempDir) {
            let host = server.address().to_string();
            let td = tempfile::TempDir::new().expect("tempdir");
            let log = camino::Utf8PathBuf::try_from(td.path().join("log.jsonl")).expect("utf-8");
            std::env::set_var("DOIGET_OPENALEX_BASE", server.uri());
            std::env::set_var("DOIGET_ARXIV_BASE", server.uri());
            std::env::set_var("DOIGET_INSPIRE_BASE", server.uri());
            let sid = "01J0000000000000000000PP62".to_string();
            (
                FetchContext {
                    http: Arc::new(crate::http::HttpClient::new_for_tests_allow_http_multi(&[
                        ("openalex", host.as_str()),
                        ("arxiv", host.as_str()),
                        ("inspire", host.as_str()),
                    ])),
                    rate_limiter: Arc::new(crate::rate_limiter::RateLimiter::new(
                        crate::RateLimits::HARD_CODED,
                    )),
                    log: Arc::new(
                        crate::provenance::ProvenanceLog::open(log, sid.clone()).expect("log"),
                    ),
                    session_id: sid,
                    cache_root: None,
                },
                td,
            )
        }

        fn clear() {
            std::env::remove_var("DOIGET_OPENALEX_BASE");
            std::env::remove_var("DOIGET_ARXIV_BASE");
            std::env::remove_var("DOIGET_INSPIRE_BASE");
        }

        async fn server() -> MockServer {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/works/doi:10.1103/bbnt-brjz"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "locations": [{"landing_page_url": "http://arxiv.org/abs/2512.07923v1"}]
                })))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/api/query"))
                .respond_with(ResponseTemplate::new(200).set_body_string(format!(
                    "<feed><entry><id>http://arxiv.org/abs/2512.07923v1</id><title>{TITLE}</title>\
                     <author><name>Souta Shimozono</name></author></entry></feed>"
                )))
                .mount(&server)
                .await;
            server
        }

        fn record(title: &str) -> Value {
            serde_json::json!({"title": [title], "author": [{"family": "Shimozono"}]})
        }

        async fn paths(server: &MockServer) -> Vec<String> {
            server
                .received_requests()
                .await
                .unwrap_or_default()
                .iter()
                .map(|r| r.url.path().to_string())
                .collect()
        }

        #[tokio::test]
        #[serial_test::serial]
        async fn an_enabled_openalex_answers_before_any_arxiv_search() {
            let server = server().await;
            let (ctx, _td) = ctx(&server).await;
            let doi = Doi::parse("10.1103/bbnt-brjz").unwrap();
            let found = find(
                &doi,
                &record(TITLE),
                &crate::MetadataAccess {
                    openalex: true,
                    ..Default::default()
                },
                &ctx,
            )
            .await
            .unwrap()
            .unwrap();
            let seen = paths(&server).await;
            let log = std::fs::read_to_string(ctx.log.path()).unwrap();
            clear();
            assert_eq!(found.found_by, FoundBy::OpenAlexLocation);
            assert_eq!(found.arxiv_id.as_str(), "2512.07923");
            assert!(!seen.iter().any(|p| p == "/api/query"), "{seen:?}");
            assert!(log.contains("\"source\":\"openalex\""), "logged: {log}");
        }

        /// #642: an enabled INSPIRE answers from `arxiv_eprints`, before
        /// any arXiv search.
        #[tokio::test]
        #[serial_test::serial]
        async fn an_enabled_inspire_answers_before_the_arxiv_search() {
            let server = server().await;
            Mock::given(method("GET"))
                .and(path("/api/doi/10.1103/bbnt-brjz"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "metadata": {"arxiv_eprints": [{"value": "2512.07923", "categories": ["cond-mat.str-el"]}]}
                })))
                .mount(&server)
                .await;
            let (ctx, _td) = ctx(&server).await;
            let doi = Doi::parse("10.1103/bbnt-brjz").unwrap();
            let enabled = crate::MetadataAccess {
                inspire: true,
                ..Default::default()
            };
            let found = find(&doi, &record(TITLE), &enabled, &ctx)
                .await
                .unwrap()
                .unwrap();
            let seen = paths(&server).await;
            clear();
            assert_eq!(found.found_by, FoundBy::Inspire);
            assert_eq!(found.arxiv_id.as_str(), "2512.07923");
            assert!(!seen.iter().any(|p| p == "/api/query"), "{seen:?}");
            assert!(
                !seen.iter().any(|p| p.starts_with("/works/")),
                "OpenAlex off: {seen:?}"
            );
        }

        #[tokio::test]
        #[serial_test::serial]
        async fn a_disabled_openalex_is_not_asked_and_the_search_answers() {
            let server = server().await;
            let (ctx, _td) = ctx(&server).await;
            let doi = Doi::parse("10.1103/bbnt-brjz").unwrap();
            let found = find(
                &doi,
                &record(TITLE),
                &crate::MetadataAccess::default(),
                &ctx,
            )
            .await
            .unwrap()
            .unwrap();
            let seen = paths(&server).await;
            let log = std::fs::read_to_string(ctx.log.path()).unwrap();
            clear();
            assert_eq!(found.found_by, FoundBy::ArxivTitleSearch);
            assert!(!seen.iter().any(|p| p.starts_with("/works/")), "{seen:?}");
            assert!(log.contains("\"source\":\"arxiv\""), "logged: {log}");
        }

        /// #640: `pubs` is asked only when enabled -- bioRxiv first, then
        /// medRxiv -- and its preprint DOI is the answer.
        #[tokio::test]
        #[serial_test::serial]
        async fn biorxiv_pubs_is_asked_only_when_enabled_and_names_the_preprint() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/pubs/biorxiv/10.1371/journal.pone.0256482/na/json"))
                .respond_with(ResponseTemplate::new(200).set_body_json(
                    serde_json::json!({"messages": [{"status": "no posts found"}], "collection": []}),
                ))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/pubs/medrxiv/10.1371/journal.pone.0256482/na/json"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "collection": [{"preprint_doi": "10.1101/2021.04.29.21256344",
                                    "preprint_platform": "medRxiv"}]
                })))
                .mount(&server)
                .await;
            let host = server.address().to_string();
            let td = tempfile::TempDir::new().expect("tempdir");
            let log = camino::Utf8PathBuf::try_from(td.path().join("log.jsonl")).expect("utf-8");
            std::env::set_var("DOIGET_BIORXIV_BASE", server.uri());
            let sid = "01J0000000000000000000BX40".to_string();
            let ctx = FetchContext {
                http: Arc::new(crate::http::HttpClient::new_for_tests_allow_http_multi(&[
                    ("biorxiv", host.as_str()),
                ])),
                rate_limiter: Arc::new(crate::rate_limiter::RateLimiter::new(
                    crate::RateLimits::HARD_CODED,
                )),
                log: Arc::new(
                    crate::provenance::ProvenanceLog::open(log, sid.clone()).expect("log"),
                ),
                session_id: sid,
                cache_root: None,
            };
            let doi = Doi::parse("10.1371/journal.pone.0256482").unwrap();
            let off = find_preprint_doi(&doi, &serde_json::json!({}), false, &ctx)
                .await
                .unwrap();
            let asked_when_off = paths(&server).await;
            let on = find_preprint_doi(&doi, &serde_json::json!({}), true, &ctx)
                .await
                .unwrap()
                .unwrap();
            std::env::remove_var("DOIGET_BIORXIV_BASE");
            assert!(off.is_none());
            assert!(asked_when_off.is_empty(), "{asked_when_off:?}");
            assert_eq!(on.doi.as_str(), "10.1101/2021.04.29.21256344");
            assert_eq!(on.platform.as_deref(), Some("medRxiv"));
            assert_eq!(on.found_by, FoundBy::BiorxivPubs);
        }

        #[tokio::test]
        #[serial_test::serial]
        async fn a_generic_title_is_not_searched_at_all() {
            let server = server().await;
            let (ctx, _td) = ctx(&server).await;
            let doi = Doi::parse("10.1103/bbnt-brjz").unwrap();
            let found = find(
                &doi,
                &record("Introduction"),
                &crate::MetadataAccess::default(),
                &ctx,
            )
            .await
            .unwrap();
            let seen = paths(&server).await;
            clear();
            assert!(found.is_none());
            assert!(seen.is_empty(), "{seen:?}");
        }
    }

    /// #642: the shape of a live INSPIRE record (10.1103/PhysRevLett.116.061102).
    #[test]
    fn an_inspire_record_gives_its_arxiv_eprint() {
        let rec = serde_json::json!({"metadata": {
            "arxiv_eprints": [{"value": "1602.03837", "categories": ["gr-qc"]}],
            "documents": [{"url": "https://inspirehep.net/files/4d19c13c"}]}});
        assert_eq!(from_inspire_record(&rec).unwrap().as_str(), "1602.03837");
        assert!(from_inspire_record(&serde_json::json!({"metadata": {}})).is_none());
    }

    /// #640: shapes from a live Crossref sample and a live `pubs` answer.
    #[test]
    fn a_non_arxiv_preprint_doi_is_read_from_crossref_or_pubs() {
        let rel = serde_json::json!({"relation": {"has-preprint": [
            {"id-type": "doi", "id": "10.48550/arXiv.2512.07923"},
            {"id-type": "doi", "id": "10.1101/2021.04.29.21256344"}]}});
        assert_eq!(
            preprint_doi_from_crossref(&rel).unwrap().as_str(),
            "10.1101/2021.04.29.21256344",
            "the arXiv DOI is from_crossref's; this is the other one"
        );
        let arxiv_only = serde_json::json!({"relation": {"has-preprint": [
            {"id-type": "arxiv", "id": "2302.04668v2"}]}});
        assert!(preprint_doi_from_crossref(&arxiv_only).is_none());
        let pubs = serde_json::json!({"messages": [{"status": "ok"}], "collection": [{
            "preprint_doi": "10.1101/2021.04.29.21256344",
            "published_doi": "10.1371/journal.pone.0256482",
            "preprint_platform": "medRxiv"}]});
        let (found, platform) = from_pubs(&pubs).unwrap();
        assert_eq!(found.as_str(), "10.1101/2021.04.29.21256344");
        assert_eq!(platform.as_deref(), Some("medRxiv"));
        assert!(from_pubs(&serde_json::json!({"collection": []})).is_none());
    }
}
