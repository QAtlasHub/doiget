//! `doiget source <ref>` — download an arXiv submission's **source bundle**
//! (every file) or just its **figures** to a directory (ADR-0034, issue #343).
//!
//! Fetches the same arXiv source tarball as `doiget tex-source`
//! (`export.arxiv.org/src/<id>`, one request) but, instead of extracting only
//! the main `.tex` text, materialises files to `--out`:
//!
//! - default: the full bundle (`*.tex`, `*.bib`, `*.sty`, figures, …),
//! - `--figures-only`: just the image artifacts.
//!
//! Files are written **opaque** (never interpreted; ADR-0034 D2). Tar entry
//! paths are sanitised in the core (`sanitize_entry_path`, ADR-0034 D3) and
//! the join under `--out` is re-checked here as defence-in-depth, so a
//! malicious archive cannot write outside the output directory (zip-slip).
//!
//! - **arXiv id** → files under `--out`.
//! - **DOI** → structured `NO_OA_AVAILABLE` (pass the arXiv id).
//! - **PDF-only / single-file submission** → `TEXT_UNAVAILABLE` with a note.

use std::io::Write;

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};

use doiget_core::paper_tex_source::{
    paper_source_bundle, resolve_arxiv_src_base, BundleFilter, SourceFile,
};
use doiget_core::{ArxivId, ErrorCode, Ref};

use super::fetch::{build_resolve_context, cli_exit_code, CliExit};
use super::output::OutputMode;

#[allow(clippy::print_stderr)]
fn print_err(args: std::fmt::Arguments<'_>) {
    eprintln!("{args}");
}

/// Run the `source` subcommand.
///
/// # Errors
///
/// Returns a typed [`ErrorCode`] as a process exit code via [`CliExit`] for
/// the fetch/resolve failures; filesystem write failures surface as a generic
/// non-zero exit through the top-level reporter.
pub async fn run(
    ref_: String,
    out_dir: Utf8PathBuf,
    figures_only: bool,
    mode: OutputMode,
    quiet_was_explicit: bool,
) -> Result<()> {
    let parsed = Ref::parse(&ref_).with_context(|| format!("invalid ref {ref_:?}"))?;
    let id: ArxivId = match parsed {
        Ref::Arxiv(a) => a,
        Ref::Doi(_) => {
            let code = ErrorCode::NoOaAvailable;
            print_err(format_args!(
                "error[{}]: no source bundle for a bare DOI — if an arXiv preprint exists, \
                 pass its id (e.g. `doiget source arxiv:2401.12345 --out ./src`)",
                code.as_wire()
            ));
            return Err(anyhow::Error::new(CliExit(cli_exit_code(code))));
        }
    };

    let base = resolve_arxiv_src_base().map_err(|e| anyhow::anyhow!("{e}"))?;
    let ctx = build_resolve_context().context("building fetch context")?;
    let filter = if figures_only {
        BundleFilter::FiguresOnly
    } else {
        BundleFilter::All
    };

    let files = match paper_source_bundle(&base, &id, filter, &ctx).await {
        Ok(f) => f,
        Err(e) => {
            let code = ErrorCode::from(&e);
            print_err(format_args!("error[{}]: {e}", code.as_wire()));
            if code == ErrorCode::TextUnavailable {
                print_err(format_args!(
                    "  = note: no {} available (PDF-only submission or single-file source). \
                     Fetch the PDF instead: `doiget fetch arxiv:{}`",
                    if figures_only {
                        "figures"
                    } else {
                        "source bundle"
                    },
                    id.as_str()
                ));
            }
            return Err(anyhow::Error::new(CliExit(cli_exit_code(code))));
        }
    };

    let written = write_files(&out_dir, &files)?;

    // The written files ARE the artifact — suppress only on *explicit* Quiet
    // (ADR-0017 Amendment 2, same rule as `doiget tex-source`). The files are
    // already on disk regardless; this only governs the stdout summary.
    if mode == OutputMode::Quiet && quiet_was_explicit {
        return Ok(());
    }

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if mode == OutputMode::Json {
        let payload = serde_json::json!({
            "ok": true,
            "arxiv_id": id.as_str(),
            "out_dir": out_dir.as_str(),
            "figures_only": figures_only,
            "count": written.len(),
            "files": written.iter().map(Utf8PathBuf::as_str).collect::<Vec<_>>(),
        });
        let s = serde_json::to_string_pretty(&payload).context("serializing source JSON")?;
        writeln!(out, "{s}").context("writing source JSON to stdout")?;
        return Ok(());
    }

    writeln!(out, "wrote {} file(s) to {out_dir}", written.len())
        .context("writing source summary")?;
    for rel in &written {
        writeln!(out, "  {rel}").context("writing source file line")?;
    }
    Ok(())
}

/// Write each [`SourceFile`] under `out_dir`, returning the relative paths
/// written (sorted for stable output).
///
/// `f.path` is already sanitised by the core (relative, no `..`; ADR-0034 D3),
/// but the join is re-verified to stay within `out_dir` as defence-in-depth —
/// a regression in the core sanitiser cannot turn into a write outside the
/// output directory here.
fn write_files(out_dir: &Utf8Path, files: &[SourceFile]) -> Result<Vec<Utf8PathBuf>> {
    std::fs::create_dir_all(out_dir.as_std_path())
        .with_context(|| format!("creating output dir {out_dir}"))?;

    let mut written: Vec<Utf8PathBuf> = Vec::with_capacity(files.len());
    for f in files {
        let dest = out_dir.join(&f.path);
        if !dest.starts_with(out_dir) {
            anyhow::bail!(
                "refusing to write outside the output dir (zip-slip guard): {}",
                f.path
            );
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent.as_std_path())
                .with_context(|| format!("creating {parent}"))?;
        }
        std::fs::write(dest.as_std_path(), &f.bytes).with_context(|| format!("writing {dest}"))?;
        written.push(f.path.clone());
    }
    written.sort();
    Ok(written)
}
