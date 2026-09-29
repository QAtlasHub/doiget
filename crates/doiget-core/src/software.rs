//! Software citations (#614, ADR-0058): a GitHub release or tag cited as
//! `@software`, and which Zenodo DOI a software record is cited by.
//!
//! Papers cite their code, often as a GitHub release with no DOI
//! (`https://github.com/srwhite59/HFDMRG.jl/releases/tag/v0.1.0`). `cite`
//! took DOIs and arXiv ids only, so those entries were written by hand.
//!
//! What is read, and from where -- only when the caller names a GitHub URL:
//!
//! - `api.github.com` `GET /repos/{owner}/{repo}` for the repository, then
//!   `/releases/tags/{tag}` (or `/releases/latest` for a bare repository
//!   URL). A tag with no release falls back to the tag's commit date
//!   (`/commits/{tag}`).
//! - `raw.githubusercontent.com` `/{owner}/{repo}/{tag or HEAD}/CITATION.cff`,
//!   the authors' own statement of how to cite them. Its authors and title
//!   win over the repository's owner and name.
//!
//! `CITATION.cff` is YAML. Only its top-level scalars and the top-level
//! `authors` list are read ([`parse_cff`]), by a line reader rather than a
//! YAML crate: those are the fields a citation needs, CFF writers emit them
//! in plain block style, and anything the reader cannot follow is skipped,
//! never guessed -- an unreadable file cites as if it were absent.

use serde_json::Value;
use url::Url;

use crate::provenance::{Capability, LogEvent, LogResult, RowInput};
use crate::source::{FetchContext, FetchError};
use crate::store::Metadata;

/// HTTP source key for the GitHub REST API.
pub const GITHUB_API: &str = "github";
/// HTTP source key for `raw.githubusercontent.com`.
pub const GITHUB_RAW: &str = "github-raw";
/// Base-URL override for [`GITHUB_API`].
pub const GITHUB_API_BASE_ENV: &str = "DOIGET_GITHUB_API_BASE";
/// Base-URL override for [`GITHUB_RAW`].
pub const GITHUB_RAW_BASE_ENV: &str = "DOIGET_GITHUB_RAW_BASE";

const API_DEFAULT: &str = "https://api.github.com";
const RAW_DEFAULT: &str = "https://raw.githubusercontent.com";

/// A GitHub repository, optionally at a tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubRef {
    /// Repository owner.
    pub owner: String,
    /// Repository name.
    pub repo: String,
    /// The release tag, when the URL named one.
    pub tag: Option<String>,
}

impl GithubRef {
    /// Parse `https://github.com/{owner}/{repo}`, optionally followed by
    /// `/releases/tag/{tag}` or `/tree/{tag}`. `None` for anything else,
    /// including other hosts and non-repository GitHub pages.
    #[must_use]
    pub fn parse(input: &str) -> Option<Self> {
        let url = Url::parse(input.trim()).ok()?;
        if !matches!(url.scheme(), "https" | "http")
            || !matches!(url.host_str(), Some("github.com" | "www.github.com"))
        {
            return None;
        }
        let segs: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
        let (owner, repo) = match segs.as_slice() {
            [o, r, ..] => (*o, r.trim_end_matches(".git")),
            _ => return None,
        };
        let valid = |s: &str| {
            !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        };
        if !valid(owner) || !valid(repo) {
            return None;
        }
        let tag = match &segs[2..] {
            [] => None,
            ["releases", "tag", tag @ ..] | ["tree", tag @ ..] if !tag.is_empty() => {
                Some(tag.join("/"))
            }
            _ => return None,
        };
        Some(Self {
            owner: owner.to_string(),
            repo: repo.to_string(),
            tag,
        })
    }

    /// The page a reader follows: the release when there is a tag, else the
    /// repository.
    #[must_use]
    pub fn html_url(&self) -> String {
        match &self.tag {
            Some(t) => format!(
                "https://github.com/{}/{}/releases/tag/{t}",
                self.owner, self.repo
            ),
            None => format!("https://github.com/{}/{}", self.owner, self.repo),
        }
    }

    /// A citation key when no template or `--key` gives one:
    /// `{repo}_{tag}` reduced to what every BibTeX processor accepts.
    #[must_use]
    pub fn default_key(&self) -> String {
        let raw = match &self.tag {
            Some(t) => format!("{}_{t}", self.repo),
            None => self.repo.clone(),
        };
        raw.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect()
    }
}

/// What `CITATION.cff` says, as far as a citation needs it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cff {
    /// `title`.
    pub title: Option<String>,
    /// `authors`, each as `Given Family` or an entity `name`.
    pub authors: Vec<String>,
    /// `version`.
    pub version: Option<String>,
    /// `doi` -- usually the Zenodo concept DOI.
    pub doi: Option<String>,
    /// `date-released`, `YYYY-MM-DD`.
    pub date_released: Option<String>,
}

/// Read the citation fields of a `CITATION.cff`: top-level `title`,
/// `version`, `doi`, `date-released`, and the top-level `authors` list.
/// Nested blocks (`preferred-citation`, `identifiers`, `references`) are
/// skipped, so their own `authors` and `title` are never mistaken for the
/// software's.
#[must_use]
pub fn parse_cff(text: &str) -> Cff {
    let mut cff = Cff::default();
    let mut in_authors = false;
    let mut current: Option<AuthorParts> = None;
    for raw in text.lines() {
        let line = strip_comment(raw);
        if line.trim().is_empty() {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if indent == 0 {
            if let Some(a) = current.take() {
                cff.authors.extend(a.render());
            }
            in_authors = false;
            let Some((key, value)) = split_key(line) else {
                continue;
            };
            match key {
                "authors" => in_authors = value.is_empty(),
                "title" => cff.title = scalar(value),
                "version" => cff.version = scalar(value),
                "doi" => cff.doi = scalar(value),
                "date-released" => cff.date_released = scalar(value),
                _ => {}
            }
            continue;
        }
        if !in_authors {
            continue;
        }
        let body = line.trim_start();
        let item = if let Some(rest) = body.strip_prefix("- ") {
            if let Some(a) = current.take() {
                cff.authors.extend(a.render());
            }
            current = Some(AuthorParts::default());
            rest.trim_start()
        } else {
            body
        };
        if let (Some(a), Some((key, value))) = (current.as_mut(), split_key(item)) {
            let v = scalar(value);
            match key {
                "given-names" => a.given = v,
                "family-names" => a.family = v,
                "name-particle" => a.particle = v,
                "name" => a.name = v,
                _ => {}
            }
        }
    }
    if let Some(a) = current.take() {
        cff.authors.extend(a.render());
    }
    cff
}

#[derive(Debug, Default)]
struct AuthorParts {
    given: Option<String>,
    family: Option<String>,
    particle: Option<String>,
    name: Option<String>,
}

impl AuthorParts {
    fn render(self) -> Option<String> {
        let family = match (self.particle, self.family) {
            (Some(p), Some(f)) => Some(format!("{p} {f}")),
            (None, f) => f,
            (Some(_), None) => None,
        };
        match (self.given, family, self.name) {
            (Some(g), Some(f), _) => Some(format!("{g} {f}")),
            (None, Some(f), _) => Some(f),
            (_, None, Some(n)) => Some(n),
            _ => None,
        }
    }
}

/// `line` without a trailing ` # comment` outside quotes.
fn strip_comment(line: &str) -> &str {
    let mut quote = None;
    let mut prev_space = true;
    for (i, c) in line.char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, '#') if prev_space => return &line[..i],
            _ => {}
        }
        prev_space = c.is_whitespace();
    }
    line
}

/// `key: value` at the start of `line`.
fn split_key(line: &str) -> Option<(&str, &str)> {
    let (key, value) = line.trim().split_once(':')?;
    let key = key.trim();
    (!key.is_empty() && !key.contains(' ')).then(|| (key, value.trim()))
}

/// A YAML scalar with its quotes removed; `None` for an empty value or the
/// start of a block (`|`, `>`), which this reader does not follow.
fn scalar(value: &str) -> Option<String> {
    let v = value.trim();
    if v.is_empty() || v.starts_with(['|', '>', '[', '{']) {
        return None;
    }
    let unquoted = v
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(v);
    Some(unquoted.to_string()).filter(|s| !s.is_empty())
}

/// A software citation resolved from GitHub.
#[derive(Debug, Clone)]
pub struct SoftwareCitation {
    /// The entry, `type_ = "software"`, with `version` in `other`.
    pub metadata: Metadata,
    /// Whether a `CITATION.cff` supplied the authors and title.
    pub from_cff: bool,
    /// Notes for the user: where each field came from when it matters.
    pub notes: Vec<String>,
}

/// Resolve `g` against GitHub: repository, release (or the tag's commit),
/// and `CITATION.cff`.
///
/// # Errors
///
/// [`FetchError`] from the repository lookup -- a 404 is `NOT_FOUND`, a 429
/// is `retry_after`, and a 403 stays `CAPABILITY_DENIED` ([`explain`] says
/// why). A missing release or `CITATION.cff` is not an error.
pub async fn resolve_github(
    g: &GithubRef,
    ctx: &FetchContext,
) -> Result<SoftwareCitation, FetchError> {
    let api = base(GITHUB_API_BASE_ENV, API_DEFAULT)?;
    let raw = base(GITHUB_RAW_BASE_ENV, RAW_DEFAULT)?;
    let repo_path = format!("repos/{}/{}", g.owner, g.repo);
    let repo = api_json(ctx, &api, &repo_path, g)
        .await?
        .ok_or_else(|| not_found(g))?;

    let mut notes = Vec::new();
    let (release, tag) = match &g.tag {
        Some(t) => (
            api_json(ctx, &api, &format!("{repo_path}/releases/tags/{t}"), g).await?,
            Some(t.clone()),
        ),
        None => {
            let latest = api_json(ctx, &api, &format!("{repo_path}/releases/latest"), g).await?;
            let tag = latest
                .as_ref()
                .and_then(|r| r.get("tag_name"))
                .and_then(Value::as_str)
                .map(str::to_string);
            if let Some(t) = &tag {
                notes.push(format!(
                    "no tag in the URL: citing the latest release, {t}; cite a release URL to pin another"
                ));
            }
            (latest, tag)
        }
    };
    let mut date = release
        .as_ref()
        .and_then(|r| r.get("published_at").or_else(|| r.get("created_at")))
        .and_then(Value::as_str)
        .map(str::to_string);
    if release.is_none() {
        if let Some(t) = &tag {
            // A tag with no release object: its commit's date is the date.
            let commit = api_json(ctx, &api, &format!("{repo_path}/commits/{t}"), g).await?;
            date = commit
                .as_ref()
                .and_then(|c| c.pointer("/commit/committer/date"))
                .and_then(Value::as_str)
                .map(str::to_string);
            if commit.is_none() {
                return Err(not_found(g));
            }
            notes.push(format!(
                "{t} is a tag with no GitHub release; its date is the tag's commit"
            ));
        }
    }

    let cff_ref = tag.as_deref().unwrap_or("HEAD");
    let cff_text = raw_text(
        ctx,
        &raw,
        &format!("{}/{}/{cff_ref}/CITATION.cff", g.owner, g.repo),
        g,
    )
    .await?;
    let cff = cff_text.as_deref().map(parse_cff);

    let owner = repo
        .pointer("/owner/login")
        .and_then(Value::as_str)
        .unwrap_or(&g.owner)
        .to_string();
    let name = repo
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(&g.repo)
        .to_string();
    let from_cff = cff.as_ref().is_some_and(|c| !c.authors.is_empty());

    let mut m = Metadata {
        schema_version: "1.0".into(),
        title: cff
            .as_ref()
            .and_then(|c| c.title.clone())
            .unwrap_or_else(|| name.clone()),
        authors: match cff.as_ref() {
            Some(c) if !c.authors.is_empty() => c.authors.clone(),
            _ => vec![owner.clone()],
        },
        year: date
            .as_deref()
            .or_else(|| cff.as_ref().and_then(|c| c.date_released.as_deref()))
            .and_then(year_of),
        url: Some(
            release
                .as_ref()
                .and_then(|r| r.get("html_url"))
                .and_then(Value::as_str)
                .map_or_else(
                    || {
                        GithubRef {
                            tag: tag.clone(),
                            ..g.clone()
                        }
                        .html_url()
                    },
                    str::to_string,
                ),
        ),
        publisher: Some("GitHub".into()),
        type_: Some("software".into()),
        ..Metadata::default()
    };
    let version = tag.or_else(|| cff.as_ref().and_then(|c| c.version.clone()));
    if let Some(v) = version {
        m.other.insert("version".into(), toml::Value::String(v));
    }
    if !from_cff {
        notes.push(format!(
            "no CITATION.cff with authors in {owner}/{name}: the author is the repository owner, {owner}"
        ));
    }
    if let Some(doi) = cff.as_ref().and_then(|c| c.doi.as_deref()) {
        match crate::Doi::parse(doi) {
            Ok(d) => {
                notes.push(format!(
                    "CITATION.cff names DOI {doi}; it is included, and `doiget cite {doi}` cites the archived record itself"
                ));
                m.doi = Some(d);
            }
            Err(_) => notes.push(format!("CITATION.cff's doi {doi:?} is not a DOI; left out")),
        }
    }
    Ok(SoftwareCitation {
        metadata: m,
        from_cff,
        notes,
    })
}

/// Whether `g` still resolves: the repository, and the release or tag the
/// URL names. `Ok(false)` for a 404 at either; transport failures are
/// errors.
///
/// # Errors
///
/// [`FetchError`] for anything other than a clean found / not-found answer.
pub async fn github_resolves(g: &GithubRef, ctx: &FetchContext) -> Result<bool, FetchError> {
    let api = base(GITHUB_API_BASE_ENV, API_DEFAULT)?;
    let repo_path = format!("repos/{}/{}", g.owner, g.repo);
    if api_json(ctx, &api, &repo_path, g).await?.is_none() {
        return Ok(false);
    }
    let Some(t) = &g.tag else {
        return Ok(true);
    };
    if api_json(ctx, &api, &format!("{repo_path}/releases/tags/{t}"), g)
        .await?
        .is_some()
    {
        return Ok(true);
    }
    Ok(api_json(ctx, &api, &format!("{repo_path}/commits/{t}"), g)
        .await?
        .is_some())
}

fn base(env: &str, default: &str) -> Result<Url, FetchError> {
    let raw = std::env::var(env).unwrap_or_else(|_| default.to_string());
    let mut url = Url::parse(&raw).map_err(|e| FetchError::SourceSchema {
        hint: format!("{env}={raw:?} is not a URL: {e}"),
    })?;
    if !url.path().ends_with('/') {
        url.set_path(&format!("{}/", url.path()));
    }
    Ok(url)
}

fn not_found(g: &GithubRef) -> FetchError {
    FetchError::Http(crate::http::HttpError::HttpStatus {
        status: 404,
        url: g.html_url(),
        retry_after_ms: None,
    })
}

/// `GET` a GitHub API path as JSON; `Ok(None)` for a 404.
async fn api_json(
    ctx: &FetchContext,
    api: &Url,
    path: &str,
    g: &GithubRef,
) -> Result<Option<Value>, FetchError> {
    let url = api.join(path).map_err(|e| FetchError::SourceSchema {
        hint: format!("building a GitHub API URL for {path}: {e}"),
    })?;
    let headers = [
        ("Accept", "application/vnd.github+json"),
        ("X-GitHub-Api-Version", "2022-11-28"),
    ];
    let Some(body) = get(ctx, GITHUB_API, url, &headers, g).await? else {
        return Ok(None);
    };
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| FetchError::SourceSchema {
            hint: format!("GitHub returned non-JSON for {path}: {e}"),
        })
}

/// `GET` a raw file as UTF-8 text; `Ok(None)` for a 404.
async fn raw_text(
    ctx: &FetchContext,
    raw: &Url,
    path: &str,
    g: &GithubRef,
) -> Result<Option<String>, FetchError> {
    let url = raw.join(path).map_err(|e| FetchError::SourceSchema {
        hint: format!("building a raw.githubusercontent.com URL for {path}: {e}"),
    })?;
    Ok(get(ctx, GITHUB_RAW, url, &[], g)
        .await?
        .map(|b| String::from_utf8_lossy(&b).into_owned()))
}

async fn get(
    ctx: &FetchContext,
    source: &'static str,
    url: Url,
    headers: &[(&str, &str)],
    g: &GithubRef,
) -> Result<Option<bytes::Bytes>, FetchError> {
    use crate::http::HttpError;
    let _permit = ctx.rate_limiter.acquire(source).await;
    let target = g.html_url();
    let row = |result, size, error_code| RowInput {
        event: LogEvent::Fetch,
        result,
        capability: Capability::Metadata,
        ref_: Some(target.as_str()),
        source: Some(source),
        error_code,
        size_bytes: size,
        license: None,
        store_path: None,
        canonical_digest: None,
    };
    match ctx
        .http
        .fetch_bytes_with_headers(source, url, headers)
        .await
    {
        Ok((body, _)) => {
            ctx.log
                .append(row(LogResult::Ok, Some(body.len() as u64), None))?;
            Ok(Some(body))
        }
        Err(HttpError::HttpStatus { status: 404, .. }) => {
            ctx.log
                .append(row(LogResult::Err, None, Some("NOT_FOUND")))?;
            Ok(None)
        }
        // A 429 is the rate limit and is reported as one, with a wait when
        // GitHub named none. A 403 is left a 403 (CAPABILITY_DENIED,
        // needs_config): GitHub sends it both for the spent unauthenticated
        // limit (60 an hour) and for a repository that is not public, and
        // without the response headers the two cannot be told apart. Calling
        // every 403 retryable would have a private repository retried every
        // 30 s under repeat suppression; [`explain`] names both causes.
        Err(HttpError::HttpStatus {
            status: 429,
            url,
            retry_after_ms,
        }) => {
            let e = FetchError::Http(HttpError::HttpStatus {
                status: 429,
                url,
                retry_after_ms: retry_after_ms.or(Some(60_000)),
            });
            let code = crate::ErrorCode::from(&e);
            ctx.log
                .append(row(LogResult::Err, None, Some(code.as_wire())))?;
            Err(e)
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

/// What a GitHub error means, when the status alone would mislead: a 403
/// is either the spent unauthenticated limit or a repository that is not
/// public, and GitHub's status does not say which.
#[must_use]
pub fn explain(e: &FetchError) -> Option<&'static str> {
    match e {
        FetchError::Http(crate::http::HttpError::HttpStatus { status: 403, .. }) => Some(
            "GitHub answers 403 both when the unauthenticated limit (60 requests an hour per \
             address) is spent -- it resets within the hour -- and when the repository is not \
             public; doiget sends no token",
        ),
        _ => None,
    }
}

fn year_of(date: &str) -> Option<i32> {
    date.get(..4)?.parse().ok()
}

/// Which DOI a Zenodo software record is cited by (#614).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZenodoDoi {
    /// The DOI given is a version DOI; `concept` names every version.
    Version {
        /// The concept DOI, from `relatedIdentifiers` `IsVersionOf`.
        concept: String,
    },
    /// The DOI given is itself the concept DOI (the record lists versions).
    Concept,
}

/// Classify a DataCite record's DOI by its `relatedIdentifiers`: a version
/// DOI points `IsVersionOf` at its concept; a concept DOI `HasVersion`s.
/// `None` when the record says neither (not Zenodo-style versioning).
#[must_use]
pub fn zenodo_doi(attributes: &Value) -> Option<ZenodoDoi> {
    let related = attributes.get("relatedIdentifiers")?.as_array()?;
    let doi_rel = |kind: &str| {
        related.iter().find(|r| {
            r.get("relationType").and_then(Value::as_str) == Some(kind)
                && r.get("relatedIdentifierType")
                    .and_then(Value::as_str)
                    .is_some_and(|t| t.eq_ignore_ascii_case("DOI"))
        })
    };
    if let Some(concept) = doi_rel("IsVersionOf")
        .and_then(|r| r.get("relatedIdentifier"))
        .and_then(Value::as_str)
    {
        return Some(ZenodoDoi::Version {
            concept: concept.to_string(),
        });
    }
    doi_rel("HasVersion").map(|_| ZenodoDoi::Concept)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn a_release_tag_or_repository_url_parses_and_nothing_else_does() {
        let g = GithubRef::parse("https://github.com/srwhite59/HFDMRG.jl/releases/tag/v0.1.0")
            .expect("release");
        assert_eq!(
            (g.owner.as_str(), g.repo.as_str(), g.tag.as_deref()),
            ("srwhite59", "HFDMRG.jl", Some("v0.1.0"))
        );
        assert_eq!(g.default_key(), "HFDMRG_jl_v0_1_0");
        assert_eq!(
            GithubRef::parse("https://github.com/o/r/tree/release/2.0")
                .unwrap()
                .tag
                .as_deref(),
            Some("release/2.0")
        );
        let bare = GithubRef::parse("https://github.com/o/r.git").unwrap();
        assert_eq!((bare.repo.as_str(), bare.tag), ("r", None));
        for no in [
            "https://gitlab.com/o/r",
            "https://github.com/o",
            "https://github.com/o/r/issues/3",
            "10.5281/zenodo.123",
            "github.com/o/r",
        ] {
            assert_eq!(GithubRef::parse(no), None, "{no}");
        }
    }

    #[test]
    fn cff_top_level_fields_and_authors_are_read_and_nested_blocks_are_not() {
        let cff = parse_cff(
            r#"cff-version: 1.2.0
message: "If you use this software, please cite it as below."
title: "HFDMRG.jl: Hartree-Fock DMRG"   # the software
version: 0.1.0
doi: 10.5281/zenodo.1234567
date-released: 2023-05-02
authors:
  - family-names: White
    given-names: Steven R.
  - family-names: Beethoven
    name-particle: van
    given-names: Ludwig
  - name: "The HFDMRG Team"
preferred-citation:
  type: article
  title: "Not the software"
  authors:
    - family-names: Other
      given-names: Paper
"#,
        );
        assert_eq!(cff.title.as_deref(), Some("HFDMRG.jl: Hartree-Fock DMRG"));
        assert_eq!(cff.version.as_deref(), Some("0.1.0"));
        assert_eq!(cff.doi.as_deref(), Some("10.5281/zenodo.1234567"));
        assert_eq!(cff.date_released.as_deref(), Some("2023-05-02"));
        assert_eq!(
            cff.authors,
            vec!["Steven R. White", "Ludwig van Beethoven", "The HFDMRG Team"]
        );
    }

    #[test]
    fn an_unreadable_cff_is_an_empty_one() {
        assert_eq!(parse_cff("{not: yaml at all"), Cff::default());
        assert_eq!(parse_cff("authors: [{name: x}]\n"), Cff::default());
    }

    #[test]
    fn a_zenodo_record_says_whether_its_doi_is_a_version_or_the_concept() {
        let version = serde_json::json!({"relatedIdentifiers": [
            {"relationType": "IsVersionOf", "relatedIdentifierType": "DOI",
             "relatedIdentifier": "10.5281/zenodo.100"}
        ]});
        assert_eq!(
            zenodo_doi(&version),
            Some(ZenodoDoi::Version {
                concept: "10.5281/zenodo.100".into()
            })
        );
        let concept = serde_json::json!({"relatedIdentifiers": [
            {"relationType": "HasVersion", "relatedIdentifierType": "DOI",
             "relatedIdentifier": "10.5281/zenodo.101"}
        ]});
        assert_eq!(zenodo_doi(&concept), Some(ZenodoDoi::Concept));
        assert_eq!(zenodo_doi(&serde_json::json!({})), None);
    }

    mod live_shape {
        //! Through the real HTTP client and log, against a mock GitHub.
        use super::super::*;
        use std::sync::Arc;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        async fn ctx_for(server: &MockServer) -> (FetchContext, tempfile::TempDir) {
            let host = server.address().to_string();
            let td = tempfile::TempDir::new().expect("tempdir");
            let log = camino::Utf8PathBuf::try_from(td.path().join("log.jsonl")).expect("utf-8");
            std::env::set_var(GITHUB_API_BASE_ENV, server.uri());
            std::env::set_var(GITHUB_RAW_BASE_ENV, server.uri());
            let session_id = "01J0000000000000000000GH14".to_string();
            let ctx = FetchContext {
                http: Arc::new(crate::http::HttpClient::new_for_tests_allow_http_multi(&[
                    (GITHUB_API, host.as_str()),
                    (GITHUB_RAW, host.as_str()),
                ])),
                rate_limiter: Arc::new(crate::rate_limiter::RateLimiter::new(
                    crate::RateLimits::HARD_CODED,
                )),
                log: Arc::new(
                    crate::provenance::ProvenanceLog::open(log, session_id.clone()).expect("log"),
                ),
                session_id,
                cache_root: None,
            };
            (ctx, td)
        }

        fn clear_env() {
            std::env::remove_var(GITHUB_API_BASE_ENV);
            std::env::remove_var(GITHUB_RAW_BASE_ENV);
        }

        async fn mount_repo(server: &MockServer) {
            Mock::given(method("GET"))
                .and(path("/repos/srwhite59/HFDMRG.jl"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "name": "HFDMRG.jl", "owner": {"login": "srwhite59"},
                    "html_url": "https://github.com/srwhite59/HFDMRG.jl"
                })))
                .mount(server)
                .await;
        }

        #[tokio::test]
        #[serial_test::serial]
        async fn a_release_with_a_cff_cites_its_authors_version_and_date() {
            let server = MockServer::start().await;
            mount_repo(&server).await;
            Mock::given(method("GET"))
                .and(path("/repos/srwhite59/HFDMRG.jl/releases/tags/v0.1.0"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "tag_name": "v0.1.0", "published_at": "2023-05-02T10:00:00Z",
                    "html_url": "https://github.com/srwhite59/HFDMRG.jl/releases/tag/v0.1.0"
                })))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/srwhite59/HFDMRG.jl/v0.1.0/CITATION.cff"))
                .respond_with(ResponseTemplate::new(200).set_body_string(
                    "title: HFDMRG.jl\nauthors:\n  - family-names: White\n    given-names: Steven R.\n",
                ))
                .mount(&server)
                .await;
            let (ctx, _td) = ctx_for(&server).await;
            let g = GithubRef::parse("https://github.com/srwhite59/HFDMRG.jl/releases/tag/v0.1.0")
                .unwrap();
            let cited = resolve_github(&g, &ctx).await.expect("cites");
            clear_env();
            let m = &cited.metadata;
            assert!(cited.from_cff);
            assert_eq!(m.authors, vec!["Steven R. White"]);
            assert_eq!(m.year, Some(2023));
            assert_eq!(m.type_.as_deref(), Some("software"));
            assert_eq!(
                m.other.get("version").and_then(toml::Value::as_str),
                Some("v0.1.0")
            );
            let bib = crate::store::render::to_bibtex("HFDMRG_jl_v0_1_0", m);
            assert!(bib.starts_with("@software{HFDMRG_jl_v0_1_0,"), "{bib}");
            assert!(bib.contains("version    = {v0.1.0}"), "{bib}");
            assert!(
                bib.contains(
                    "url        = {https://github.com/srwhite59/HFDMRG.jl/releases/tag/v0.1.0}"
                ),
                "{bib}"
            );
            let csl = crate::store::render::to_csl_array("k", m);
            assert_eq!(csl[0]["type"], "software");
            assert_eq!(csl[0]["version"], "v0.1.0");
        }

        #[tokio::test]
        #[serial_test::serial]
        async fn a_tag_without_a_release_or_cff_uses_the_commit_date_and_the_owner() {
            let server = MockServer::start().await;
            mount_repo(&server).await;
            Mock::given(method("GET"))
                .and(path("/repos/srwhite59/HFDMRG.jl/commits/v0.0.9"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "commit": {"committer": {"date": "2022-11-30T08:00:00Z"}}
                })))
                .mount(&server)
                .await;
            let (ctx, _td) = ctx_for(&server).await;
            let g = GithubRef::parse("https://github.com/srwhite59/HFDMRG.jl/tree/v0.0.9").unwrap();
            let cited = resolve_github(&g, &ctx).await.expect("cites");
            clear_env();
            assert!(!cited.from_cff);
            assert_eq!(cited.metadata.authors, vec!["srwhite59"]);
            assert_eq!(cited.metadata.year, Some(2022));
            assert!(
                cited.notes.iter().any(|n| n.contains("no GitHub release")),
                "{:?}",
                cited.notes
            );
            assert!(
                cited.notes.iter().any(|n| n.contains("repository owner")),
                "{:?}",
                cited.notes
            );
        }

        #[tokio::test]
        #[serial_test::serial]
        async fn a_bare_repository_url_cites_the_latest_release_or_no_version() {
            let server = MockServer::start().await;
            mount_repo(&server).await;
            Mock::given(method("GET"))
                .and(path("/repos/srwhite59/HFDMRG.jl/releases/latest"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "tag_name": "v0.2.0", "published_at": "2024-01-15T00:00:00Z"
                })))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/repos/o/norel"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "name": "norel", "owner": {"login": "o"}
                })))
                .mount(&server)
                .await;
            let (ctx, _td) = ctx_for(&server).await;
            let latest = resolve_github(
                &GithubRef::parse("https://github.com/srwhite59/HFDMRG.jl").unwrap(),
                &ctx,
            )
            .await
            .expect("cites");
            let none = resolve_github(
                &GithubRef::parse("https://github.com/o/norel").unwrap(),
                &ctx,
            )
            .await
            .expect("cites");
            clear_env();
            assert_eq!(
                latest
                    .metadata
                    .other
                    .get("version")
                    .and_then(toml::Value::as_str),
                Some("v0.2.0")
            );
            assert_eq!(latest.metadata.year, Some(2024));
            assert!(
                latest.notes.iter().any(|n| n.contains("latest release")),
                "{:?}",
                latest.notes
            );
            // No release and no tag: no version and no date are invented.
            assert!(!none.metadata.other.contains_key("version"));
            assert_eq!(none.metadata.year, None);
        }

        #[tokio::test]
        #[serial_test::serial]
        async fn a_missing_repository_is_not_found_and_the_hourly_limit_is_a_rate_limit() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/repos/o/gone"))
                .respond_with(ResponseTemplate::new(404))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/repos/o/busy"))
                .respond_with(ResponseTemplate::new(403))
                .mount(&server)
                .await;
            let (ctx, _td) = ctx_for(&server).await;
            let gone = resolve_github(
                &GithubRef::parse("https://github.com/o/gone").unwrap(),
                &ctx,
            )
            .await
            .expect_err("404");
            let busy = resolve_github(
                &GithubRef::parse("https://github.com/o/busy").unwrap(),
                &ctx,
            )
            .await
            .expect_err("403");
            let resolves = github_resolves(
                &GithubRef::parse("https://github.com/o/gone").unwrap(),
                &ctx,
            )
            .await
            .expect("a clean answer");
            clear_env();
            assert_eq!(crate::ErrorCode::from(&gone), crate::ErrorCode::NotFound);
            // A 403 stays a refusal (a private repository must not be
            // retried every 30 s), and the explanation names both causes.
            assert_eq!(
                crate::ErrorCode::from(&busy),
                crate::ErrorCode::CapabilityDenied
            );
            assert!(explain(&busy).is_some_and(|w| w.contains("60 requests an hour")));
            assert!(!resolves);
            // Every request is on the provenance log, under its own source.
            let log = std::fs::read_to_string(ctx.log.path()).expect("log");
            assert!(log.contains("\"source\":\"github\""), "{log}");
        }
    }
}
