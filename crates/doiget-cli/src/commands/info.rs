//! `doiget info <ref>` subcommand — read-only metadata inspection.
//!
//! Reads the metadata TOML for a given [`Ref`] from the [`FsStore`] and
//! prints it on stdout. Network access is never required.
//!
//! Per [`docs/PUBLIC_API.md`](../../../../docs/PUBLIC_API.md) §2 the read goes
//! through the [`Store::read`] trait method, so any future store backend
//! (e.g. a SQLite index in Phase 2) is a drop-in replacement.

use std::io::Write;

use anyhow::{bail, Context, Result};

use doiget_core::store::{FsStore, Store};

use super::resolve_store_root;

/// Run the `info` subcommand against the configured store.
///
/// `input` is the user-supplied ref string (e.g. `"10.1234/example"`,
/// `"arxiv:2401.12345"`, or any of the schemes accepted by [`Ref::parse`]).
///
/// On success, the entry's [`Metadata`](doiget_core::store::Metadata) is
/// written to stdout as TOML. On a missing entry, the function returns an
/// error so the CLI exits non-zero — the caller (a shell pipeline) can
/// distinguish "entry not in store" from "entry was empty".
pub fn run(input: String, mode: super::output::OutputMode, quiet_was_explicit: bool) -> Result<()> {
    // `mode` honors ADR-0017: explicit `Quiet` skips emission but still
    // resolves the entry so the "not-found → exit non-zero" contract
    // continues to hold (#203). The non-TTY *implicit* Quiet does NOT
    // suppress — `info` is artifact-class (ADR-0017 Amendment 2 / #301):
    // its stdout IS the requested metadata, which an agent / pipe / ssh
    // caller needs. `Json` serialises `Metadata` directly (it carries
    // `Serialize`); the wire form is the field-name JSON of the on-disk
    // TOML (#204).
    let ref_ = super::parse_ref_or_exit(&input)?;
    let safekey = ref_.safekey();

    let store_root = resolve_store_root()?;
    let store = FsStore::new(store_root)?;

    let metadata = store
        .read(&safekey)
        .with_context(|| format!("failed to read store entry for {input}"))?;

    match metadata {
        Some(m) => {
            if mode == super::output::OutputMode::Quiet && quiet_was_explicit {
                // Explicit Quiet only (--quiet / -q / --mode quiet /
                // DOIGET_MODE=quiet): existence-check, exit 0, no stdout.
                return Ok(());
            }
            // Workspace lints deny `print_stdout` (the `print!`/`println!`
            // macros) so JSON-RPC frames never collide with diagnostics.
            // `writeln!` / `write!` against an explicit `stdout().lock()`
            // is the sanctioned escape hatch — the caller chose stdout
            // explicitly. See `docs/SECURITY.md` §3 / ADR-0001.
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            if mode == super::output::OutputMode::Json {
                let envelope = serde_json::json!({
                    "ok": true,
                    "ref": input,
                    "safekey": safekey.as_str(),
                    "metadata": m,
                });
                let s = serde_json::to_string_pretty(&envelope)
                    .context("failed to serialize metadata to JSON for stdout")?;
                writeln!(out, "{s}").context("failed to write metadata JSON to stdout")?;
                return Ok(());
            }
            // Human (default): re-serialize to TOML for stdout. We use
            // `toml::to_string_pretty` for human readability; this is NOT
            // the §7-normalized form written to disk, but `info` is a
            // presentation surface, not a round-tripper.
            let s = toml::to_string_pretty(&m)
                .context("failed to serialize metadata to TOML for stdout")?;
            write!(out, "{s}").context("failed to write metadata to stdout")?;
            // toml::to_string_pretty already emits a trailing newline, but
            // be defensive in case a future toml-rs revision changes that.
            if !s.ends_with('\n') {
                writeln!(out).context("failed to write trailing newline")?;
            }
            Ok(())
        }
        None => bail!("no entry for {input}"),
    }
}
