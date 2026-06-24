//! Citation renderers for stored [`Metadata`] — BibTeX and CSL JSON 1.0.
//!
//! Phase 2 / Slice 15b. The rendering logic originally lived in the
//! `doiget-cli` `bib` / `csl` subcommands; it is hoisted here so the
//! `doiget-mcp` `doiget_bibtex_export` / `doiget_csl_export` tools and
//! the CLI share a single implementation (`docs/MCP_TOOLS.md` §1 rows
//! `doiget_bibtex_export` / `doiget_csl_export`).
//!
//! Both renderers are pure functions of a [`Metadata`] plus a citation
//! key (the entry's safekey). No I/O, no network. They emit the Phase 1
//! binding fields from `docs/STORE.md` §2 (title, authors, year, doi,
//! venue, publisher, issn); richer entry-type / field mapping is a
//! Phase 2 follow-up.

use serde::Serialize;

use super::Metadata;

// ---------------------------------------------------------------------------
// BibTeX
// ---------------------------------------------------------------------------

/// Render a single BibTeX entry for `m`, keyed by `citation_key`.
///
/// `journal-article` → `@article`; everything else → `@misc` (Phase 2
/// starter — `@inproceedings` / `@book` mapping is a follow-up). Field
/// order: `title`, `author`, `year`, `doi`, `journal`, `volume`,
/// `number`, `pages`, `publisher`, `issn`, then — when the entry carries
/// an arXiv id — `eprint`, `archivePrefix`, `primaryClass` (issue #303).
/// Any empty / `None` field is omitted. The returned string is a complete
/// entry terminated by `}\n`.
///
/// Literal `{` / `}` in a field value would unbalance the surrounding
/// braces; they are stripped (with a `tracing::warn!`) rather than
/// TeX-escaped — real-world Crossref / Unpaywall titles rarely contain
/// bare braces, so this is safe-by-default for the Phase 2 starter.
#[must_use]
pub fn to_bibtex(citation_key: &str, m: &Metadata) -> String {
    let mut out = String::new();
    let entry_type = bibtex_entry_type(m.type_.as_deref());
    out.push_str(&format!("@{entry_type}{{{citation_key},\n"));

    push_field(&mut out, "title", &m.title);
    if !m.authors.is_empty() {
        // BibTeX joins multiple authors with the literal token " and ".
        push_field(&mut out, "author", &m.authors.join(" and "));
    }
    if let Some(year) = m.year {
        push_field(&mut out, "year", &year.to_string());
    }
    if let Some(doi) = &m.doi {
        push_field(&mut out, "doi", doi.as_str());
    }
    if let Some(venue) = m.venue.as_deref() {
        if !venue.is_empty() {
            push_field(&mut out, "journal", venue);
        }
    }
    if let Some(volume) = m.volume.as_deref() {
        if !volume.is_empty() {
            push_field(&mut out, "volume", volume);
        }
    }
    // BibTeX names the issue field `number`.
    if let Some(issue) = m.issue.as_deref() {
        if !issue.is_empty() {
            push_field(&mut out, "number", issue);
        }
    }
    if let Some(pages) = m.pages.as_deref() {
        if !pages.is_empty() {
            push_field(&mut out, "pages", pages);
        }
    }
    if let Some(publisher) = m.publisher.as_deref() {
        if !publisher.is_empty() {
            push_field(&mut out, "publisher", publisher);
        }
    }
    if let Some(issn) = m.issn.as_deref() {
        if !issn.is_empty() {
            push_field(&mut out, "issn", issn);
        }
    }

    // arXiv preprint identity (issue #303): emit `eprint` + `archivePrefix`
    // (+ `primaryClass` when known) for any entry carrying an arXiv id, so
    // the reference resolves on arXiv and in reference managers instead of
    // reading as a title+author stub. Standard arXiv BibTeX convention;
    // applies to both the `@misc` preprint and a `@article` that also has a
    // preprint.
    if let Some(arxiv_id) = &m.arxiv_id {
        push_field(&mut out, "eprint", arxiv_id.as_str());
        push_field(&mut out, "archivePrefix", "arXiv");
        if let Some(class) = arxiv_primary_class(m) {
            push_field(&mut out, "primaryClass", &class);
        }
    }

    out.push_str("}\n");
    out
}

/// The arXiv primary subject class for a BibTeX `primaryClass` field.
///
/// Prefers the parsed Atom category (`Metadata::arxiv_categories[0]`, the
/// only source for a new-style id like `2012.03644`). Falls back to the
/// archive prefix embedded in an **old-style** id (`cond-mat/0403602` →
/// `cond-mat`); a new-style id with no stored categories yields `None`, so
/// the field is honestly omitted rather than guessed.
fn arxiv_primary_class(m: &Metadata) -> Option<String> {
    if let Some(first) = m.arxiv_categories.first() {
        return Some(first.clone());
    }
    m.arxiv_id.as_ref().and_then(|id| {
        id.as_str()
            .split_once('/')
            .map(|(archive, _)| archive.to_string())
    })
}

/// Map a Crossref-taxonomy `type` string to a BibTeX entry type.
///
/// Phase 2 starter only differentiates `journal-article` (→ `article`)
/// from everything else (→ `misc`).
fn bibtex_entry_type(type_: Option<&str>) -> &'static str {
    match type_ {
        Some("journal-article") => "article",
        _ => "misc",
    }
}

/// Append a single `  <key>      = {<value>},\n` line, padded so the `=`
/// columns line up across the seven-field Phase 2 surface (width 10 is
/// wide enough for `publisher`, the longest key).
fn push_field(out: &mut String, key: &str, value: &str) {
    let escaped = strip_bibtex_unsafe(key, value);
    out.push_str(&format!("  {key:<10} = {{{escaped}}},\n"));
}

/// Strip BibTeX-unsafe `{` / `}` from `value`, warning once per field so
/// the dropped characters are visible in stderr / structured logs.
///
/// Crossref embeds HTML / MathML markup in titles and venues (`<i>`,
/// `<sub>`, `<mml:math>…</mml:math>`); those `<…>` tags are removed first
/// (their inner text is kept) so the rendered BibTeX is clean enough to
/// paste into a `.bib`. This is the same pragmatic trade-off doi2bib
/// makes — it is a tag scrubber, not a TeX-aware math translator, so a
/// title's math markup collapses to its plain-text content rather than to
/// `$…$`.
fn strip_bibtex_unsafe(key: &str, value: &str) -> String {
    let detagged = strip_markup_tags(value);
    if detagged.contains('{') || detagged.contains('}') {
        tracing::warn!(
            field = key,
            "stripping literal '{{'/'}}' from BibTeX field value; \
             a TeX-aware escaper lands in a Phase 2 follow-up"
        );
    }
    detagged
        .chars()
        .filter(|c| !matches!(c, '{' | '}'))
        .collect()
}

/// Remove HTML / MathML markup tags (`<i>`, `<sub>`, `<mml:math>`, …),
/// keeping the text between them. Equivalent to deleting every `<…>`
/// run: a deliberately simple angle-bracket scanner, not an HTML parser.
///
/// A `<` with no matching `>` (e.g. genuine inline math `a < b` that
/// Crossref left unescaped) leaves the remainder verbatim — only
/// well-formed tag runs are dropped. Strings with no markup return
/// unchanged without allocating a scan buffer.
fn strip_markup_tags(value: &str) -> String {
    if !(value.contains('<') && value.contains('>')) {
        return value.to_string();
    }
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(lt) = rest.find('<') {
        match rest[lt..].find('>') {
            Some(gt_rel) => {
                out.push_str(&rest[..lt]);
                rest = &rest[lt + gt_rel + 1..];
            }
            // No closing '>' for this '<': keep the remainder as-is.
            None => break,
        }
    }
    out.push_str(rest);
    out
}

// ---------------------------------------------------------------------------
// CSL JSON 1.0
// ---------------------------------------------------------------------------

/// Render `m` as a CSL JSON 1.0 **array** (a single-element array, so it
/// is a drop-in for citeproc-js / pandoc `--csl-json` consumers that
/// expect a list of items), keyed by `citation_key`.
///
/// `journal-article` → CSL `article-journal`; everything else →
/// `manuscript` (citeproc-js renders that without forcing a container).
/// Empty optional fields are omitted from the JSON.
#[must_use]
pub fn to_csl_array(citation_key: &str, m: &Metadata) -> serde_json::Value {
    let item = build_csl_item(citation_key, m);
    // `CslItem` is all-`Serialize` over owned/borrowed primitives, so
    // `to_value` cannot fail; fall back to an empty array rather than
    // panicking if a future field breaks that invariant.
    serde_json::to_value([item]).unwrap_or_else(|_| serde_json::Value::Array(Vec::new()))
}

/// One CSL JSON 1.0 item, scoped to the binding fields the local
/// `Metadata` schema can populate. Field order is the citeproc-js
/// conventional order so a human diffing two outputs sees a stable
/// column layout.
#[derive(Debug, Serialize)]
struct CslItem<'a> {
    id: &'a str,
    #[serde(rename = "type")]
    type_: &'static str,
    title: &'a str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    author: Vec<CslName>,
    #[serde(skip_serializing_if = "Option::is_none")]
    issued: Option<CslIssued>,
    #[serde(rename = "DOI", skip_serializing_if = "Option::is_none")]
    doi: Option<&'a str>,
    #[serde(rename = "container-title", skip_serializing_if = "Option::is_none")]
    container_title: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    volume: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    issue: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    page: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    publisher: Option<&'a str>,
    #[serde(rename = "ISSN", skip_serializing_if = "Option::is_none")]
    issn: Option<&'a str>,
}

/// CSL name-variable shape. Empty halves are omitted so a single-token
/// name lands as `{"family": "Plato"}` rather than with a stray `given`.
#[derive(Debug, Serialize)]
struct CslName {
    #[serde(skip_serializing_if = "String::is_empty")]
    family: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    given: String,
}

/// CSL date-variable shape, year-only for Phase 1. `date-parts` is a
/// list-of-lists; we only know the year so the inner list is `[<year>]`.
#[derive(Debug, Serialize)]
struct CslIssued {
    #[serde(rename = "date-parts")]
    date_parts: Vec<Vec<i32>>,
}

fn build_csl_item<'a>(citation_key: &'a str, m: &'a Metadata) -> CslItem<'a> {
    CslItem {
        id: citation_key,
        type_: match m.type_.as_deref() {
            Some("journal-article") => "article-journal",
            _ => "manuscript",
        },
        title: &m.title,
        author: m.authors.iter().map(|s| parse_author(s)).collect(),
        issued: m.year.map(|y| CslIssued {
            date_parts: vec![vec![y]],
        }),
        doi: m.doi.as_ref().map(|d| d.as_str()),
        container_title: m.venue.as_deref(),
        volume: m.volume.as_deref(),
        issue: m.issue.as_deref(),
        page: m.pages.as_deref(),
        publisher: m.publisher.as_deref(),
        issn: m.issn.as_deref(),
    }
}

/// Split a free-form name string into CSL `family` / `given` halves.
///
/// - `Family, Given` (comma present): split on the first comma.
/// - Otherwise split on the LAST whitespace: left is given, right is
///   family (`"Alice Researcher"` → family `"Researcher"`, given
///   `"Alice"`) — the convention citeproc-js uses for string names.
/// - Single token: whole string is the family, `given` empty.
fn parse_author(name: &str) -> CslName {
    let trimmed = name.trim();
    if let Some((family, given)) = trimmed.split_once(',') {
        CslName {
            family: family.trim().to_string(),
            given: given.trim().to_string(),
        }
    } else if let Some(idx) = trimmed.rfind(char::is_whitespace) {
        let (given, family) = trimmed.split_at(idx);
        CslName {
            family: family.trim().to_string(),
            given: given.trim().to_string(),
        }
    } else {
        CslName {
            family: trimmed.to_string(),
            given: String::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::TimeZone;

    use super::*;
    use crate::store::{DoigetExtension, Metadata};
    use crate::{Doi, SCHEMA_VERSION};

    fn fixture(type_: Option<&str>) -> Metadata {
        Metadata {
            schema_version: SCHEMA_VERSION.to_string(),
            title: "Quantum Stuff".to_string(),
            authors: vec!["Alice Researcher".to_string(), "Bob Coauthor".to_string()],
            year: Some(2026),
            doi: Some(Doi::parse("10.1234/example").expect("valid DOI")),
            arxiv_id: None,
            arxiv_categories: vec![],
            abstract_: None,
            venue: Some("Phys Rev X".to_string()),
            volume: Some("12".to_string()),
            issue: Some("3".to_string()),
            pages: Some("031001".to_string()),
            publisher: Some("APS".to_string()),
            issn: Some("2160-3308".to_string()),
            isbn: None,
            type_: type_.map(str::to_string),
            keywords: vec![],
            url: None,
            pdf_path: None,
            doiget: Some(DoigetExtension {
                fetched_at: chrono::Utc
                    .with_ymd_and_hms(2026, 5, 6, 12, 0, 0)
                    .single()
                    .expect("valid timestamp"),
                source: "unpaywall".to_string(),
                license: "CC-BY-4.0".to_string(),
                oa_status: None,
                size_bytes: 1234,
                mcp_call_id: None,
                tags: Vec::new(),
                collections: Vec::new(),
                annotation: None,
            }),
            other: BTreeMap::new(),
        }
    }

    // ---- BibTeX ----

    #[test]
    fn bibtex_journal_article_renders_as_article() {
        let s = to_bibtex("doi_10.1234_example", &fixture(Some("journal-article")));
        assert!(s.starts_with("@article{doi_10.1234_example,\n"), "{s}");
        assert!(s.contains("title      = {Quantum Stuff},"), "{s}");
        assert!(
            s.contains("author     = {Alice Researcher and Bob Coauthor},"),
            "{s}"
        );
        assert!(s.contains("year       = {2026},"), "{s}");
        assert!(s.contains("doi        = {10.1234/example},"), "{s}");
        assert!(s.contains("journal    = {Phys Rev X},"), "{s}");
        assert!(s.contains("volume     = {12},"), "{s}");
        assert!(s.contains("number     = {3},"), "{s}");
        assert!(s.contains("pages      = {031001},"), "{s}");
        assert!(s.contains("publisher  = {APS},"), "{s}");
        assert!(s.contains("issn       = {2160-3308},"), "{s}");
        assert!(s.ends_with("}\n"), "{s}");
    }

    #[test]
    fn bibtex_missing_and_unknown_type_render_as_misc() {
        assert!(to_bibtex("k", &fixture(None)).starts_with("@misc{k,\n"));
        assert!(to_bibtex("k", &fixture(Some("posted-content"))).starts_with("@misc{k,\n"));
    }

    #[test]
    fn bibtex_arxiv_emits_eprint_archiveprefix_primaryclass() {
        // issue #303: an arXiv entry must carry the preprint identity, not
        // just title + author. New-style id → `primaryClass` from the
        // parsed Atom category (`arxiv_categories[0]`).
        let mut m = fixture(None);
        m.doi = None;
        m.venue = None;
        m.volume = None;
        m.issue = None;
        m.pages = None;
        m.publisher = None;
        m.issn = None;
        m.arxiv_id = Some(crate::ArxivId::parse("2012.03644").expect("valid id"));
        m.arxiv_categories = vec!["cond-mat.str-el".to_string(), "cond-mat.dis-nn".to_string()];
        let s = to_bibtex("arxiv_2012.03644", &m);
        assert!(s.starts_with("@misc{arxiv_2012.03644,\n"), "{s}");
        // Long keys are not padded, so the field lines are exact.
        assert!(s.contains("archivePrefix = {arXiv},"), "{s}");
        assert!(s.contains("primaryClass = {cond-mat.str-el},"), "{s}");
        // `eprint` is a short key; assert the value to avoid padding fuss.
        assert!(s.contains("eprint") && s.contains("= {2012.03644},"), "{s}");
        // Year still rendered (populated by the cite overlay upstream).
        assert!(s.contains("year       = {2026},"), "{s}");
    }

    #[test]
    fn bibtex_arxiv_old_style_id_primaryclass_from_prefix() {
        // No stored categories (e.g. a pre-#303 store entry): the old-style
        // id's archive prefix supplies `primaryClass`.
        let mut m = fixture(None);
        m.doi = None;
        m.arxiv_id = Some(crate::ArxivId::parse("cond-mat/0403602").expect("valid id"));
        m.arxiv_categories = vec![];
        let s = to_bibtex("k", &m);
        assert!(s.contains("= {cond-mat/0403602},"), "{s}");
        assert!(s.contains("primaryClass = {cond-mat},"), "{s}");
    }

    #[test]
    fn bibtex_non_arxiv_omits_eprint() {
        // A DOI-only entry must not grow arXiv fields.
        let s = to_bibtex("k", &fixture(Some("journal-article")));
        assert!(!s.contains("eprint"), "{s}");
        assert!(!s.contains("archivePrefix"), "{s}");
    }

    #[test]
    fn bibtex_empty_optionals_omitted() {
        let mut m = fixture(Some("journal-article"));
        m.venue = None;
        m.volume = None;
        m.issue = None;
        m.pages = None;
        m.publisher = None;
        m.issn = None;
        let s = to_bibtex("k", &m);
        assert!(!s.contains("journal"), "{s}");
        assert!(!s.contains("volume"), "{s}");
        assert!(!s.contains("number"), "{s}");
        assert!(!s.contains("pages"), "{s}");
        assert!(!s.contains("publisher"), "{s}");
        assert!(!s.contains("issn"), "{s}");
        assert!(s.contains("title") && s.contains("author") && s.contains("year"));
    }

    #[test]
    fn bibtex_no_authors_omits_author_line() {
        let mut m = fixture(Some("journal-article"));
        m.authors = vec![];
        assert!(!to_bibtex("k", &m).contains("author"));
    }

    #[test]
    fn bibtex_braces_stripped() {
        let mut m = fixture(Some("journal-article"));
        m.title = "A {curly} Title".to_string();
        assert!(to_bibtex("k", &m).contains("title      = {A curly Title},"));
    }

    #[test]
    fn bibtex_html_mathml_tags_stripped() {
        let mut m = fixture(Some("journal-article"));
        // A Crossref-style title with MathML + an inline italic tag.
        m.title = "Spin-<i>S</i> chains with <mml:math><mml:mi>S</mml:mi>\
                   </mml:math>=1 order"
            .to_string();
        let s = to_bibtex("k", &m);
        assert!(
            s.contains("title      = {Spin-S chains with S=1 order},"),
            "{s}"
        );
    }

    #[test]
    fn bibtex_unescaped_lt_without_close_is_preserved() {
        // A bare `<` with no closing `>` is genuine math, not a tag:
        // keep the remainder verbatim rather than swallowing it.
        let mut m = fixture(Some("journal-article"));
        m.title = "Regime a < b holds".to_string();
        assert!(to_bibtex("k", &m).contains("title      = {Regime a < b holds},"));
    }

    // ---- CSL ----

    #[test]
    fn csl_array_shape_and_fields() {
        let v = to_csl_array("doi_10.1234_example", &fixture(Some("journal-article")));
        let arr = v.as_array().expect("CSL output is an array");
        assert_eq!(arr.len(), 1);
        let it = &arr[0];
        assert_eq!(it["id"], "doi_10.1234_example");
        assert_eq!(it["type"], "article-journal");
        assert_eq!(it["title"], "Quantum Stuff");
        assert_eq!(it["DOI"], "10.1234/example");
        assert_eq!(it["container-title"], "Phys Rev X");
        assert_eq!(it["volume"], "12");
        assert_eq!(it["issue"], "3");
        assert_eq!(it["page"], "031001");
        assert_eq!(it["ISSN"], "2160-3308");
        assert_eq!(it["issued"]["date-parts"][0][0], 2026);
        assert_eq!(it["author"][0]["family"], "Researcher");
        assert_eq!(it["author"][0]["given"], "Alice");
    }

    #[test]
    fn csl_unknown_type_is_manuscript() {
        let v = to_csl_array("k", &fixture(None));
        assert_eq!(v.as_array().unwrap()[0]["type"], "manuscript");
    }

    #[test]
    fn csl_comma_name_split() {
        let mut m = fixture(Some("journal-article"));
        m.authors = vec!["Curie, Marie".to_string(), "Plato".to_string()];
        let v = to_csl_array("k", &m);
        let authors = v.as_array().unwrap()[0]["author"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(authors[0]["family"], "Curie");
        assert_eq!(authors[0]["given"], "Marie");
        assert_eq!(authors[1]["family"], "Plato");
        assert!(
            authors[1].get("given").is_none(),
            "single-token name has no given"
        );
    }
}
