//! `doiget csl` — emit CSL JSON 1.0 from the **local store**, offline.
//!
//! Three shapes (parity with `bib`, issue #305):
//! - `csl <ref>` — a single-element CSL JSON array for one stored entry.
//! - `csl --all` — every store entry as one deduplicated CSL JSON array.
//! - `csl --from-file <FILE>` — the refs listed in FILE (plain refs /
//!   CSL-JSON / BibTeX), each rendered from the store; missing entries are
//!   skipped and counted toward a non-zero exit.
//!
//! All shapes are pure store reads — no network. Rendering lives in
//! [`doiget_core::store::render::to_csl_array`] so the CLI and the
//! `doiget_csl_export` MCP tool share one implementation.

use std::io::Write;

use anyhow::{bail, Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use serde_json::Value;

use doiget_core::refs::{self, Format};
use doiget_core::store::{render, FsStore, Metadata, Store};
use doiget_core::{Ref, Safekey};

use super::fetch::CliExit;
use super::resolve_store_root;

/// Stderr sink for progress / skip notes (`docs/ERRORS.md` §3); stdout
/// carries only the CSL JSON artifact (ADR-0001).
#[allow(clippy::print_stderr)]
fn print_err(args: std::fmt::Arguments<'_>) {
    eprintln!("{args}");
}

/// Run the `csl` subcommand against the configured store.
///
/// Exactly one selector must be supplied: a positional `ref_`, `--all`, or
/// `--from-file`. CSL JSON is product output, so Quiet does NOT suppress
/// it. A missing single entry — or any missing ref under `--from-file` —
/// yields a non-zero exit (the array still serializes, possibly empty).
pub fn run(
    ref_: Option<String>,
    all: bool,
    from_file: Option<Utf8PathBuf>,
    _mode: super::output::OutputMode,
) -> Result<()> {
    let selectors =
        usize::from(ref_.is_some()) + usize::from(all) + usize::from(from_file.is_some());
    if selectors > 1 {
        bail!("`--all`, `--from-file`, and a positional ref are mutually exclusive");
    }
    // Fail the usage error BEFORE opening the store (parity with `bib`,
    // review #318): otherwise a store-open failure would mask the real
    // "no selector" mistake.
    if selectors == 0 {
        bail!("specify a ref, `--all`, or `--from-file <FILE>`");
    }

    let store = FsStore::new(resolve_store_root()?)?;

    if all {
        return run_all(&store);
    }
    if let Some(path) = from_file {
        return run_from_file(&store, &path);
    }

    // Single ref (the original behavior): a one-element array.
    let input = match ref_ {
        Some(r) => r,
        None => bail!("specify a ref, `--all`, or `--from-file <FILE>`"),
    };
    let ref_ = Ref::parse(&input).with_context(|| format!("invalid ref: {input}"))?;
    let safekey = ref_.safekey();
    match store.read(&safekey)? {
        Some(m) => write_array(&render::to_csl_array(safekey.as_str(), &m)),
        None => bail!("no entry for {input}"),
    }
}

/// `--all`: every store entry as one deduplicated CSL JSON array. An empty
/// store emits `[]` with a stderr note (exit 0 — no ref was requested).
fn run_all(store: &FsStore) -> Result<()> {
    let entries = store
        .list_recent(usize::MAX)
        .context("failed to enumerate the store")?;

    let mut items: Vec<Value> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for e in &entries {
        match store.read(&e.safekey) {
            Ok(Some(m)) => push_item(&mut items, &mut seen, &e.safekey, &m),
            Ok(None) => {}
            Err(err) => print_err(format_args!(
                "csl --all: skipping {} (read failed: {err})",
                e.safekey.as_str()
            )),
        }
    }

    write_array(&Value::Array(items))?;
    print_err(format_args!("csl --all: exported {} entries", seen.len()));
    Ok(())
}

/// `--from-file`: render the refs listed in `path` from the store. Missing
/// entries are skipped (stderr note) and counted; the process exits
/// non-zero (failure count, capped at 255 — same convention as `batch` /
/// `bib`) when any requested ref could not be rendered.
fn run_from_file(store: &FsStore, path: &Utf8Path) -> Result<()> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading --from-file list: {path}"))?;
    // Same bibliography adapter `batch` / `bib` use: plain refs / CSL-JSON
    // / BibTeX, auto-detected by extension + content.
    let parsed = refs::parse_input(&raw, Format::Auto, Some(path));

    let mut items: Vec<Value> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let mut missing = 0usize;
    for entry in parsed {
        let ref_ = match entry {
            Ok(p) => p.ref_,
            Err(e) => {
                missing += 1;
                print_err(format_args!(
                    "csl --from-file: skipping unparsable entry ({e})"
                ));
                continue;
            }
        };
        let safekey = ref_.safekey();
        // A read error on one entry skips that entry (counted), matching
        // `run_all` — it must NOT abort the whole export and lose every
        // remaining ref (review #318).
        match store.read(&safekey) {
            Ok(Some(m)) => push_item(&mut items, &mut seen, &safekey, &m),
            Ok(None) => {
                missing += 1;
                print_err(format_args!(
                    "csl --from-file: no store entry for {} (skipped)",
                    ref_.as_input_str()
                ));
            }
            Err(err) => {
                missing += 1;
                print_err(format_args!(
                    "csl --from-file: read failed for {} ({err}; skipped)",
                    ref_.as_input_str()
                ));
            }
        }
    }

    write_array(&Value::Array(items))?;
    print_err(format_args!(
        "csl --from-file: exported {} entries, {missing} missing",
        seen.len()
    ));
    if missing > 0 {
        let code = missing.min(255) as i32;
        return Err(anyhow::Error::new(CliExit(code)));
    }
    Ok(())
}

/// Append one entry's CSL item(s) to `items`, deduplicated by citation key
/// (`safekey`). `to_csl_array` returns a single-element array; its elements
/// are flattened into the combined array so the output is one flat CSL
/// list (what citeproc-js / pandoc expect).
fn push_item(items: &mut Vec<Value>, seen: &mut Vec<String>, safekey: &Safekey, m: &Metadata) {
    let key = safekey.as_str();
    if seen.iter().any(|k| k == key) {
        return;
    }
    seen.push(key.to_string());
    match render::to_csl_array(key, m) {
        Value::Array(rendered) if !rendered.is_empty() => items.extend(rendered),
        // `to_csl_array` falls back to an empty array on a (rare)
        // serialization failure. Don't silently drop the entry from the
        // export — surface it (review #318), consistent with the release's
        // never-silently-lose-data contract.
        other => tracing::error!(
            safekey = key,
            value = %other,
            "to_csl_array produced no CSL item; entry omitted from export"
        ),
    }
}

/// Serialize and write a CSL JSON array to stdout. Workspace lints deny
/// `print!`/`println!`; `writeln!` against an explicit `stdout().lock()` is
/// the sanctioned escape hatch (ADR-0001).
fn write_array(array: &Value) -> Result<()> {
    let json =
        serde_json::to_string_pretty(array).context("failed to serialize CSL JSON for stdout")?;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(out, "{json}").context("failed to write CSL JSON to stdout")
}
