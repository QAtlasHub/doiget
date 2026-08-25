//! Subcommand implementations for the `doiget` CLI.
//!
//! Each module corresponds to a single `clap` subcommand declared in
//! `main.rs`. The dispatch table in `main.rs` calls `run(...)` on the
//! matching module. Subcommands return `anyhow::Result<()>`; any error
//! surfaces via the CLI's top-level error reporter (stderr).
//!
//! ## Phase 1 surface (so far)
//!
//! - [`audit_log`] — `doiget audit-log --verify` recomputes the SHA-256 hash
//!   chain on the provenance log and reports any mismatches.
//! - [`batch`] — `doiget batch <path>` multi-ref orchestrator (rate-bounded).
//! - [`bib`] — `doiget bib <ref>` BibTeX exporter (Phase 2 starter).
//! - [`cite`] — `doiget cite <ref>` live-resolve BibTeX (doi2bib-style).
//! - [`config`] — `doiget config show/path/doctor`.
//! - [`csl`] — `doiget csl <ref>` exports a stored entry as CSL JSON 1.0.
//! - [`fetch`] — `doiget fetch <ref>` orchestrator (arXiv E2E + DOI metadata-only).
//! - [`info`] — prints a stored entry's `Metadata` as TOML on stdout.
//! - [`list_recent`] — prints up to N most-recently-fetched entries.
//! - [`search`] — case-insensitive substring search over stored metadata.
//!
//! Other subcommands (`serve`) land in separate PRs.

/// Parse a ref, or render the failure in the `docs/ERRORS.md` §3
/// "Researcher (CLI human)" form and return the CLI exit error.
///
/// #477. `error[CODE]: message` held on `fetch` alone -- #119 did it there
/// and nowhere else -- while `info`, `link`, `cite`, `text`, `tag`, `bib`,
/// `csl` and `source` each wrote their own `with_context("invalid ref: …")`
/// and let `anyhow` print a bare `Error:` plus a `Caused by:` chain. The
/// closed error-code set is a load-bearing promise of this project: a
/// caller is told it can key off `error[CODE]:`, and on eight of nine
/// commands there was no code to key off, while the `Caused by:` chain
/// leaked internal error types that are in no contract.
///
/// Same shape as `render_fetch_error` and for the same reason -- one
/// renderer, so a future change to the contract cannot reach some call
/// sites and miss others.
///
/// # Errors
///
/// Always, when parsing fails: an [`anyhow::Error`] wrapping
/// [`CliExit`](fetch::CliExit) with the `INVALID_REF` exit code. The
/// message has already been written to stderr.
pub fn parse_ref_or_exit(input: &str) -> anyhow::Result<doiget_core::Ref> {
    match doiget_core::Ref::parse(input) {
        Ok(r) => Ok(r),
        Err(e) => {
            render_ref_parse_error(&e);
            Err(anyhow::Error::new(fetch::CliExit(fetch::cli_exit_code(
                doiget_core::ErrorCode::InvalidRef,
            ))))
        }
    }
}

/// The renderer on its own, for the two call sites that already choose
/// their own exit code.
///
/// `fetch` exits 1 (`cli_exit_code(InvalidRef)`) and `graph` exits 2,
/// citing `docs/ERRORS.md` §4 "misuse" -- and §4 does say an unparsable
/// argument is misuse, so they disagree and `graph` is the one following
/// the table. Filed separately rather than changed here: #477 is about the
/// message, `fetch`'s exit 1 is pinned by a named test, and quietly moving
/// an exit code is how a script breaks without anyone noticing.
///
/// One renderer either way, which is the part that was missing.
pub fn render_ref_parse_error(e: &doiget_core::RefParseError) {
    output::print_err(format_args!(
        "error[{}]: invalid ref: {e}",
        doiget_core::ErrorCode::InvalidRef.as_wire()
    ));
}

pub mod audit_log;
pub mod batch;
pub mod bib;
pub mod capabilities;
pub mod cite;
pub mod config;
pub mod csl;
pub mod fetch;
pub mod frontier;
pub mod info;
pub mod link;
pub mod lint;
pub mod list_recent;
pub mod output;
pub mod provenance;
pub mod resolve_citation;
pub mod search;
pub mod source;
pub mod tag;
pub mod tex_source;
pub mod text;
pub mod verify;
pub mod version;

// Phase 4 / Slice 16. Compile-gated by the `citation` Cargo feature
// (which itself enables `doiget-core/citation`).
#[cfg(feature = "citation")]
pub mod graph;

use anyhow::{Context, Result};
use camino::Utf8PathBuf;

/// Resolve the path of the user `config.toml`, for messages that need to
/// name the file the user must edit.
///
/// Same resolution as [`config::ResolvedConfig::from_env`]
/// (`<dirs::config_dir()>/doiget/config.toml`), kept here so the fetch-side
/// denial help (issue #405) and `config doctor` cannot drift. Returns
/// `None` only when the platform has no config dir at all, in which case
/// callers should fall back to naming the file generically — a missing
/// config dir must never turn an advisory line into a hard error.
pub(crate) fn user_config_path() -> Option<Utf8PathBuf> {
    // MUST stay `config_dir_utf8` — the resolver the READER uses. This
    // shipped as `dirs::config_dir()`, which ignores `XDG_CONFIG_HOME` on
    // Windows, so the denial help named `%APPDATA%\doiget\config.toml`
    // while `build_http_client` was loading the XDG one. Naming the wrong
    // file is worse than naming none, and it is the whole point of the
    // #405 help line. Same fix as `ResolvedConfig::from_env`.
    Some(
        fetch::config_dir_utf8()
            .ok()?
            .join("doiget")
            .join("config.toml"),
    )
}

/// Where a resolved store root came from (#441).
///
/// Reported by `doiget config doctor` so that a setting which did nothing
/// can no longer look like a setting that worked. That was the sharpest
/// part of #441: `config init` recommended `[store] root`, `doctor`
/// confirmed the recommendation, and the value was never read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StoreRootSource {
    /// `DOIGET_STORE_ROOT` (also how `--store-root` is applied).
    Env,
    /// `[store] root` in the user's `config.toml`.
    ConfigFile,
    /// Nothing set it — `./papers` under the cwd (ADR-0036).
    CwdDefault,
}

impl StoreRootSource {
    /// Short label for `config show` / `doctor`.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Env => "DOIGET_STORE_ROOT",
            Self::ConfigFile => "[store] root in config.toml",
            Self::CwdDefault => "default: ./papers under the cwd",
        }
    }
}

/// Resolve the on-disk store root.
///
/// Resolution order (ADR-0036, `docs/CONFIG.md` §4):
///
/// 1. `DOIGET_STORE_ROOT`, if set and non-empty. `--store-root` is applied
///    by writing this variable, so the flag rides the same rung.
/// 2. `[store] root` in the user's `config.toml`, expanded via
///    [`doiget_core::user_extension::expand_store_root`].
/// 3. `./papers` — `papers/` directly under the current working directory
///    (#344 / ADR-0036), so fetched artifacts are visible where the user
///    (or an LLM agent) is working rather than hidden in a far-off home
///    directory.
///
/// Rung 2 is new in #441. It was documented from the start — ADR-0036
/// states the order, `docs/CONFIG.md` §3 lists the key, `config init`
/// writes it into the template it generates and `config doctor` recommends
/// it — and read by nothing, so the store silently kept following the cwd.
/// The old doc comment on this function said the config rung "lands with
/// the `config` subcommand"; that subcommand shipped in 0.8.8 without it.
pub(crate) fn resolve_store_root() -> Result<Utf8PathBuf> {
    resolve_store_root_with_source().map(|(root, _)| root)
}

/// [`resolve_store_root`] plus which rung answered.
pub(crate) fn resolve_store_root_with_source() -> Result<(Utf8PathBuf, StoreRootSource)> {
    if let Ok(s) = std::env::var("DOIGET_STORE_ROOT") {
        // Ignore an empty value or an unexpanded "${...}" placeholder — a
        // Desktop-Extension config left blank can pass the literal
        // "${user_config.store_root}", which must not become a path (#369).
        let s = s.trim();
        if !s.is_empty() && !s.contains("${") {
            return Ok((Utf8PathBuf::from(s), StoreRootSource::Env));
        }
    }
    if let Some(root) = store_root_from_config() {
        return Ok((root, StoreRootSource::ConfigFile));
    }
    let cwd = std::env::current_dir().context(
        "could not determine the current working directory for the default store root (set \
            DOIGET_STORE_ROOT to choose an explicit store location)",
    )?;
    Utf8PathBuf::from_path_buf(cwd)
        .map(|d| (d.join("papers"), StoreRootSource::CwdDefault))
        .map_err(|p| anyhow::anyhow!("current directory path is not UTF-8: {}", p.display()))
}

/// `[store] root` from the user's `config.toml`, if any.
///
/// Does not fail the command on a malformed file — the store root is
/// resolved by every subcommand, and one bad line should not be a total
/// outage — but it does NOT stay quiet about it.
///
/// The previous comment here justified silence by claiming "a parse error
/// surfaces with a proper diagnostic on the network path that owns this
/// file". The #468 review checked that claim and it is false for most
/// callers: `list-recent`, `search`, `info`, `tag`, `bib` and `csl` never
/// build an HTTP client, so that diagnostic never runs for them. TOML fails
/// the whole document, so a typo anywhere — even under `[network]` — makes
/// `[store] root` unreadable, and the command silently used `./papers`
/// under the cwd as though nothing had been configured.
///
/// `search` then reports zero results, indistinguishable from an empty
/// library. Worse, `tag` **writes** metadata into the wrong store and
/// reports success. A warning is the minimum; the resolved source is also
/// reported by `config doctor`.
fn store_root_from_config() -> Option<Utf8PathBuf> {
    let path = user_config_path()?;
    let cfg = match doiget_core::user_extension::load(&path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                path = %path,
                error = %e,
                "config.toml could not be read; [store] root ignored and the default store                  root used instead. Run `doiget config doctor` to see which root is in effect."
            );
            return None;
        }
    };
    let raw = cfg.store_root?;
    Some(doiget_core::user_extension::expand_store_root(&raw))
}
