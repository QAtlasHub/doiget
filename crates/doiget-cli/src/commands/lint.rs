//! `doiget lint <path>` — structural validation of a BibTeX bibliography,
//! independent of DOI resolution (`doiget verify`'s job).
//!
//! **Read-only**: lint never rewrites the file. It is also **math-aware** —
//! inline `$...$` in a title is content, not a malformed field — so a
//! hand-edited maths title (e.g. `$T\bar{T}$`) survives untouched and is
//! never flagged.
//!
//! One JSON-Lines record per finding on stdout:
//! `{"key","entry_type","rule","severity","message"}`. The summary goes to
//! stderr unless `--quiet`.
//!
//! Rules and default severity:
//!
//! - `parse_error` (**error**) — the file is not parseable BibTeX. The
//!   `biblatex` parser also rejects duplicate / blank citation keys at
//!   this stage, so those surface as a `parse_error` (with a descriptive
//!   message) rather than via a dedicated rule.
//! - `missing_required_field` (**warning**) — an expected field for the
//!   entry type is absent. Advisory: real-world `.bib` files are often
//!   loose, so this is a comment, not a failure.
//! - `empty_field` (**warning**) — a present field is blank / whitespace.
//! - `title_math_hazard` (**warning**) — a `title` carries `$$` display
//!   math, which some downstream renderers (e.g. DocumenterCitations)
//!   cannot process; inline `$...$` is fine. Best-effort, advisory.
//!
//! Exit code = number of `error` findings (capped at 255). `--strict`
//! promotes warnings so that ANY finding fails the run.

use anyhow::{Context, Result};
use biblatex::{Bibliography, ChunksExt, Entry, EntryType};

use super::fetch::CliExit;
use super::output::OutputMode;

/// Finding severity. The label is intrinsic to the rule; `--strict` only
/// changes whether warnings count toward the exit code (mirrors `verify`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Severity {
    Error,
    Warning,
}

impl Severity {
    fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    }
}

/// A required-field slot: presence of ANY listed field satisfies it. This
/// absorbs BibTeX/BibLaTeX spelling variants (`journal` vs `journaltitle`,
/// `year` vs `date`, `author` vs `editor`).
type Req = &'static [&'static str];

/// Required-field sets per entry type. Deliberately modest — the goal is
/// to flag obviously-incomplete entries, not to enforce the full BibLaTeX
/// data model. Unknown / other types require only a title.
fn required_fields(t: &EntryType) -> Vec<Req> {
    match t {
        EntryType::Article => vec![
            &["author"],
            &["title"],
            &["journal", "journaltitle"],
            &["year", "date"],
        ],
        EntryType::Book | EntryType::MvBook => vec![
            &["author", "editor"],
            &["title"],
            &["publisher"],
            &["year", "date"],
        ],
        EntryType::InProceedings | EntryType::InCollection | EntryType::InBook => {
            vec![&["author"], &["title"], &["booktitle"], &["year", "date"]]
        }
        EntryType::Proceedings | EntryType::MvProceedings => {
            vec![&["title"], &["year", "date"]]
        }
        EntryType::PhdThesis | EntryType::MastersThesis | EntryType::Thesis => vec![
            &["author"],
            &["title"],
            &["school", "institution"],
            &["year", "date"],
        ],
        EntryType::TechReport | EntryType::Report => {
            vec![&["author"], &["title"], &["institution"], &["year", "date"]]
        }
        _ => vec![&["title"]],
    }
}

/// `true` when at least one of `names` is present on `entry` with a
/// non-empty value.
fn has_any(entry: &Entry, names: Req) -> bool {
    names.iter().any(|n| {
        entry
            .fields
            .get(*n)
            .is_some_and(|c| !c.format_verbatim().trim().is_empty())
    })
}

/// Canonical lowercase entry-type label for the JSON record (informational).
fn entry_type_label(t: &EntryType) -> String {
    format!("{t:?}").to_ascii_lowercase()
}

/// Entry point for `doiget lint <path> [--strict]`.
pub fn run(path: String, strict: bool, mode: OutputMode) -> Result<()> {
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read bibliography file {path}"))?;

    let mut errors = 0u32;
    let mut warnings = 0u32;

    // Tally + emit one JSON-Lines record. Severity is intrinsic to the
    // rule; `strict` is applied only to the exit code below.
    let mut emit = |key: &str, entry_type: &str, rule: &str, sev: Severity, message: String| {
        match sev {
            Severity::Error => errors += 1,
            Severity::Warning => warnings += 1,
        }
        let record = serde_json::json!({
            "key": key,
            "entry_type": entry_type,
            "rule": rule,
            "severity": sev.as_str(),
            "message": message,
        });
        #[allow(clippy::print_stdout)]
        {
            println!("{record}");
        }
    };

    match Bibliography::parse(&text) {
        Err(e) => {
            emit(
                "",
                "",
                "parse_error",
                Severity::Error,
                format!("file did not parse as BibTeX: {e}"),
            );
        }
        Ok(bib) => {
            for entry in bib.iter() {
                let key = entry.key.clone();
                let et = entry_type_label(&entry.entry_type);

                for slot in required_fields(&entry.entry_type) {
                    if !has_any(entry, slot) {
                        emit(
                            &key,
                            &et,
                            "missing_required_field",
                            Severity::Warning,
                            format!("missing expected field for `{et}`: {}", slot.join(" / ")),
                        );
                    }
                }

                for (name, chunks) in &entry.fields {
                    if chunks.format_verbatim().trim().is_empty() {
                        emit(
                            &key,
                            &et,
                            "empty_field",
                            Severity::Warning,
                            format!("field `{name}` is present but empty"),
                        );
                    }
                }

                // Math-aware title hazard: inline `$...$` is fine, but `$$`
                // display math breaks some renderers (DocumenterCitations).
                // Best-effort: inspect the re-serialised title.
                if let Some(chunks) = entry.fields.get("title") {
                    let rendered = chunks.to_biblatex_string(false);
                    let dollars = rendered.matches('$').count();
                    if rendered.contains("$$") || dollars % 2 == 1 {
                        emit(
                            &key,
                            &et,
                            "title_math_hazard",
                            Severity::Warning,
                            "title math is not clean inline `$...$` (found `$$` or an unbalanced \
                             `$`); some renderers (e.g. DocumenterCitations) cannot process it"
                                .to_string(),
                        );
                    }
                }
            }
        }
    }

    let total = errors + warnings;
    if mode != OutputMode::Quiet {
        #[allow(clippy::print_stderr)]
        {
            eprintln!(
                "lint: {total} findings — {errors} error, {warnings} warning{}",
                if strict {
                    " (strict: warnings fail)"
                } else {
                    ""
                }
            );
        }
    }

    let failing = errors + if strict { warnings } else { 0 };
    if failing == 0 {
        Ok(())
    } else {
        Err(anyhow::Error::new(CliExit(failing.min(255) as i32)))
    }
}
