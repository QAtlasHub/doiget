//! Bibliography input adapters per ADR-0030.
//!
//! Parses three input shapes into an iterator of `Ref`s with optional
//! `entry_key` provenance back to the source bibliography:
//!
//! - **Plain refs**: one `doi:…` / `arxiv:…` / bare-DOI / bare-arXiv id
//!   per line, with `#`-prefixed comments and blank lines tolerated.
//!   The existing `doiget batch <refs.txt>` shape.
//! - **CSL-JSON**: a JSON array of entries with `id` (citation key),
//!   `DOI`, and optionally `archivePrefix = "arXiv"` + `eprint`
//!   fields. Parsed via the workspace's existing `serde_json` — no
//!   new dependency.
//! - **BibTeX / BibLaTeX (.bib)**: parsed via the `biblatex` crate
//!   (ADR-0030 D2). One `@entrytype{KEY, …}` per entry; the `doi`
//!   field is preferred, falling back to an arXiv `eprint`.
//!
//! Identifier-pick priority per ADR-0030 D3: `doi` > `arxiv` > `pmid`
//! (PMID adapter parking until the `Ref::Pmid` variant lands in a
//! later slice; current code carries the rule through without
//! producing a `Pmid` ref).
//!
//! Parse-error policy per ADR-0030 D5: a single entry's failure is
//! captured per-entry and does NOT abort the whole batch. The caller
//! decides whether to skip-and-warn (default) or fail-closed
//! (`--strict`).

use biblatex::{Bibliography, ChunksExt};
use camino::Utf8Path;
use thiserror::Error;

use crate::{Ref, RefParseError};

/// One successfully-parsed bibliography entry.
///
/// `entry_key` echoes the source bibliography's citation key
/// (BibTeX `@article{KEY,…}` / CSL-JSON `"id"`) so downstream
/// automation can bridge the fetch outcome back to the originating
/// reference — the load-bearing field for the Zotero / Mendeley
/// "attach fetched PDF to this reference" workflow per ADR-0030 §6.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ParsedEntry {
    /// The identifier the adapter chose for this entry (`Ref::Doi` /
    /// `Ref::Arxiv`).
    pub ref_: Ref,
    /// The source bibliography's citation key, when one is available.
    /// `None` for plain-refs input (no key concept) and for any
    /// future input shape that lacks per-entry keys.
    pub entry_key: Option<String>,
}

/// Why a single bibliography entry failed to produce a `Ref`.
///
/// Closed-enum so the failure-class can be exposed at the
/// `docs/ERRORS.md` §3 INVALID_REF surface without leaking parser
/// internals.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum ParseError {
    /// The line did not contain a `doi:` / `arxiv:` / bare-DOI /
    /// bare-arXiv id — empty (after trimming) or just a comment.
    /// Plain-refs path filters these out silently; CSL-JSON path
    /// emits this when an entry has no resolvable identifier.
    #[error("entry has no DOI / arXiv id (entry_key={entry_key:?})")]
    NoIdentifier {
        /// The source bibliography's citation key, when known.
        entry_key: Option<String>,
    },
    /// The entry DOES carry an identifier, and it is one doiget recognises
    /// and cannot resolve yet (#500).
    ///
    /// Distinct from [`Self::NoIdentifier`] because the two send a reader in
    /// opposite directions. "entry has no DOI / arXiv id" is accurate about
    /// what the parser did and wrong about the entry: a PubMed-exported
    /// `.bib` record carrying `pmid = {9659853}` is not deficient, and a user
    /// who believes it is will go and edit a bibliography that was fine. The
    /// missing piece is on doiget's side.
    ///
    /// Surfaces as `NOT_IMPLEMENTED` rather than `INVALID_REF`: the input is
    /// valid and the support is absent, and the two carry different advice --
    /// "wait for a release" versus "correct your input" (ADR-0055).
    #[error("entry {entry_key:?} is identified only by {kind} {value:?}, which doiget cannot resolve yet (issue #500) -- it is NOT missing an identifier")]
    UnsupportedIdentifier {
        /// Human-facing name of the identifier class, e.g. `"PMID"`.
        kind: &'static str,
        /// The identifier as written in the entry.
        value: String,
        /// The source bibliography's citation key, when known.
        entry_key: Option<String>,
    },
    /// The identifier was present but `Ref::parse` rejected it
    /// (malformed DOI suffix, invalid arXiv id shape, etc.).
    #[error(
        "entry identifier {raw:?} did not parse as a Ref \
         (entry_key={entry_key:?}): {source}"
    )]
    InvalidRef {
        /// The raw identifier string the parser saw.
        raw: String,
        /// The source bibliography's citation key, when known.
        entry_key: Option<String>,
        /// The structured `Ref::parse` failure.
        #[source]
        source: RefParseError,
    },
    /// The whole input did not deserialise — CSL-JSON that is not a
    /// JSON array, top-level malformed JSON, etc. This is a
    /// whole-input failure, not a per-entry failure; callers receive
    /// it as the sole `Err` element of the result iterator.
    #[error("input did not deserialise as {format}: {message}")]
    Decode {
        /// Which parser branch produced the failure (`"csl-json"` /
        /// `"bibtex"`).
        format: &'static str,
        /// `serde_json::Error::to_string()`.
        message: String,
    },
    /// Format requested or detected, but no parser for it is shipped.
    /// Retained as a forward-compatible variant for input shapes not
    /// yet implemented (e.g. a future RIS adapter); the `bibtex` and
    /// `csl-json` paths are both live today.
    #[error("{format} parsing is not yet implemented")]
    UnsupportedFormat {
        /// The format token naming the unsupported shape.
        format: &'static str,
    },
}

/// The claim [`ParseError::UnsupportedIdentifier`] makes, without the
/// `entry {entry_key:?}` prefix its `Display` carries.
///
/// Callers that put `entry_key` in a field of its own -- the CLI `verify`
/// row and the MCP `batch_from_bibliography` envelope both do -- would
/// otherwise say it twice. One definition rather than a copy at each site,
/// because the copies drifted: two of the three carried a run of joined-line
/// whitespace into user-facing output before anything asserted the text.
#[must_use]
pub fn unsupported_identifier_claim(kind: &str, value: &str) -> String {
    format!("entry is identified only by {kind} {value:?}, which doiget cannot resolve yet (issue #500); it is NOT missing an identifier")
}

/// Input-shape discriminator per ADR-0030 D4.
///
/// `Auto` means "detect from path extension and/or content
/// fingerprint"; the explicit variants name a parser directly and
/// skip detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Format {
    /// Detect from file extension if a path was supplied, else from
    /// content fingerprint; fall through to [`Format::Refs`].
    Auto,
    /// Plain refs — one identifier per line, `#` comments, blanks.
    Refs,
    /// CSL-JSON array per <https://citationstyles.org/>.
    CslJson,
    /// BibTeX / BibLaTeX, parsed via the `biblatex` crate (ADR-0030 D2).
    Bibtex,
}

impl Format {
    /// Wire token used by the CLI `--format` flag and the MCP tool
    /// input schema's `format` field per ADR-0030 §6.
    pub fn as_wire(&self) -> &'static str {
        match self {
            Format::Auto => "auto",
            Format::Refs => "refs",
            Format::CslJson => "csl-json",
            Format::Bibtex => "bibtex",
        }
    }
}

/// Detect the input format per ADR-0030 D4.
///
/// Precedence: file extension first (when `path` is `Some`), then
/// content fingerprint, then fallback to [`Format::Refs`]. The
/// caller's explicit `--format` flag should short-circuit this
/// function — it is the slowest of the three precedence rules in the
/// ADR.
pub fn detect_format(path: Option<&Utf8Path>, content: &str) -> Format {
    if let Some(p) = path {
        let ext = p.extension().unwrap_or_default().to_ascii_lowercase();
        match ext.as_str() {
            "bib" | "biblatex" => return Format::Bibtex,
            "json" | "csl" => return Format::CslJson,
            _ => {}
        }
    }
    // Content fingerprint: peek the first non-blank, non-comment line.
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed.starts_with('@') {
            return Format::Bibtex;
        }
        if trimmed.starts_with('[') || trimmed.starts_with('{') {
            return Format::CslJson;
        }
        break;
    }
    Format::Refs
}

/// Parse `text` per `format`, dispatching to the matching shape
/// parser. `path` is consulted only when `format == Format::Auto` to
/// drive [`detect_format`].
///
/// Returns one element per discovered entry — `Ok` for entries that
/// produced a `Ref`, `Err` for per-entry failures the caller should
/// surface as a JSONL `INVALID_REF` line. A whole-input decode
/// failure ([`ParseError::Decode`]) is returned as a single-element
/// `Err` so the caller's exit-code path treats it as one parse error
/// rather than zero.
pub fn parse_input(
    text: &str,
    format: Format,
    path: Option<&Utf8Path>,
) -> Vec<Result<ParsedEntry, ParseError>> {
    let resolved = match format {
        Format::Auto => detect_format(path, text),
        other => other,
    };
    match resolved {
        Format::Refs | Format::Auto => parse_plain_refs(text),
        Format::CslJson => parse_csl_json(text),
        Format::Bibtex => parse_bibtex(text),
    }
}

/// Parse plain refs — the existing batch input format. One ref per
/// non-blank, non-comment line. `entry_key` is always `None` for this
/// shape; plain refs have no citation-key concept.
pub fn parse_plain_refs(text: &str) -> Vec<Result<ParsedEntry, ParseError>> {
    let mut out = Vec::new();
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        out.push(match Ref::parse(line) {
            Ok(ref_) => Ok(ParsedEntry {
                ref_,
                entry_key: None,
            }),
            Err(e) => Err(ParseError::InvalidRef {
                raw: line.to_string(),
                entry_key: None,
                source: e,
            }),
        });
    }
    out
}

/// Parse a CSL-JSON document — a JSON array of objects, each with at
/// least an `id` (citation key) and one of `DOI`, or `archivePrefix`
/// + `eprint` (arXiv).
///
/// Identifier-pick priority per ADR-0030 D3:
///
/// 1. `DOI` field (case-sensitive per the CSL-JSON spec but Zotero
///    sometimes emits `doi` lowercase — we accept both).
/// 2. `archivePrefix == "arXiv"` (case-insensitive) + `eprint`
///    (or `note: "arXiv:..."` shape Zotero emits).
/// 3. (PMID parking — `Ref::Pmid` not yet defined; PMIDs in CSL-JSON
///    are recorded as parse failures with `NoIdentifier` until the
///    variant lands.)
///
/// `entry_key` is the `id` field verbatim.
pub fn parse_csl_json(text: &str) -> Vec<Result<ParsedEntry, ParseError>> {
    let parsed: serde_json::Result<Vec<serde_json::Value>> = serde_json::from_str(text);
    let entries = match parsed {
        Ok(arr) => arr,
        Err(e) => {
            return vec![Err(ParseError::Decode {
                format: "csl-json",
                message: e.to_string(),
            })]
        }
    };
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        // `id` is usually a string in real-world Zotero exports but
        // the spec allows numeric ids too — stringify either form so
        // the operator can find the entry in their library.
        let entry_key = entry.get("id").and_then(|v| {
            if let Some(s) = v.as_str() {
                Some(s.to_string())
            } else if v.is_number() {
                Some(v.to_string())
            } else {
                None
            }
        });
        out.push(parse_csl_entry(&entry, entry_key));
    }
    out
}

/// Pick the highest-priority identifier on a single CSL-JSON entry
/// and parse it. Honors ADR-0030 D3 priority.
fn parse_csl_entry(
    entry: &serde_json::Value,
    entry_key: Option<String>,
) -> Result<ParsedEntry, ParseError> {
    // Priority 1: DOI (both `DOI` per spec and `doi` lowercase per
    // real-world exports). Zotero emits uppercase; Mendeley sometimes
    // lowercase.
    if let Some(doi) = entry
        .get("DOI")
        .or_else(|| entry.get("doi"))
        .and_then(|v| v.as_str())
    {
        let raw = doi.trim();
        if !raw.is_empty() {
            return match Ref::parse(raw) {
                Ok(ref_) => Ok(ParsedEntry { ref_, entry_key }),
                Err(e) => Err(ParseError::InvalidRef {
                    raw: raw.to_string(),
                    entry_key,
                    source: e,
                }),
            };
        }
    }
    // Priority 2: arXiv — `archivePrefix == "arXiv"` (CSL extension)
    // OR the Zotero-specific `note: "arXiv:..."` shape.
    let is_arxiv = entry
        .get("archivePrefix")
        .or_else(|| entry.get("archive_prefix"))
        .and_then(|v| v.as_str())
        .map(|s| s.eq_ignore_ascii_case("arxiv"))
        .unwrap_or(false);
    if is_arxiv {
        if let Some(eprint) = entry.get("eprint").and_then(|v| v.as_str()) {
            let raw = eprint.trim();
            if !raw.is_empty() {
                let with_scheme = if raw.to_ascii_lowercase().starts_with("arxiv:") {
                    raw.to_string()
                } else {
                    format!("arxiv:{raw}")
                };
                return match Ref::parse(&with_scheme) {
                    Ok(ref_) => Ok(ParsedEntry { ref_, entry_key }),
                    Err(e) => Err(ParseError::InvalidRef {
                        raw: with_scheme,
                        entry_key,
                        source: e,
                    }),
                };
            }
        }
    }
    // Fallback: scan `note` for an embedded `arXiv:NNNN.NNNNN` —
    // Zotero often stores the arXiv id there instead of a typed
    // field. The pattern is intentionally narrow (must follow the
    // canonical "arXiv:" prefix); free-text DOIs in notes are NOT
    // mined here.
    if let Some(note) = entry.get("note").and_then(|v| v.as_str()) {
        if let Some(idx) = note.to_ascii_lowercase().find("arxiv:") {
            let tail = &note[idx + "arxiv:".len()..];
            // Take chars matching the arXiv id alphabet (digits / dot /
            // slash / letters / hyphen) — stop at the first separator
            // so the rest of the note is ignored.
            let id: String = tail
                .chars()
                .take_while(|c| matches!(c, '0'..='9' | '.' | '/' | 'a'..='z' | 'A'..='Z' | '-'))
                .collect();
            if !id.is_empty() {
                let with_scheme = format!("arxiv:{id}");
                return match Ref::parse(&with_scheme) {
                    Ok(ref_) => Ok(ParsedEntry { ref_, entry_key }),
                    Err(e) => Err(ParseError::InvalidRef {
                        raw: with_scheme,
                        entry_key,
                        source: e,
                    }),
                };
            }
        }
    }
    Err(ParseError::NoIdentifier { entry_key })
}

/// Parse a BibTeX / BibLaTeX document via the `biblatex` crate
/// (ADR-0030 D2). One `@entrytype{KEY, …}` produces one entry;
/// `entry_key` is the citation key verbatim.
///
/// A whole-input parse failure (malformed BibTeX the `biblatex` crate
/// rejects) is returned as a single-element [`ParseError::Decode`] so
/// the caller's exit-code path counts it as one error rather than
/// zero — matching the CSL-JSON behaviour.
///
/// Identifier-pick priority per ADR-0030 D3: `doi` field, then
/// `eprint` (arXiv). See `parse_bibtex_entry`.
pub fn parse_bibtex(text: &str) -> Vec<Result<ParsedEntry, ParseError>> {
    let bib = match Bibliography::parse(text) {
        Ok(b) => b,
        Err(e) => {
            return vec![Err(ParseError::Decode {
                format: "bibtex",
                message: e.to_string(),
            })]
        }
    };
    bib.iter()
        .map(|entry| parse_bibtex_entry(entry, Some(entry.key.clone())))
        .collect()
}

/// Pick the highest-priority identifier on a single BibTeX entry and
/// parse it. Honors ADR-0030 D3 priority (`doi` > arXiv `eprint`).
fn parse_bibtex_entry(
    entry: &biblatex::Entry,
    entry_key: Option<String>,
) -> Result<ParsedEntry, ParseError> {
    // Priority 1: `doi` field. The typed accessor formats the chunk
    // value to a `String`; `Err` means the field is absent or not a
    // plain string, which we treat as "no DOI here" and fall through.
    if let Ok(doi) = entry.doi() {
        let raw = doi.trim();
        if !raw.is_empty() {
            return match Ref::parse(raw) {
                Ok(ref_) => Ok(ParsedEntry { ref_, entry_key }),
                Err(e) => Err(ParseError::InvalidRef {
                    raw: raw.to_string(),
                    entry_key,
                    source: e,
                }),
            };
        }
    }
    // Priority 2: arXiv via the `eprint` field. The BibTeX convention
    // is `eprint = {2204.12345}` with `archivePrefix = {arXiv}` (or the
    // BibLaTeX `eprinttype = {arxiv}`). We accept the eprint as an arXiv
    // id when the prefix names arXiv OR is absent (the dominant
    // single-preprint-server convention); a prefix that names something
    // else (e.g. `eprinttype = {pubmed}`) is skipped rather than
    // parsed incorrectly.
    if let Ok(eprint) = entry.eprint() {
        let raw = eprint.trim();
        if !raw.is_empty() && arxiv_eligible(entry) {
            let with_scheme = if raw.to_ascii_lowercase().starts_with("arxiv:") {
                raw.to_string()
            } else {
                format!("arxiv:{raw}")
            };
            return match Ref::parse(&with_scheme) {
                Ok(ref_) => Ok(ParsedEntry { ref_, entry_key }),
                Err(e) => Err(ParseError::InvalidRef {
                    raw: with_scheme,
                    entry_key,
                    source: e,
                }),
            };
        }
    }
    // #500: before reporting "no identifier", check for one doiget simply
    // does not support. Saying "no DOI / arXiv id" about an entry that
    // carries a PMID is accurate about the parser and wrong about the entry.
    if let Some((kind, value)) = unsupported_identifier(entry) {
        return Err(ParseError::UnsupportedIdentifier {
            kind,
            value,
            entry_key,
        });
    }
    Err(ParseError::NoIdentifier { entry_key })
}

/// An identifier doiget recognises but cannot resolve yet (#500).
///
/// Only classes doiget can *name*. An entry carrying some field this does not
/// know about still reports [`ParseError::NoIdentifier`], which stays correct
/// for it: the point is not to guess, it is to stop saying "no identifier"
/// about the cases where there demonstrably is one.
///
/// `pmid = {...}` is what PubMed's own BibTeX export writes. The BibLaTeX
/// shape is `eprint = {...}` with `eprinttype = {pubmed}`, which
/// [`arxiv_eligible`] already refuses -- correctly, and until now silently.
fn unsupported_identifier(entry: &biblatex::Entry) -> Option<(&'static str, String)> {
    let field = |name: &str| -> Option<String> {
        let v = entry.get(name)?.format_verbatim().trim().to_string();
        (!v.is_empty()).then_some(v)
    };

    if let Some(v) = field("pmid") {
        return Some(("PMID", v));
    }
    if let Some(v) = field("pmcid") {
        return Some(("PMCID", v));
    }
    let names_pubmed = entry
        .get("archiveprefix")
        .or_else(|| entry.get("eprinttype"))
        .is_some_and(|c| c.format_verbatim().to_ascii_lowercase().contains("pubmed"));
    if names_pubmed {
        if let Some(v) = field("eprint") {
            return Some(("PMID", v));
        }
    }
    None
}

/// Whether an `eprint` field should be interpreted as an arXiv id.
/// True when `archivePrefix` / `eprinttype` names arXiv (case-
/// insensitive) or is absent; false when it explicitly names a
/// different preprint server.
fn arxiv_eligible(entry: &biblatex::Entry) -> bool {
    match entry
        .get("archiveprefix")
        .or_else(|| entry.get("eprinttype"))
    {
        Some(chunks) => chunks
            .format_verbatim()
            .to_ascii_lowercase()
            .contains("arxiv"),
        None => true,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    // ---- detect_format ---------------------------------------------

    /// #500: the entry from PubMed's own BibTeX export. It carries `pmid`,
    /// and reporting "entry has no DOI / arXiv id" about it is accurate about
    /// the parser and wrong about the entry -- a user who believes it goes and
    /// edits a bibliography that was fine.
    ///
    /// The PMID is real: `9659853` is Coryell 1998, whose DOI
    /// `10.1176/ajp.155.7.895` NCBI's own esummary returns for it.
    #[test]
    fn a_pubmed_only_entry_says_it_has_a_pmid_not_that_it_has_nothing() {
        let bib = r#"@article{coryell1998,
  title = {Lithium discontinuation and subsequent effectiveness},
  author = {Coryell, William},
  year = {1998},
  pmid = {9659853},
}"#;
        let out = parse_bibtex(bib);
        assert_eq!(out.len(), 1);
        match &out[0] {
            Err(ParseError::UnsupportedIdentifier {
                kind,
                value,
                entry_key,
            }) => {
                assert_eq!(kind, &"PMID");
                assert_eq!(value, "9659853");
                assert_eq!(entry_key.as_deref(), Some("coryell1998"));
                let msg = out[0].as_ref().unwrap_err().to_string();
                assert!(
                    msg.contains("NOT missing an identifier"),
                    "the message has to contradict the wrong conclusion explicitly, or the reader draws it anyway: {msg}"
                );
            }
            other => panic!("expected UnsupportedIdentifier, got {other:?}"),
        }
    }

    /// The BibLaTeX shape. `arxiv_eligible` already refused this -- correctly,
    /// and until now silently, which is the whole complaint.
    #[test]
    fn the_biblatex_eprinttype_pubmed_shape_is_recognised_too() {
        let bib = r#"@article{e,
  title = {T},
  eprint = {9659853},
  eprinttype = {pubmed},
}"#;
        let out = parse_bibtex(bib);
        assert!(
            matches!(
                &out[0],
                Err(ParseError::UnsupportedIdentifier { kind: "PMID", .. })
            ),
            "got {:?}",
            out[0]
        );
    }

    /// An entry with genuinely nothing still reports `NoIdentifier`. The point
    /// is not to relabel every failure -- it is to stop saying "no identifier"
    /// about the cases where there demonstrably is one.
    #[test]
    fn an_entry_with_no_identifier_at_all_is_unchanged() {
        let bib = "@article{x,
  title = {T},
  year = {2020},
}";
        let out = parse_bibtex(bib);
        assert!(
            matches!(&out[0], Err(ParseError::NoIdentifier { .. })),
            "got {:?}",
            out[0]
        );
    }

    /// A DOI still wins. The new check runs only after every supported
    /// identifier has been tried, so adding it cannot divert an entry doiget
    /// could actually have resolved.
    #[test]
    fn a_doi_alongside_a_pmid_still_resolves() {
        let bib = r#"@article{both,
  title = {T},
  doi = {10.1176/ajp.155.7.895},
  pmid = {9659853},
}"#;
        let out = parse_bibtex(bib);
        let parsed = out[0].as_ref().expect("the DOI must still win");
        assert_eq!(parsed.ref_.as_input_str(), "10.1176/ajp.155.7.895");
    }

    #[test]
    fn detect_by_bib_extension() {
        let p = Utf8Path::new("/tmp/library.bib");
        assert_eq!(detect_format(Some(p), ""), Format::Bibtex);
    }

    #[test]
    fn detect_by_json_extension() {
        let p = Utf8Path::new("/tmp/library.json");
        assert_eq!(detect_format(Some(p), ""), Format::CslJson);
    }

    #[test]
    fn detect_by_csl_extension() {
        let p = Utf8Path::new("/tmp/library.csl");
        assert_eq!(detect_format(Some(p), ""), Format::CslJson);
    }

    #[test]
    fn detect_by_fingerprint_bibtex_at_sign() {
        let body = "# comment\n\n@article{foo,\n  doi = {10.1/x}\n}\n";
        assert_eq!(detect_format(None, body), Format::Bibtex);
    }

    #[test]
    fn detect_by_fingerprint_csl_json_array() {
        let body = "[{\"id\":\"foo\",\"DOI\":\"10.1/x\"}]";
        assert_eq!(detect_format(None, body), Format::CslJson);
    }

    #[test]
    fn detect_by_fingerprint_falls_through_to_refs() {
        let body = "doi:10.1234/foo\narxiv:2401.12345\n";
        assert_eq!(detect_format(None, body), Format::Refs);
    }

    // ---- plain refs ------------------------------------------------

    #[test]
    fn plain_refs_parses_mix_with_comments_and_blanks() {
        let body = "\
# header comment
doi:10.1234/foo

   arxiv:2401.12345
# trailing comment
";
        let parsed = parse_plain_refs(body);
        assert_eq!(parsed.len(), 2);
        let okays: Vec<_> = parsed.into_iter().filter_map(Result::ok).collect();
        assert!(matches!(okays[0].ref_, Ref::Doi(_)));
        assert!(matches!(okays[1].ref_, Ref::Arxiv(_)));
        assert!(okays.iter().all(|e| e.entry_key.is_none()));
    }

    #[test]
    fn plain_refs_surface_per_line_invalid_refs() {
        let body = "doi:10.1234/foo\nnot-a-ref\narxiv:2401.12345\n";
        let parsed = parse_plain_refs(body);
        assert_eq!(parsed.len(), 3);
        assert!(parsed[0].is_ok());
        assert!(matches!(parsed[1], Err(ParseError::InvalidRef { .. })));
        assert!(parsed[2].is_ok());
    }

    // ---- CSL-JSON --------------------------------------------------

    #[test]
    fn csl_json_picks_doi_when_present() {
        let body = r#"[{"id":"foo2024","DOI":"10.1234/foo"}]"#;
        let parsed = parse_csl_json(body);
        assert_eq!(parsed.len(), 1);
        let entry = parsed.into_iter().next().unwrap().expect("entry parses");
        assert!(matches!(entry.ref_, Ref::Doi(_)));
        assert_eq!(entry.entry_key.as_deref(), Some("foo2024"));
    }

    #[test]
    fn csl_json_accepts_lowercase_doi_field() {
        // Mendeley exports sometimes lowercase the field name.
        let body = r#"[{"id":"x","doi":"10.5555/bar"}]"#;
        let parsed = parse_csl_json(body);
        let entry = parsed.into_iter().next().unwrap().expect("entry parses");
        assert!(matches!(entry.ref_, Ref::Doi(_)));
    }

    #[test]
    fn csl_json_picks_arxiv_via_archive_prefix_and_eprint() {
        let body = r#"[{"id":"arx","archivePrefix":"arXiv","eprint":"2401.12345"}]"#;
        let parsed = parse_csl_json(body);
        let entry = parsed.into_iter().next().unwrap().expect("entry parses");
        assert!(matches!(entry.ref_, Ref::Arxiv(_)));
    }

    #[test]
    fn csl_json_arxiv_archive_prefix_is_case_insensitive() {
        let body = r#"[{"id":"arx","archivePrefix":"ARXIV","eprint":"2401.12345"}]"#;
        let parsed = parse_csl_json(body);
        let entry = parsed.into_iter().next().unwrap().expect("entry parses");
        assert!(matches!(entry.ref_, Ref::Arxiv(_)));
    }

    #[test]
    fn csl_json_doi_beats_arxiv_when_both_present() {
        // ADR-0030 D3: priority is DOI > arXiv > PMID.
        let body = r#"[{
            "id":"both",
            "DOI":"10.1234/foo",
            "archivePrefix":"arXiv",
            "eprint":"2401.12345"
        }]"#;
        let parsed = parse_csl_json(body);
        let entry = parsed.into_iter().next().unwrap().expect("entry parses");
        assert!(matches!(entry.ref_, Ref::Doi(_)));
    }

    #[test]
    fn csl_json_arxiv_from_note_field() {
        // Zotero often dumps "arXiv:NNNN.NNNNN" into the note field
        // instead of a typed field.
        let body = r#"[{"id":"znote","note":"Comment: 12 pages. arXiv:2401.12345"}]"#;
        let parsed = parse_csl_json(body);
        let entry = parsed.into_iter().next().unwrap().expect("entry parses");
        assert!(matches!(entry.ref_, Ref::Arxiv(_)));
    }

    #[test]
    fn csl_json_entry_without_any_identifier_yields_no_identifier_error() {
        let body = r#"[{"id":"empty","title":"no ids here"}]"#;
        let parsed = parse_csl_json(body);
        assert!(matches!(
            parsed.into_iter().next().unwrap(),
            Err(ParseError::NoIdentifier { .. })
        ));
    }

    #[test]
    fn csl_json_invalid_doi_surface_as_invalid_ref_per_entry() {
        let body = r#"[{"id":"bad","DOI":"not-a-doi"}]"#;
        let parsed = parse_csl_json(body);
        match &parsed[0] {
            Err(ParseError::InvalidRef { raw, entry_key, .. }) => {
                assert_eq!(raw, "not-a-doi");
                assert_eq!(entry_key.as_deref(), Some("bad"));
            }
            other => panic!("expected InvalidRef, got {other:?}"),
        }
    }

    #[test]
    fn csl_json_top_level_malformed_yields_single_decode_error() {
        let body = "{this is not JSON}";
        let parsed = parse_csl_json(body);
        assert_eq!(parsed.len(), 1);
        assert!(matches!(
            parsed[0],
            Err(ParseError::Decode {
                format: "csl-json",
                ..
            })
        ));
    }

    #[test]
    fn csl_json_non_array_top_level_yields_decode_error() {
        // A single-entry object (not an array) is not a valid CSL-JSON
        // document by the spec — the top level MUST be an array even
        // for a single entry.
        let body = r#"{"id":"x","DOI":"10.1/x"}"#;
        let parsed = parse_csl_json(body);
        assert!(matches!(
            parsed[0],
            Err(ParseError::Decode {
                format: "csl-json",
                ..
            })
        ));
    }

    // ---- parse_input dispatch -------------------------------------

    #[test]
    fn parse_input_auto_dispatches_csl_json_by_content() {
        let body = r#"[{"id":"foo","DOI":"10.1234/foo"}]"#;
        let parsed = parse_input(body, Format::Auto, None);
        assert_eq!(parsed.len(), 1);
        assert!(matches!(
            parsed[0],
            Ok(ParsedEntry {
                ref_: Ref::Doi(_),
                ..
            })
        ));
    }

    #[test]
    fn parse_input_auto_dispatches_refs_by_content() {
        let body = "doi:10.1234/foo\n";
        let parsed = parse_input(body, Format::Auto, None);
        assert_eq!(parsed.len(), 1);
        assert!(matches!(
            parsed[0],
            Ok(ParsedEntry {
                ref_: Ref::Doi(_),
                ..
            })
        ));
    }

    // ---- BibTeX parsing (ADR-0030 D2) -----------------------------

    #[test]
    fn bibtex_picks_doi_and_preserves_key() {
        let body = r#"@article{Onsager1944,
            author = {Onsager, Lars},
            title  = {Crystal Statistics},
            doi    = {10.1103/PhysRev.65.117}
        }"#;
        let parsed = parse_bibtex(body);
        assert_eq!(parsed.len(), 1);
        let entry = parsed.into_iter().next().unwrap().expect("entry parses");
        assert!(matches!(entry.ref_, Ref::Doi(_)));
        assert_eq!(entry.entry_key.as_deref(), Some("Onsager1944"));
    }

    #[test]
    fn bibtex_picks_arxiv_via_eprint() {
        let body = r#"@article{Pollmann2012,
            title         = {Detection of SPT order},
            eprint        = {1010.3732},
            archivePrefix = {arXiv}
        }"#;
        let entry = parse_bibtex(body)
            .into_iter()
            .next()
            .unwrap()
            .expect("entry parses");
        assert!(matches!(entry.ref_, Ref::Arxiv(_)));
    }

    #[test]
    fn bibtex_bare_eprint_without_prefix_is_arxiv() {
        // The dominant convention: a lone `eprint` field is an arXiv id.
        let body = "@misc{x, eprint = {2204.12345}}";
        let entry = parse_bibtex(body)
            .into_iter()
            .next()
            .unwrap()
            .expect("entry parses");
        assert!(matches!(entry.ref_, Ref::Arxiv(_)));
    }

    #[test]
    fn bibtex_non_arxiv_eprinttype_reports_the_identifier_it_found() {
        // This test used to assert `NoIdentifier`, with the comment "the entry
        // has no resolvable identifier". The entry has a PMID. It is not
        // resolvable BY DOIGET, which is a different statement, and the one
        // #500 is about -- so the test was pinning the wrong claim.
        let body = "@article{x, eprint = {12345678}, eprinttype = {pubmed}}";
        let res = parse_bibtex(body).into_iter().next().unwrap();
        match res {
            Err(ParseError::UnsupportedIdentifier { kind, value, .. }) => {
                assert_eq!(kind, "PMID");
                assert_eq!(value, "12345678");
            }
            other => panic!("expected UnsupportedIdentifier, got {other:?}"),
        }
    }

    #[test]
    fn bibtex_doi_beats_arxiv_when_both_present() {
        // ADR-0030 D3: priority is DOI > arXiv > PMID.
        let body = r#"@article{both,
            doi           = {10.1234/foo},
            eprint        = {2401.12345},
            archivePrefix = {arXiv}
        }"#;
        let entry = parse_bibtex(body)
            .into_iter()
            .next()
            .unwrap()
            .expect("entry parses");
        assert!(matches!(entry.ref_, Ref::Doi(_)));
    }

    #[test]
    fn bibtex_multiple_entries_each_yield_a_result() {
        let body = r#"
            @article{a, doi = {10.1103/PhysRev.65.117}}
            @article{b, eprint = {1010.3732}, archivePrefix = {arXiv}}
            @article{c, title = {no identifier here}}
        "#;
        let parsed = parse_bibtex(body);
        assert_eq!(parsed.len(), 3);
        assert!(matches!(
            parsed[0],
            Ok(ParsedEntry {
                ref_: Ref::Doi(_),
                ..
            })
        ));
        assert!(matches!(
            parsed[1],
            Ok(ParsedEntry {
                ref_: Ref::Arxiv(_),
                ..
            })
        ));
        assert!(matches!(parsed[2], Err(ParseError::NoIdentifier { .. })));
    }

    #[test]
    fn bibtex_entry_without_identifier_yields_no_identifier_error() {
        let body = "@book{nodoi, title = {A Book}, author = {Author, A.}}";
        let res = parse_bibtex(body).into_iter().next().unwrap();
        assert!(matches!(res, Err(ParseError::NoIdentifier { .. })));
    }

    #[test]
    fn bibtex_invalid_doi_surfaces_as_invalid_ref_per_entry() {
        let body = "@article{bad, doi = {not-a-doi}}";
        let res = parse_bibtex(body).into_iter().next().unwrap();
        assert!(matches!(res, Err(ParseError::InvalidRef { .. })));
    }

    #[test]
    fn bibtex_malformed_input_yields_single_decode_error() {
        // A truncated entry the biblatex parser rejects outright.
        let body = "@article{unterminated, doi = {10.1234/x}";
        let parsed = parse_bibtex(body);
        assert_eq!(parsed.len(), 1);
        assert!(matches!(
            parsed[0],
            Err(ParseError::Decode {
                format: "bibtex",
                ..
            })
        ));
    }

    #[test]
    fn parse_input_bibtex_dispatches_and_parses() {
        let body = "@article{foo, doi = {10.1234/foo}}";
        let parsed = parse_input(body, Format::Bibtex, None);
        assert_eq!(parsed.len(), 1);
        let entry = parsed.into_iter().next().unwrap().expect("entry parses");
        assert!(matches!(entry.ref_, Ref::Doi(_)));
        assert_eq!(entry.entry_key.as_deref(), Some("foo"));
    }

    #[test]
    fn parse_input_auto_dispatches_bibtex_by_content() {
        // Leading `@article{` fingerprint routes to the BibTeX parser.
        let body = "@article{auto, doi = {10.1234/auto}}";
        let parsed = parse_input(body, Format::Auto, None);
        let entry = parsed.into_iter().next().unwrap().expect("entry parses");
        assert!(matches!(entry.ref_, Ref::Doi(_)));
    }

    #[test]
    fn parse_input_auto_with_path_uses_extension() {
        let body = "[]";
        let parsed = parse_input(body, Format::Auto, Some(Utf8Path::new("foo.csl")));
        assert_eq!(
            parsed.len(),
            0,
            "empty array yields zero entries: {parsed:?}"
        );
    }

    // ---- Format::as_wire ------------------------------------------

    #[test]
    fn format_wire_strings_are_stable() {
        // Pinned because the strings appear in the CLI --format flag,
        // the MCP tool input schema, and the JSON-Lines parse-error
        // records (ADR-0030 §6). A drift would be a wire-format break.
        assert_eq!(Format::Auto.as_wire(), "auto");
        assert_eq!(Format::Refs.as_wire(), "refs");
        assert_eq!(Format::CslJson.as_wire(), "csl-json");
        assert_eq!(Format::Bibtex.as_wire(), "bibtex");
    }

    #[test]
    fn the_unsupported_identifier_claim_denies_the_wrong_reading() {
        // #500's whole point: the sentence must put the gap on doiget's
        // side. A reader who takes "no identifier" at face value goes and
        // edits a `.bib` that was fine.
        let msg = unsupported_identifier_claim("PMID", "9659853");
        assert!(msg.contains("PMID"), "names the identifier kind: {msg}");
        assert!(msg.contains("9659853"), "quotes the value: {msg}");
        assert!(
            msg.contains("NOT missing an identifier"),
            "denies the wrong reading: {msg}"
        );
        assert!(msg.contains("#500"), "points at the issue: {msg}");
        // The `entry_key` prefix belongs to `Display`, not here -- callers
        // carry it in a field of its own and would say it twice.
        assert!(!msg.starts_with("entry {"), "no entry_key prefix: {msg}");
    }
}
