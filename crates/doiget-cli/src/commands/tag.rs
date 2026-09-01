//! `doiget tag <ref> [tags...]` and `doiget annotate <ref> <text>` subcommands.
//!
//! `doiget tag` adds / removes tags and collection membership on a stored
//! entry by mutating the `[doiget].tags` and `[doiget].collections` arrays in
//! the metadata TOML (issue #294). All mutations are idempotent.
//!
//! `doiget annotate` sets or clears the `[doiget].annotation` freeform string.
//!
//! Both commands require the entry to be in the store first (they only mutate
//! the `[doiget]` table; they never fetch or download anything).

use std::io::Write;

use anyhow::{bail, Context, Result};

use doiget_core::store::{FsStore, Store};

use super::output::OutputMode;
use super::resolve_store_root;

/// Run the `tag` subcommand: add/remove tags or collections on a stored entry.
///
/// `add` — tags to add (idempotent).
/// `remove` — tags to remove.
/// `collection_add` — collections to join (idempotent).
/// `collection_remove` — collections to leave.
/// `list` — print current tags / collections / annotation then exit.
#[allow(clippy::too_many_arguments)]
pub fn run(
    ref_str: String,
    add: Vec<String>,
    remove: Vec<String>,
    collection_add: Vec<String>,
    collection_remove: Vec<String>,
    list: bool,
    mode: OutputMode,
    quiet_was_explicit: bool,
) -> Result<()> {
    let ref_ = super::parse_ref_or_exit(&ref_str)?;
    let safekey = ref_.safekey();

    let store_root = resolve_store_root()?;
    let store = FsStore::new(store_root)?;

    let mut metadata = store
        .read(&safekey)
        .with_context(|| format!("failed to read store entry for {ref_str}"))?
        .with_context(|| {
            format!("no store entry for {ref_str}; run `doiget fetch {ref_str}` first")
        })?;

    {
        let ext = metadata.doiget.as_mut().with_context(|| {
            format!(
                "entry {ref_str} has no [doiget] table; \
                 run `doiget fetch {ref_str}` first"
            )
        })?;

        if list {
            if mode == OutputMode::Quiet && quiet_was_explicit {
                return Ok(());
            }
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            if mode == OutputMode::Json {
                let v = serde_json::json!({
                    "ref": ref_str,
                    "tags": &ext.tags,
                    "collections": &ext.collections,
                    "annotation": &ext.annotation,
                });
                let s = serde_json::to_string_pretty(&v)
                    .context("failed to serialize tag info to JSON")?;
                writeln!(out, "{s}").context("failed to write tag info JSON to stdout")?;
            } else {
                let tags_str = if ext.tags.is_empty() {
                    "-".to_string()
                } else {
                    ext.tags.join(", ")
                };
                let cols_str = if ext.collections.is_empty() {
                    "-".to_string()
                } else {
                    ext.collections.join(", ")
                };
                let ann_str = ext.annotation.as_deref().unwrap_or("-");
                writeln!(out, "tags:        {tags_str}").context("stdout write")?;
                writeln!(out, "collections: {cols_str}").context("stdout write")?;
                writeln!(out, "annotation:  {ann_str}").context("stdout write")?;
            }
            return Ok(());
        }

        if add.is_empty()
            && remove.is_empty()
            && collection_add.is_empty()
            && collection_remove.is_empty()
        {
            bail!(
                "no action specified; provide <tag>... to add, \
                 or use --remove / --collection / --list"
            );
        }

        for t in &add {
            if !ext.tags.contains(t) {
                ext.tags.push(t.clone());
            }
        }
        for t in &remove {
            ext.tags.retain(|x| x != t);
        }
        for c in &collection_add {
            if !ext.collections.contains(c) {
                ext.collections.push(c.clone());
            }
        }
        for c in &collection_remove {
            ext.collections.retain(|x| x != c);
        }
    }

    store
        .write_user_authored(&safekey, &metadata, None)
        .with_context(|| format!("failed to write updated metadata for {ref_str}"))?;

    Ok(())
}

/// Run the `annotate` subcommand: set or clear the freeform annotation on a
/// stored entry. Exactly one of `text` (Some) or `clear = true` must be given.
pub fn run_annotate(ref_str: String, text: Option<String>, clear: bool) -> Result<()> {
    if !clear && text.is_none() {
        // `docs/ERRORS.md` §4: a missing required argument is misuse, which
        // is exit 2. A bare `bail!` gave the generic 1 — the same gap #492
        // closed for an unparsable ref, one argument over.
        super::output::print_err(format_args!("error: provide annotation <text> or --clear"));
        return Err(anyhow::Error::new(super::fetch::CliExit(2)));
    }

    let ref_ = super::parse_ref_or_exit(&ref_str)?;
    let safekey = ref_.safekey();

    let store_root = resolve_store_root()?;
    let store = FsStore::new(store_root)?;

    let mut metadata = store
        .read(&safekey)
        .with_context(|| format!("failed to read store entry for {ref_str}"))?
        .with_context(|| {
            format!("no store entry for {ref_str}; run `doiget fetch {ref_str}` first")
        })?;

    {
        let ext = metadata.doiget.as_mut().with_context(|| {
            format!(
                "entry {ref_str} has no [doiget] table; \
                 run `doiget fetch {ref_str}` first"
            )
        })?;

        if clear {
            ext.annotation = None;
        } else if let Some(t) = text {
            if t.is_empty() {
                bail!(
                    "annotation text must not be empty; \
                     use --clear to remove the annotation"
                );
            }
            ext.annotation = Some(t);
        }
    }

    store
        .write_user_authored(&safekey, &metadata, None)
        .with_context(|| format!("failed to write updated metadata for {ref_str}"))?;

    Ok(())
}
