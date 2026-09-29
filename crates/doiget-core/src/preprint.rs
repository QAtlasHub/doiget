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
}

impl FoundBy {
    /// Wire token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CrossrefRelation => "crossref_relation",
            Self::OpenAlexLocation => "openalex_location",
            Self::ArxivTitleSearch => "arxiv_title_search",
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
    openalex_enabled: bool,
    ctx: &FetchContext,
) -> Result<Option<Found>, FetchError> {
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
    let title = crossref_message
        .pointer("/title/0")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let family = crossref_message
        .pointer("/author/0/family")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if normalise(title).len() < 12 || family.is_empty() {
        // A short or missing title, or no named first author, is not enough
        // to tell one paper from another.
        return Ok(None);
    }
    match arxiv_search(title, family, ctx).await {
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
    let _permit = ctx.rate_limiter.acquire("openalex").await;
    match ctx.http.fetch_bytes("openalex", url).await {
        Ok((body, _)) => {
            serde_json::from_slice(&body)
                .map(Some)
                .map_err(|e| FetchError::SourceSchema {
                    hint: format!("OpenAlex returned non-JSON: {e}"),
                })
        }
        Err(crate::http::HttpError::HttpStatus { status: 404, .. }) => Ok(None),
        Err(e) => Err(FetchError::Http(e)),
    }
}

async fn arxiv_search(title: &str, family: &str, ctx: &FetchContext) -> Result<String, FetchError> {
    let mut url = base("DOIGET_ARXIV_BASE", "https://export.arxiv.org")?;
    url.set_path("/api/query");
    // arXiv's query language: a quoted phrase for the title, the surname
    // for the author. Quotes inside the title would end the phrase early.
    let phrase: String = title.chars().filter(|c| *c != '"').collect();
    url.query_pairs_mut()
        .append_pair("search_query", &format!("ti:\"{phrase}\" AND au:{family}"))
        .append_pair("max_results", "5");
    let _permit = ctx.rate_limiter.acquire("arxiv").await;
    let (body, _) = ctx.http.fetch_bytes("arxiv", url).await?;
    Ok(String::from_utf8_lossy(&body).into_owned())
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
}
