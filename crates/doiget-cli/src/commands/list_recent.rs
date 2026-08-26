//! `doiget list-recent [--limit=N]` subcommand — read-only most-recent
//! listing.
//!
//! Reads the configured store's `.metadata/` directory via
//! [`Store::list_recent`] and prints one row per entry on stdout, ordered
//! most-recent first by `[doiget].fetched_at`. Network access is never
//! required.
//!
//! Output is a tab-separated table with a header line. The five columns
//! match [`EntryInfo`](doiget_core::store::EntryInfo): `safekey`, `year`,
//! `title`, `fetched_at`, `pdf`. Missing `year` / `fetched_at` render as
//! `-`.
//!
//! `pdf` is #481. Without it this command -- the only one that answers
//! "what do I have?" without knowing the ref in advance -- showed a
//! metadata-only entry identically to a fetched paper. Batch fifty refs
//! with ten blocked, come back later, and the natural conclusion is fifty
//! papers.

use std::io::Write;

use anyhow::{Context, Result};

use doiget_core::store::{FsStore, Store};

use super::resolve_store_root;

/// Format string for [`chrono::DateTime`] columns. RFC3339-shaped, UTC, no
/// fractional seconds — matches the on-disk wire format from
/// [`docs/STORE.md`](../../../../docs/STORE.md) §2.
const FETCHED_AT_FMT: &str = "%Y-%m-%dT%H:%M:%SZ";

/// Run the `list-recent` subcommand against the configured store.
///
/// Emits a tab-separated table on stdout. The column order is intentionally
/// stable for `cut(1)` consumption; future fields will be APPENDED, not
/// inserted.
pub fn run(
    limit: usize,
    missing_pdf: bool,
    mode: super::output::OutputMode,
    quiet_was_explicit: bool,
) -> Result<()> {
    // `mode` honors ADR-0017: explicit `Quiet` suppresses the TSV table
    // but the store read still runs (so I/O failures surface as exit 1)
    // (#203). The non-TTY *implicit* Quiet does NOT suppress —
    // `list-recent` is artifact-class (ADR-0017 Amendment 2 / #301): the
    // listing IS the requested output. Json body is tracked in #204.
    let store_root = resolve_store_root()?;
    let store = FsStore::new(store_root)?;
    let mut entries = store
        .list_recent(limit)
        .context("failed to list recent store entries")?;
    // #481: "which of my fifty need retrying?" is the question the pdf
    // column creates, and scanning a fifty-row table by eye is the thing a
    // flag exists to avoid.
    if missing_pdf {
        entries.retain(|e| !e.has_pdf());
    }

    if mode == super::output::OutputMode::Quiet && quiet_was_explicit {
        return Ok(());
    }

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if mode == super::output::OutputMode::Json {
        // `EntryInfo` serialises `size_bytes` directly. `has_pdf` is
        // derived and added beside it, because a consumer should not have
        // to know that `0` and `null` both mean "no PDF" while meaning
        // different things about the entry.
        let entries_json: Vec<serde_json::Value> = entries
            .iter()
            .map(|e| {
                let mut v = serde_json::to_value(e)
                    .unwrap_or_else(|_| serde_json::json!({ "safekey": e.safekey.as_str() }));
                if let Some(o) = v.as_object_mut() {
                    o.insert("has_pdf".into(), serde_json::json!(e.has_pdf()));
                }
                v
            })
            .collect();
        let envelope = serde_json::json!({
            "ok": true,
            "count": entries_json.len(),
            "entries": entries_json,
        });
        let s = serde_json::to_string_pretty(&envelope)
            .context("failed to serialize list-recent entries to JSON")?;
        writeln!(out, "{s}").context("failed to write list-recent JSON to stdout")?;
        return Ok(());
    }
    writeln!(out, "safekey\tyear\ttitle\tfetched_at\tpdf")
        .context("failed to write list-recent header to stdout")?;
    for e in &entries {
        let year = e.year.map(|y| y.to_string()).unwrap_or_else(|| "-".into());
        let fetched = e
            .fetched_at
            .map(|t| t.format(FETCHED_AT_FMT).to_string())
            .unwrap_or_else(|| "-".into());
        writeln!(
            out,
            "{}\t{}\t{}\t{}\t{}",
            e.safekey.as_str(),
            year,
            e.title,
            fetched,
            pdf_cell(e)
        )
        .context("failed to write list-recent row to stdout")?;
    }
    Ok(())
}

/// Render the `pdf` column: the stored PDF size, or a dash for a
/// metadata-only entry.
///
/// #481. APPENDED rather than inserted before `title`, because this
/// module's contract is explicit that column order is stable for `cut(1)`
/// and new fields go on the end. The issue suggested inserting it; that
/// would break every existing `cut -f3` in the wild.
///
/// The size rather than a bare boolean: "1.0 MB" and "-" answer "did I get
/// it?" equally well, and the size additionally answers "is this the paper
/// or a one-page stub?", which is the next question and costs nothing.
pub(crate) fn pdf_cell(e: &doiget_core::store::EntryInfo) -> String {
    match e.size_bytes {
        // `None` is "no `[doiget]` table", a different unknown from
        // "0 bytes stored". Both mean no PDF; only one means the entry
        // lacks the table that would say so.
        None => "?".to_string(),
        Some(0) => "-".to_string(),
        #[allow(clippy::cast_precision_loss)]
        Some(n) if n >= 1_048_576 => format!("{:.1} MB", n as f64 / 1_048_576.0),
        #[allow(clippy::cast_precision_loss)]
        Some(n) if n >= 1024 => format!("{:.1} kB", n as f64 / 1024.0),
        Some(n) => format!("{n} B"),
    }
}
