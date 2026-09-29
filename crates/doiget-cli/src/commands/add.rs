//! `doiget add` -- put a hand-downloaded PDF in the store (#606).
//!
//! `doiget add <ref> <file.pdf>` adds one file; `doiget add --from-dir DIR`
//! matches a downloads folder against the entries still missing a PDF --
//! those of `--refs FILE`, else every store entry without one -- by the id in
//! each file's name (`BF01340294.pdf` for `10.1007/BF01340294`). The folder
//! form only prints its plan unless `--apply` is given.
//!
//! Every check and the recorded provenance are `doiget_core::user_pdf`'s:
//! the PDF is never read beyond its `%PDF-` magic (ADR-0003), and the entry
//! says `origin = "user-supplied"`, `license = "unknown"`.

use anyhow::{bail, Context, Result};
use camino::{Utf8Path, Utf8PathBuf};

use doiget_core::refs::parse_input;
use doiget_core::store::{blocking_section, FsStore, Store};
use doiget_core::user_pdf::{add_user_pdf, file_stem, match_file, AddError};
use doiget_core::{CapabilityProfile, Ref};

use super::fetch::CliExit;
use super::output::print_err;

/// Entry point for `doiget add`.
///
/// # Errors
///
/// Misuse (exit 2) for a bad argument combination; otherwise the number of
/// files that could not be added.
pub async fn run(
    ref_: Option<String>,
    file: Option<Utf8PathBuf>,
    from_dir: Option<Utf8PathBuf>,
    refs: Option<Utf8PathBuf>,
    force: bool,
    apply: bool,
) -> Result<()> {
    // A bad ref is reported under the error[CODE] contract every ref-taking
    // command honours, before any other argument is looked at.
    let parsed = match &ref_ {
        Some(r) => Some(super::parse_ref_or_exit(r)?),
        None => None,
    };
    let store_root = super::resolve_store_root()?;
    let store = FsStore::new(store_root.clone())?;
    let ctx = super::fetch::build_resolve_context()?;
    let profile = CapabilityProfile::from_env()?;
    match (parsed, file, from_dir) {
        (Some(ref_), Some(f), None) => {
            match add_user_pdf(&ref_, &f, force, &profile, &ctx, &store, &store_root).await {
                Ok(o) => {
                    print_err(format_args!(
                        "added {} ({} bytes, user-supplied){} -> {}",
                        ref_.as_input_str(),
                        o.size_bytes,
                        if o.replaced {
                            ", replacing the stored PDF"
                        } else {
                            ""
                        },
                        o.path
                    ));
                    print_err(format_args!("     \"{}\"", o.title));
                    Ok(())
                }
                Err(e) => fail(&e),
            }
        }
        (None, None, Some(dir)) => {
            run_from_dir(
                &dir,
                refs.as_deref(),
                force,
                apply,
                &profile,
                &ctx,
                &store,
                &store_root,
            )
            .await
        }
        _ => {
            print_err(format_args!(
                "error: give a ref and a file (`doiget add <ref> <file.pdf>`), or `--from-dir DIR`"
            ));
            Err(anyhow::Error::new(CliExit(2)))
        }
    }
}

fn fail(e: &AddError) -> Result<()> {
    print_err(format_args!("error: {e}"));
    // docs/ERRORS.md §4: a resolve failure exits as `fetch` would for its
    // code, and 4 is the store's I/O failure (#649 review: these were
    // swapped, 4 for a resolve and 1 for the store).
    let code = match e {
        AddError::Resolve { source, .. } => super::fetch::cli_exit_code(source.into()),
        AddError::Store(_) | AddError::UnreadableEntry { .. } => 4,
        _ => 2,
    };
    Err(anyhow::Error::new(CliExit(code)))
}

#[allow(clippy::too_many_arguments)]
async fn run_from_dir(
    dir: &Utf8Path,
    refs: Option<&Utf8Path>,
    force: bool,
    apply: bool,
    profile: &CapabilityProfile,
    ctx: &doiget_core::source::FetchContext,
    store: &FsStore,
    store_root: &Utf8Path,
) -> Result<()> {
    let candidates = match refs {
        Some(path) => {
            let text = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
            parse_input(&text, doiget_core::refs::Format::Auto, Some(path))
                .into_iter()
                .filter_map(Result::ok)
                .map(|p| p.ref_)
                .filter(|r| !has_pdf(store_root, r))
                .collect::<Vec<_>>()
        }
        None => missing_in_store(store, store_root)?,
    };
    let mut files: Vec<Utf8PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {dir}"))?
        .filter_map(|e| e.ok())
        .filter_map(|e| Utf8PathBuf::from_path_buf(e.path()).ok())
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("pdf")))
        .collect();
    files.sort();
    if files.is_empty() {
        bail!("no .pdf files in {dir}");
    }

    let mut plan: Vec<(Utf8PathBuf, Ref)> = Vec::new();
    let mut unmatched: Vec<Utf8PathBuf> = Vec::new();
    for f in files {
        match match_file(&file_stem(&f), &candidates) {
            Some(r) if !plan.iter().any(|(_, p)| p == r) => plan.push((f, r.clone())),
            _ => unmatched.push(f),
        }
    }
    let still_missing = candidates
        .iter()
        .filter(|r| !plan.iter().any(|(_, p)| p == *r))
        .count();

    let mut failures = 0usize;
    for (f, r) in &plan {
        if !apply {
            print_err(format_args!("would add {} -> {}", f, r.as_input_str()));
            continue;
        }
        match add_user_pdf(r, f, force, profile, ctx, store, store_root).await {
            Ok(o) => print_err(format_args!(
                "added {} -> {} ({})",
                f,
                r.as_input_str(),
                o.path
            )),
            Err(e) => {
                failures += 1;
                print_err(format_args!("error: {f}: {e}"));
            }
        }
    }
    for f in &unmatched {
        print_err(format_args!(
            "unmatched: {f} (its name is not the id of exactly one entry missing a PDF; \
             add it with `doiget add <ref> {f}`)"
        ));
    }
    print_err(format_args!(
        "add --from-dir: {} matched{}, {} unmatched file(s), {} entr{} still without a PDF",
        plan.len(),
        if apply {
            ""
        } else {
            " (dry run: pass --apply to add them)"
        },
        unmatched.len(),
        still_missing,
        if still_missing == 1 { "y" } else { "ies" },
    ));
    if failures > 0 {
        let code = i32::try_from(failures.min(255)).unwrap_or(255);
        return Err(anyhow::Error::new(CliExit(code)));
    }
    Ok(())
}

fn has_pdf(store_root: &Utf8Path, r: &Ref) -> bool {
    store_root
        .join(format!("{}.pdf", r.safekey().as_str()))
        .exists()
}

/// Store entries with metadata but no PDF, as refs.
fn missing_in_store(store: &FsStore, store_root: &Utf8Path) -> Result<Vec<Ref>> {
    let entries = blocking_section(|| store.list_recent(usize::MAX))?;
    let mut out = Vec::new();
    for e in entries {
        if store_root
            .join(format!("{}.pdf", e.safekey.as_str()))
            .exists()
        {
            continue;
        }
        let m = match blocking_section(|| store.read(&e.safekey)) {
            Ok(Some(m)) => m,
            Ok(None) => continue,
            Err(err) => {
                print_err(format_args!(
                    "warning: skipping {} (its store entry could not be read: {err})",
                    e.safekey.as_str()
                ));
                continue;
            }
        };
        if let Some(d) = m.doi {
            out.push(Ref::Doi(d));
        } else if let Some(a) = m.arxiv_id {
            out.push(Ref::Arxiv(a));
        }
    }
    Ok(out)
}
