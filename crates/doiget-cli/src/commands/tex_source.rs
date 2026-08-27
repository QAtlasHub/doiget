//! `doiget tex-source <ref>` — fetch the raw LaTeX source of an arXiv paper.
//!
//! Fetches the arXiv source tarball (`export.arxiv.org/src/<id>`), extracts
//! the main `.tex` file, and emits its content. This is the structured-text
//! complement to `doiget text` (ar5iv HTML extraction) and is more reliable
//! for papers that ar5iv has not processed.
//!
//! - **arXiv id** → raw LaTeX source via
//!   [`doiget_core::paper_tex_source::paper_tex_source`].
//! - **DOI** → structured `NO_OA_AVAILABLE`.
//! - **PDF-only submission** → `TEXT_UNAVAILABLE` with an actionable note.

use std::io::Write;

use anyhow::{Context, Result};

use doiget_core::paper_tex_source::{paper_tex_source, resolve_arxiv_src_base, PaperTexSource};
use doiget_core::{ArxivId, ErrorCode, Ref};

use super::fetch::{build_resolve_context, cli_exit_code, CliExit};
use super::output::print_err;
use super::output::OutputMode;

/// Run the `tex-source` subcommand.
///
/// # Errors
///
/// Returns a typed [`ErrorCode`] as a process exit code via [`CliExit`].
pub async fn run(
    ref_: String,
    max_chars: Option<usize>,
    no_cache: bool,
    mode: OutputMode,
    quiet_was_explicit: bool,
) -> Result<()> {
    // #492 / ADR-0049: one renderer, one exit code. This used to be
    // `Ref::parse(..).with_context(..)?`, which exited 1 and leaked the
    // `Caused by:` chain that #477's contract exists to replace — the ADR
    // claimed the rule held for "every ref-taking command" and this was one
    // of two that were never in the set.
    let parsed = super::parse_ref_or_exit(&ref_)?;
    let id: ArxivId = match parsed {
        Ref::Arxiv(a) => a,
        Ref::Doi(_) => {
            let code = ErrorCode::NoOaAvailable;
            print_err(format_args!(
                "error[{}]: no TeX source for a bare DOI — if an arXiv preprint exists, \
                 pass its id (e.g. `doiget tex-source arxiv:2401.12345`)",
                code.as_wire()
            ));
            return Err(anyhow::Error::new(CliExit(cli_exit_code(code))));
        }
    };

    let base = resolve_arxiv_src_base().map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut ctx = build_resolve_context().context("building fetch context")?;
    if no_cache {
        ctx.cache_root = None;
    }

    let tex = match paper_tex_source(&base, &id, max_chars, &ctx).await {
        Ok(t) => t,
        Err(e) => {
            let code = ErrorCode::from(&e);
            print_err(format_args!("error[{}]: {e}", code.as_wire()));
            if code == ErrorCode::TextUnavailable {
                print_err(format_args!(
                    "  = note: no TeX source available (PDF-only or no .tex files). \
                     Fetch the PDF instead: `doiget fetch arxiv:{}`",
                    id.as_str()
                ));
            }
            return Err(anyhow::Error::new(CliExit(cli_exit_code(code))));
        }
    };

    // TeX source is the requested artifact — suppress only on *explicit* Quiet
    // (ADR-0017 Amendment 2, same logic as `doiget text`).
    if mode == OutputMode::Quiet && quiet_was_explicit {
        return Ok(());
    }

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if mode == OutputMode::Json {
        let s = serde_json::to_string_pretty(&tex).context("serializing tex-source JSON")?;
        writeln!(out, "{s}").context("writing tex-source JSON to stdout")?;
        return Ok(());
    }

    render_human(&mut out, &tex)?;
    Ok(())
}

fn render_human(out: &mut impl Write, tex: &PaperTexSource) -> Result<()> {
    if let Some(f) = &tex.main_file {
        writeln!(out, "% source: {f}").context("writing file header")?;
    }
    writeln!(out, "{}", tex.tex_source).context("writing tex source to stdout")?;
    if tex.truncated {
        print_err(format_args!(
            "note: output truncated to {} chars (raise or drop --max-chars for the full source)",
            tex.char_count
        ));
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, missing_docs)]
mod tests {
    use super::*;

    #[test]
    fn human_render_emits_file_header_and_source() {
        let tex = PaperTexSource {
            arxiv_id: "2401.12345".into(),
            main_file: Some("main.tex".into()),
            tex_source: "\\documentclass{article}".into(),
            char_count: 23,
            truncated: false,
            retrieved_from: "https://export.arxiv.org/src/2401.12345".into(),
        };
        let mut buf: Vec<u8> = Vec::new();
        render_human(&mut buf, &tex).expect("render");
        let s = String::from_utf8(buf).expect("utf8");
        assert!(s.contains("% source: main.tex"), "got: {s}");
        assert!(s.contains("\\documentclass"), "got: {s}");
    }

    #[test]
    fn json_envelope_has_expected_fields() {
        let tex = PaperTexSource {
            arxiv_id: "2401.12345".into(),
            main_file: None,
            tex_source: "\\documentclass{article}".into(),
            char_count: 23,
            truncated: false,
            retrieved_from: "https://export.arxiv.org/src/2401.12345".into(),
        };
        let v = serde_json::to_value(&tex).expect("serialize");
        assert_eq!(v["arxiv_id"], "2401.12345");
        assert_eq!(v["truncated"], false);
        assert_eq!(v["char_count"], 23);
    }
}
