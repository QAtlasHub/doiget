//! `doiget text <ref>` — extract a paper's full text (the #281 "read"
//! step; ADR-0032).
//!
//! Fetches the ar5iv LaTeXML-XHTML rendering of an **arXiv** paper and
//! emits it as sectioned plain text — the read step of the agent research
//! loop, without an external pdf-to-text tool. The PDF blob is never
//! opened (ADR-0032 D1).
//!
//! - **arXiv id** → ar5iv extraction via
//!   [`doiget_core::paper_text::paper_text`].
//! - **DOI** → a structured `NO_OA_AVAILABLE` ("pass the arXiv id"):
//!   DOI→arXiv resolution is #281 item 5 (ADR-0032 D5).
//!
//! `--max-chars N` caps the returned text (truncation is flagged, never
//! silent); `--no-cache` bypasses the on-disk text cache. `--mode json`
//! emits the [`PaperText`] structure; the human mode renders a
//! Markdown-ish title + section layout. Tier-1 OA metadata, always-on —
//! ships in the default `oa-only` binary (ADR-0032 D2).

use std::io::Write;

use anyhow::{Context, Result};

use doiget_core::paper_text::{paper_text, PaperText, AR5IV_DEFAULT_BASE};
use doiget_core::{ArxivId, ErrorCode, Ref};

use super::fetch::{build_resolve_context, cli_exit_code, CliExit};
use super::output::print_err;
use super::output::OutputMode;

/// Run the `text` subcommand.
///
/// # Errors
///
/// Surfaces a typed [`ErrorCode`] as a process exit code via
/// [`CliExit`]: an invalid ref is a usage error; a DOI yields
/// `NO_OA_AVAILABLE`; an ar5iv render with no extractable prose yields
/// `TEXT_UNAVAILABLE` (never a silent exit-0 — issue #302) with an
/// actionable "fetch the PDF" note; other extraction failures map through
/// [`ErrorCode::from`].
pub async fn run(
    ref_: String,
    max_chars: Option<usize>,
    no_cache: bool,
    mode: OutputMode,
    quiet_was_explicit: bool,
) -> Result<()> {
    let parsed = super::parse_ref_or_exit(&ref_)?;
    let id: ArxivId = match parsed {
        Ref::Arxiv(a) => a,
        Ref::Doi(_) => {
            // No full-text HTML source for a bare DOI in PR4; DOI→arXiv
            // linking is #281 item 5 (ADR-0032 D5). Report honestly rather
            // than silently failing.
            // `NOT_IMPLEMENTED`, not `NO_OA_AVAILABLE`. The latter carries
            // disposition `needs_config` -- "a named change makes it" -- and
            // there is no knob: this command is arXiv-only and DOI-to-arXiv
            // linking is not built (#281 item 5). Kept identical to the MCP
            // sibling; changing one surface and not the other is the defect
            // this release is about, and the first pass at this fix did
            // exactly that.
            let code = ErrorCode::NotImplemented;
            print_err(format_args!(
                "error[{}]: no full-text source for a DOI — if an arXiv preprint exists, \
                 pass its id (e.g. `doiget text arxiv:2401.12345`)",
                code.as_wire()
            ));
            return Err(anyhow::Error::new(CliExit(cli_exit_code(code))));
        }
    };

    let base = resolve_ar5iv_base()?;
    // `text` is a read-only resolve command (like `cite` / `verify`), so it
    // reuses the resolve context — which enables the on-disk cache root
    // (`docs/CACHE.md`) that `paper_text` consults for the text cache.
    let mut ctx = build_resolve_context().context("building fetch context")?;
    if no_cache {
        ctx.cache_root = None;
    }

    let text = match paper_text(&base, &id, max_chars, &ctx).await {
        Ok(t) => t,
        Err(e) => {
            let code = ErrorCode::from(&e);
            print_err(format_args!("error[{}]: {e}", code.as_wire()));
            // `text unavailable` is the one read-step failure with a
            // concrete next action: the id is valid and the PDF may well be
            // fetchable, so spell the exact command out (issue #302) rather
            // than leave the agent to infer it. Mirrors the `= note:` line
            // `render_fetch_error` attaches to denial-class failures.
            if code == ErrorCode::TextUnavailable {
                print_err(format_args!(
                    "  = note: the arXiv id is valid — fetch the PDF instead: `doiget fetch arxiv:{}`",
                    id.as_str()
                ));
            }
            return Err(anyhow::Error::new(CliExit(cli_exit_code(code))));
        }
    };

    // Extracted paper prose IS the requested artifact (like `bib` / `info`),
    // so an *implicit* non-TTY Quiet (e.g. `doiget text arxiv:… > paper.txt`)
    // must NOT swallow it — only an explicit `--quiet` / `DOIGET_MODE=quiet`
    // does. This is ADR-0017 Amendment 2 (#301), extended to `text`.
    if mode == OutputMode::Quiet && quiet_was_explicit {
        return Ok(());
    }

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if mode == OutputMode::Json {
        let s = serde_json::to_string_pretty(&text).context("serializing paper text JSON")?;
        writeln!(out, "{s}").context("writing paper text JSON to stdout")?;
        return Ok(());
    }

    render_human(&mut out, &text)?;
    Ok(())
}

/// Resolve the ar5iv base URL: `DOIGET_AR5IV_BASE` override (tests) or the
/// production default.
fn resolve_ar5iv_base() -> Result<url::Url> {
    let raw = std::env::var("DOIGET_AR5IV_BASE").unwrap_or_else(|_| AR5IV_DEFAULT_BASE.to_string());
    url::Url::parse(&raw).with_context(|| format!("DOIGET_AR5IV_BASE is not a URL: {raw}"))
}

/// Render extracted text in human mode: a Markdown-ish title + section
/// layout. The truncation note (when applicable) goes to stderr so it does
/// not pollute the piped text body on stdout.
fn render_human(out: &mut impl Write, text: &PaperText) -> Result<()> {
    if let Some(t) = &text.title {
        writeln!(out, "# {t}").context("writing title to stdout")?;
    }
    for sec in &text.sections {
        if let Some(h) = &sec.heading {
            writeln!(out, "\n## {h}").context("writing section heading to stdout")?;
        }
        if !sec.text.is_empty() {
            writeln!(out, "{}", sec.text).context("writing section body to stdout")?;
        }
    }
    if text.truncated {
        print_err(format_args!(
            "note: output truncated to {} chars (raise or drop --max-chars for the full text)",
            text.char_count
        ));
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use doiget_core::paper_text::{TextSection, TextSource};

    fn sample() -> PaperText {
        PaperText {
            arxiv_id: "2401.12345".into(),
            source: TextSource::Ar5iv,
            title: Some("A Title".into()),
            sections: vec![
                TextSection {
                    heading: None,
                    text: "Lead paragraph.".into(),
                },
                TextSection {
                    heading: Some("1 Introduction".into()),
                    text: "Body text.".into(),
                },
            ],
            char_count: 25,
            truncated: false,
            retrieved_from: "https://ar5iv.labs.arxiv.org/html/2401.12345".into(),
        }
    }

    #[test]
    fn json_envelope_is_the_paper_text_shape() {
        let v = serde_json::to_value(sample()).expect("serialize");
        assert_eq!(v["arxiv_id"], "2401.12345");
        assert_eq!(v["source"], "ar5iv");
        assert_eq!(v["title"], "A Title");
        assert_eq!(v["sections"][1]["heading"], "1 Introduction");
        assert_eq!(v["truncated"], false);
    }

    #[test]
    fn human_render_lays_out_title_and_sections() {
        let mut buf: Vec<u8> = Vec::new();
        render_human(&mut buf, &sample()).expect("render");
        let s = String::from_utf8(buf).expect("utf8");
        assert!(s.contains("# A Title"), "got: {s}");
        assert!(s.contains("## 1 Introduction"), "got: {s}");
        assert!(s.contains("Lead paragraph."), "got: {s}");
        assert!(s.contains("Body text."), "got: {s}");
    }

    #[test]
    fn resolve_ar5iv_base_defaults_to_production() {
        // With no override set, the default base must be the production
        // ar5iv host. (Serial-free: only asserts the default branch when
        // the env var is absent.)
        if std::env::var("DOIGET_AR5IV_BASE").is_err() {
            let u = resolve_ar5iv_base().expect("base");
            assert_eq!(u.as_str(), "https://ar5iv.labs.arxiv.org/");
        }
    }
}
