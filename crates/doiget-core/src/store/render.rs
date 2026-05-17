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
/// order: `title`, `author`, `year`, `doi`, `journal`, `publisher`,
/// `issn`; any empty / `None` field is omitted. The returned string is a
/// complete entry terminated by `}\n`.
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

    out.push_str("}\n");
    out
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
fn strip_bibtex_unsafe(key: &str, value: &str) -> String {
    if value.contains('{') || value.contains('}') {
        tracing::warn!(
            field = key,
            "stripping literal '{{'/'}}' from BibTeX field value; \
             a TeX-aware escaper lands in a Phase 2 follow-up"
        );
    }
    value.chars().filter(|c| !matches!(c, '{' | '}')).collect()
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
            abstract_: None,
            venue: Some("Phys Rev X".to_string()),
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
                size_bytes: 1234,
                mcp_call_id: None,
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
    fn bibtex_empty_optionals_omitted() {
        let mut m = fixture(Some("journal-article"));
        m.venue = None;
        m.publisher = None;
        m.issn = None;
        let s = to_bibtex("k", &m);
        assert!(!s.contains("journal"), "{s}");
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
