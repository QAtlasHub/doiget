//! `doiget bib <ref>` subcommand — emit a BibTeX entry for a stored entry.
//!
//! Phase 2 starter: read the stored [`Metadata`] for a [`Ref`] and write a
//! minimal BibTeX entry on stdout. The Phase 1 binding fields from
//! `docs/STORE.md` §2 (title, authors, year, doi, venue, publisher, issn)
//! are emitted as `@article` for `type = "journal-article"` and `@misc`
//! otherwise. Richer entry-type mapping (`@inproceedings`, `@book`, custom
//! note fields) lands in a follow-up.
//!
//! Output is a single hand-rolled BibTeX entry — no external bibtex /
//! biber crate is pulled in for Phase 2. The emitter is intentionally
//! conservative: any literal `{` / `}` in a field value is stripped (with a
//! `tracing::warn!`), and missing / empty fields are omitted. Real-world
//! Crossref / Unpaywall titles rarely contain bare braces, so the
//! strip-on-encounter policy is safe-by-default and keeps the Phase 2
//! starter free of a TeX-aware escaper.

use std::io::Write;

use anyhow::{bail, Context, Result};

use doiget_core::store::{FsStore, Metadata, Store};
use doiget_core::Ref;

use super::resolve_store_root;

/// Run the `bib` subcommand against the configured store.
///
/// `input` is the user-supplied ref string (e.g. `"10.1234/example"`,
/// `"arxiv:2401.12345"`, or any of the schemes accepted by [`Ref::parse`]).
///
/// On success, a BibTeX entry derived from the entry's [`Metadata`] is
/// written to stdout. On a missing entry, the function returns an error
/// so the CLI exits non-zero.
pub fn run(input: String) -> Result<()> {
    let ref_ = Ref::parse(&input).with_context(|| format!("invalid ref: {input}"))?;
    let safekey = ref_.safekey();

    let store_root = resolve_store_root()?;
    let store = FsStore::new(store_root)?;

    let metadata = store
        .read(&safekey)
        .with_context(|| format!("failed to read store entry for {input}"))?;

    match metadata {
        Some(m) => {
            // Workspace lints deny `print_stdout` (the `print!`/`println!`
            // macros) so JSON-RPC frames never collide with diagnostics.
            // `writeln!` against an explicit `stdout().lock()` is the
            // sanctioned escape hatch — the caller chose stdout
            // explicitly. See `docs/SECURITY.md` §3 / ADR-0001.
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            write_bibtex_entry(&mut out, safekey.as_str(), &m)
                .context("failed to write BibTeX entry to stdout")?;
            Ok(())
        }
        None => bail!("no entry for {input}"),
    }
}

/// Map a Crossref-taxonomy `type` string to a BibTeX entry type.
///
/// Phase 2 starter only differentiates `journal-article` (→ `@article`)
/// from everything else (→ `@misc`). Extending the map to
/// `@inproceedings` / `@book` / `@techreport` is a Phase 2 follow-up that
/// requires an explicit list of accepted Crossref types per entry kind.
fn entry_type_for(type_: Option<&str>) -> &'static str {
    match type_ {
        Some("journal-article") => "article",
        _ => "misc",
    }
}

/// Write a single BibTeX entry for `m` keyed by `citation_key`.
///
/// Field order matches the spec block: `title`, `author`, `year`, `doi`,
/// `journal`, `publisher`, `issn`. Any field that resolves to `None` /
/// empty is omitted entirely.
fn write_bibtex_entry<W: Write>(
    out: &mut W,
    citation_key: &str,
    m: &Metadata,
) -> std::io::Result<()> {
    let entry_type = entry_type_for(m.type_.as_deref());
    writeln!(out, "@{entry_type}{{{citation_key},")?;

    write_field(out, "title", &m.title)?;
    if !m.authors.is_empty() {
        // BibTeX joins multiple authors with the literal token " and ".
        write_field(out, "author", &m.authors.join(" and "))?;
    }
    if let Some(year) = m.year {
        write_field(out, "year", &year.to_string())?;
    }
    if let Some(doi) = &m.doi {
        write_field(out, "doi", doi.as_str())?;
    }
    if let Some(venue) = m.venue.as_deref() {
        if !venue.is_empty() {
            write_field(out, "journal", venue)?;
        }
    }
    if let Some(publisher) = m.publisher.as_deref() {
        if !publisher.is_empty() {
            write_field(out, "publisher", publisher)?;
        }
    }
    if let Some(issn) = m.issn.as_deref() {
        if !issn.is_empty() {
            write_field(out, "issn", issn)?;
        }
    }

    writeln!(out, "}}")?;
    Ok(())
}

/// Write a single `<key> = {<value>},` line, padded so the `=` columns
/// line up across the seven-field Phase 2 surface.
fn write_field<W: Write>(out: &mut W, key: &str, value: &str) -> std::io::Result<()> {
    let escaped = strip_unsafe(key, value);
    // Width 10 is wide enough for `publisher` (the longest key in the
    // Phase 2 set). Future fields longer than 10 chars will reflow the
    // table; that's an acceptable cost for a hand-rolled emitter.
    writeln!(out, "  {key:<10} = {{{escaped}}},")
}

/// Strip BibTeX-unsafe characters from `value`.
///
/// Phase 2 starter takes the pragmatic route: any literal `{` or `}`
/// would unbalance the surrounding braces, and no escaping convention
/// applies inside a brace-wrapped value without falling back to a full
/// LaTeX escaper. Real-world titles rarely contain bare braces, so we
/// strip them and emit a `tracing::warn!` so the dropped chars are
/// visible in stderr / structured logs.
fn strip_unsafe(key: &str, value: &str) -> String {
    let has_braces = value.contains('{') || value.contains('}');
    if has_braces {
        tracing::warn!(
            field = key,
            "stripping literal '{{'/'}}' from BibTeX field value; \
             a TeX-aware escaper lands in a Phase 2 follow-up"
        );
    }
    value.chars().filter(|c| !matches!(c, '{' | '}')).collect()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::TimeZone;

    use doiget_core::store::{DoigetExtension, Metadata};
    use doiget_core::{Doi, SCHEMA_VERSION};

    use super::*;

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

    fn render(citation_key: &str, m: &Metadata) -> String {
        let mut buf: Vec<u8> = Vec::new();
        write_bibtex_entry(&mut buf, citation_key, m).expect("write_bibtex_entry");
        String::from_utf8(buf).expect("UTF-8 BibTeX output")
    }

    #[test]
    fn journal_article_renders_as_article() {
        let m = fixture(Some("journal-article"));
        let s = render("doi_10.1234_example", &m);
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
    fn missing_type_renders_as_misc() {
        let m = fixture(None);
        let s = render("doi_10.1234_example", &m);
        assert!(s.starts_with("@misc{doi_10.1234_example,\n"), "{s}");
    }

    #[test]
    fn unknown_type_renders_as_misc() {
        let m = fixture(Some("posted-content"));
        let s = render("doi_10.1234_example", &m);
        assert!(s.starts_with("@misc{doi_10.1234_example,\n"), "{s}");
    }

    #[test]
    fn empty_optional_fields_are_omitted() {
        let mut m = fixture(Some("journal-article"));
        m.venue = None;
        m.publisher = None;
        m.issn = None;
        let s = render("doi_10.1234_example", &m);
        assert!(!s.contains("journal"), "{s}");
        assert!(!s.contains("publisher"), "{s}");
        assert!(!s.contains("issn"), "{s}");
        // Required-shape fields still emitted.
        assert!(s.contains("title"));
        assert!(s.contains("author"));
        assert!(s.contains("year"));
        assert!(s.contains("doi"));
    }

    #[test]
    fn no_authors_omits_author_line() {
        let mut m = fixture(Some("journal-article"));
        m.authors = vec![];
        let s = render("doi_10.1234_example", &m);
        assert!(!s.contains("author"), "{s}");
    }

    #[test]
    fn braces_in_value_are_stripped() {
        let mut m = fixture(Some("journal-article"));
        m.title = "A {curly} Title".to_string();
        let s = render("doi_10.1234_example", &m);
        assert!(s.contains("title      = {A curly Title},"), "{s}");
    }
}
