//! `doiget csl <ref>` — emit a CSL JSON 1.0 array for a stored Metadata.
//!
//! Phase 1 emits a single-entry array with the binding fields from
//! [`docs/STORE.md`](../../../../docs/STORE.md) §2 (title, authors, year,
//! doi, venue, publisher, issn). Richer shapes (abstract, keywords,
//! container-IDs, page ranges) follow in Phase 2.
//!
//! The output is human-readable pretty-printed JSON, deliberately a JSON
//! ARRAY (not a bare object) so it is a drop-in for citeproc-js / pandoc
//! `--csl-json` consumers that expect an array of items.
//!
//! Reads go through [`Store::read`] per [`docs/PUBLIC_API.md`](../../../../docs/PUBLIC_API.md)
//! §2. Network access is never required.

use std::io::Write;

use anyhow::{bail, Context, Result};
use serde::Serialize;

use doiget_core::store::{FsStore, Metadata, Store};
use doiget_core::Ref;

use super::resolve_store_root;

/// Run the `csl` subcommand against the configured store.
///
/// `input` is the user-supplied ref string (e.g. `"10.1234/example"` or
/// `"arxiv:2401.12345"` — anything accepted by [`Ref::parse`]).
///
/// On success, a single-element CSL JSON 1.0 array is written to stdout.
/// On a missing entry, the function returns an error so the CLI exits
/// non-zero — pipelines can distinguish "not in store" from "empty entry".
pub fn run(input: String) -> Result<()> {
    let ref_ = Ref::parse(&input).with_context(|| format!("invalid ref: {input}"))?;
    let safekey = ref_.safekey();

    let store_root = resolve_store_root()?;
    let store = FsStore::new(store_root)?;

    let metadata = store
        .read(&safekey)
        .with_context(|| format!("failed to read store entry for {input}"))?;

    let metadata = match metadata {
        Some(m) => m,
        None => bail!("no entry for {input}"),
    };

    let item = build_csl_item(safekey.as_str(), &metadata);
    let array = vec![item];
    let json =
        serde_json::to_string_pretty(&array).context("failed to serialize CSL JSON for stdout")?;

    // Workspace lints deny `print_stdout` (`println!`/`print!`) so the
    // sanctioned escape hatch is `writeln!(stdout().lock(), ...)`.
    // See `docs/SECURITY.md` §3 / ADR-0001 — this guarantees JSON-RPC
    // frames never collide with diagnostics.
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(out, "{json}").context("failed to write CSL JSON to stdout")?;
    Ok(())
}

/// One CSL JSON 1.0 item, scoped to the binding fields the local
/// `Metadata` schema can populate.
///
/// Field order is the citeproc-js conventional order (id, type, title,
/// author, issued, identifiers, container, publisher) so a human
/// diffing two outputs sees a stable column layout.
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

/// CSL name-variable shape: `{ "family": "...", "given": "..." }`.
///
/// Empty halves are omitted so a single-token name like `"Plato"` lands
/// as `{"family": "Plato"}` rather than `{"family": "Plato", "given": ""}`,
/// which citeproc-js renders with a stray comma.
#[derive(Debug, Serialize)]
struct CslName {
    #[serde(skip_serializing_if = "String::is_empty")]
    family: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    given: String,
}

/// CSL date-variable shape, year-only for Phase 1.
///
/// `date-parts` is a list-of-lists: outer list holds 1 or 2 entries
/// (single date or range), inner list is `[year, month?, day?]`. We only
/// know the year, so the inner list is `[<year>]`.
#[derive(Debug, Serialize)]
struct CslIssued {
    #[serde(rename = "date-parts")]
    date_parts: Vec<Vec<i32>>,
}

/// Build a [`CslItem`] from a stored [`Metadata`].
///
/// Type mapping is intentionally narrow: Crossref's `journal-article`
/// becomes CSL's `article-journal`. Everything else falls back to
/// `manuscript`, which citeproc-js renders sensibly without forcing
/// a container. Phase 2 will expand the table to cover `book-chapter`,
/// `proceedings-article`, etc.
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
/// Heuristic, Phase 1:
///
/// - If the name contains a comma, it is `Family, Given` form: split on
///   the first comma; left side is family, right side is given.
/// - Otherwise, split on the LAST whitespace: left is given, right is
///   family. (`"Alice Researcher"` → family `"Researcher"`,
///   given `"Alice"`.) This is the convention citeproc-js itself uses
///   for unparsed string names.
/// - If neither rule applies (single token), the whole string is the
///   family name and `given` is empty.
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
