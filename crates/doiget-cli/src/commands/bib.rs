//! `doiget bib` subcommand — emit BibTeX from the **local store**, offline.
//!
//! Three shapes (issue #305):
//! - `bib <ref>` — one stored entry (the original behavior).
//! - `bib --all` — every entry in the store as one deduplicated `.bib`.
//! - `bib --from-file <FILE>` — the refs listed in FILE, each rendered from
//!   the store; entries not present are skipped and counted toward a
//!   non-zero exit.
//!
//! All shapes are pure store reads — no network — so "fetch a batch, then
//! emit a complete `.bib`" is a single offline command. The actual
//! rendering lives in [`doiget_core::store::render::to_bibtex`] so the CLI
//! and the `doiget_bibtex_export` MCP tool share one implementation.

use std::io::Write;

use anyhow::{bail, Context, Result};
use camino::{Utf8Path, Utf8PathBuf};

use doiget_core::refs::{self, Format};
use doiget_core::store::{render, FsStore, Metadata, Store};
use doiget_core::Safekey;

use super::fetch::CliExit;
use super::output::print_err;
use super::resolve_store_root;

/// Run the `bib` subcommand against the configured store.
///
/// Exactly one selector must be supplied: a positional `ref_`, `--all`, or
/// `--from-file`. The BibTeX output is product output (the requested
/// artifact), not informational, so Quiet does NOT suppress it. A missing
/// single entry — or any missing ref under `--from-file` — yields a
/// non-zero exit (never a silent empty stdout: the #302 / #304 contract).
pub fn run(
    ref_: Option<String>,
    all: bool,
    from_file: Option<Utf8PathBuf>,
    _mode: super::output::OutputMode,
) -> Result<()> {
    // Exactly-one-of selector validation (clap leaves all three optional).
    let selectors =
        usize::from(ref_.is_some()) + usize::from(all) + usize::from(from_file.is_some());
    if selectors == 0 {
        bail!("specify a ref, `--all`, or `--from-file <FILE>`");
    }
    if selectors > 1 {
        bail!("`--all`, `--from-file`, and a positional ref are mutually exclusive");
    }

    let store = FsStore::new(resolve_store_root()?)?;

    if all {
        return run_all(&store);
    }
    if let Some(path) = from_file {
        return run_from_file(&store, &path);
    }

    // Single ref (the original behavior). Selector validation above
    // guarantees `ref_` is Some here; match rather than `expect` (the
    // workspace denies `expect`/`panic` in non-test code).
    let input = match ref_ {
        Some(r) => r,
        None => bail!("specify a ref, `--all`, or `--from-file <FILE>`"),
    };
    let ref_ = super::parse_ref_or_exit(&input)?;
    let safekey = ref_.safekey();
    match store.read(&safekey)? {
        Some(m) => write_all(&render::to_bibtex(safekey.as_str(), &m)),
        None => bail!("no entry for {input}"),
    }
}

/// `--all`: render every store entry, deduplicated by citation key, as one
/// `.bib`. An empty store is not a failure — it exits 0 with a stderr note,
/// since "nothing to export" is a valid outcome (no ref was requested).
fn run_all(store: &FsStore) -> Result<()> {
    // `usize::MAX` ⇒ every entry; `list_recent` already orders by recency.
    let entries = store
        .list_recent(usize::MAX)
        .context("failed to enumerate the store")?;

    if entries.is_empty() {
        print_err(format_args!(
            "bib --all: the store is empty; nothing to export"
        ));
        return Ok(());
    }

    let mut out = String::new();
    let mut seen: Vec<String> = Vec::new();
    let mut rendered = 0usize;
    for e in &entries {
        // An entry listed but unreadable (deleted/raced) is skipped loudly
        // rather than aborting the whole export.
        match store.read(&e.safekey) {
            Ok(Some(m)) => push_entry(&mut out, &mut seen, &e.safekey, &m, &mut rendered),
            Ok(None) => {}
            Err(err) => print_err(format_args!(
                "bib --all: skipping {} (read failed: {err})",
                e.safekey.as_str()
            )),
        }
    }

    write_all(&out)?;
    print_err(format_args!("bib --all: exported {rendered} entries"));
    Ok(())
}

/// `--from-file`: render the refs listed in `path` from the store. Missing
/// entries are skipped (with a stderr note) and counted; the process exits
/// non-zero (failure count, capped at 255 — same convention as `batch`)
/// when any requested ref could not be rendered, so a script can tell a
/// complete export from a partial one.
fn run_from_file(store: &FsStore, path: &Utf8Path) -> Result<()> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading --from-file list: {path}"))?;
    // Same bibliography adapter `batch` uses: plain refs / CSL-JSON /
    // BibTeX, auto-detected by extension + content.
    let parsed = refs::parse_input(&raw, Format::Auto, Some(path));

    let mut out = String::new();
    let mut seen: Vec<String> = Vec::new();
    let mut rendered = 0usize;
    let mut missing = 0usize;
    for entry in parsed {
        let ref_ = match entry {
            Ok(p) => p.ref_,
            Err(e) => {
                missing += 1;
                print_err(format_args!(
                    "bib --from-file: skipping unparsable entry ({e})"
                ));
                continue;
            }
        };
        let safekey = ref_.safekey();
        // A read error on one entry skips that entry (counted), matching
        // `run_all` — it must NOT abort the whole export and lose every
        // remaining ref (review #318).
        match store.read(&safekey) {
            Ok(Some(m)) => push_entry(&mut out, &mut seen, &safekey, &m, &mut rendered),
            Ok(None) => {
                missing += 1;
                print_err(format_args!(
                    "bib --from-file: no store entry for {} (skipped)",
                    ref_.as_input_str()
                ));
            }
            Err(err) => {
                missing += 1;
                print_err(format_args!(
                    "bib --from-file: read failed for {} ({err}; skipped)",
                    ref_.as_input_str()
                ));
            }
        }
    }

    write_all(&out)?;
    print_err(format_args!(
        "bib --from-file: exported {rendered} entries, {missing} missing"
    ));
    if missing > 0 {
        // `docs/ERRORS.md` §4: exit = number of failures, capped at 255.
        let code = missing.min(255) as i32;
        return Err(anyhow::Error::new(CliExit(code)));
    }
    Ok(())
}

/// Append one rendered entry to `out`, deduplicated by citation key
/// (`safekey`): a ref list may name the same paper twice, and a deduped
/// `.bib` avoids duplicate-key warnings in LaTeX.
fn push_entry(
    out: &mut String,
    seen: &mut Vec<String>,
    safekey: &Safekey,
    m: &Metadata,
    rendered: &mut usize,
) {
    let key = safekey.as_str();
    if seen.iter().any(|k| k == key) {
        return;
    }
    seen.push(key.to_string());
    if !out.is_empty() {
        // Blank line between entries for readability; entries already end
        // with `}\n`.
        out.push('\n');
    }
    out.push_str(&render::to_bibtex(key, m));
    *rendered += 1;
}

/// Write the rendered BibTeX to stdout. Workspace lints deny
/// `print!`/`println!`; `write!` against an explicit `stdout().lock()` is
/// the sanctioned escape hatch (ADR-0001). Entries already terminate with
/// `}\n`, so no trailing newline is added.
fn write_all(bib: &str) -> Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    write!(out, "{bib}").context("failed to write BibTeX to stdout")
}
