//! `doiget verify <path>` — check that every DOI / arXiv reference in a
//! bibliography file resolves to real metadata, WITHOUT downloading any
//! PDF or writing to the store.
//!
//! Each entry is classified:
//!
//! - **valid** — the id resolved to metadata (Crossref / arXiv).
//! - **illegal** — the id is malformed (`Ref::parse` rejected it, e.g. a
//!   typo like `1O.1234`), or the whole file failed to parse. Always
//!   counts toward the exit code: a malformed id is a definite source
//!   error, independent of the network.
//! - **unresolved** — a well-formed id that did not resolve (it does not
//!   exist, OR a transient network failure). The current `ErrorCode` set
//!   does not distinguish "404 absent" from "network blip", so this is a
//!   warning by default and only fails the run under `--strict` (intended
//!   for a network-stable CI lane).
//! - **unverifiable** — the entry carried no DOI / arXiv id at all.
//!   Warning by default; fails under `--strict`.
//!
//! Exit code = number of failing entries, capped at 255 (mirrors
//! `doiget batch`). "Failing" = illegal, plus unresolved + unverifiable
//! when `--strict` is set. JSON-Lines (one record per entry) is written
//! to stdout regardless of mode; the summary goes to stderr unless
//! `--quiet`.

use anyhow::{bail, Context, Result};
use camino::Utf8Path;

use doiget_core::orchestrator::resolve_only;
use doiget_core::refs::{parse_input, Format, ParseError};
use doiget_core::verify_config::{self, OnMissingId};
use doiget_core::CapabilityProfile;

use super::fetch::CliExit;
use super::output::OutputMode;

/// Resolve the `[verify]` config from `<config_dir>/doiget/config.toml`.
/// Best-effort: a missing file is defaults; a malformed file degrades to
/// defaults with a stderr warning rather than aborting the run.
fn load_verify_config() -> verify_config::VerifyConfig {
    let path = match crate::commands::fetch::config_dir_utf8() {
        Ok(dir) => dir.join("doiget").join("config.toml"),
        Err(_) => return verify_config::VerifyConfig::default(),
    };
    match verify_config::load(&path) {
        Ok(cfg) => cfg,
        Err(e) => {
            #[allow(clippy::print_stderr)]
            {
                eprintln!("warning: ignoring [verify] config: {e}");
            }
            verify_config::VerifyConfig::default()
        }
    }
}

/// Map the `--format` flag token to a [`Format`].
fn parse_format(s: &str) -> Result<Format> {
    match s {
        "auto" => Ok(Format::Auto),
        "refs" => Ok(Format::Refs),
        "csl-json" => Ok(Format::CslJson),
        "bibtex" => Ok(Format::Bibtex),
        other => bail!("unknown --format {other:?} (expected auto|refs|csl-json|bibtex)"),
    }
}

/// Entry point for `doiget verify <path> [--format] [--strict]`.
pub async fn run(path: String, format: String, cli_strict: bool, mode: OutputMode) -> Result<()> {
    let fmt = parse_format(&format)?;
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read reference file {path}"))?;
    let entries = parse_input(&text, fmt, Some(Utf8Path::new(&path)));

    // Resolve effective policy: CLI `--strict` is the strictest setting,
    // forcing both unresolved and id-less entries to fail and overriding
    // the `[verify]` config.
    let config = load_verify_config();
    let strict = cli_strict || config.strict;
    let on_missing = if cli_strict {
        // CLI --strict is the strictest setting: id-less entries fail.
        OnMissingId::Error
    } else if strict {
        // strict came from `[verify] strict = true`: unresolved ids fail.
        // Do not let `skip` silently drop id-less entries in a strict run —
        // surface them at least as a warning so the summary is honest.
        match config.on_missing_id {
            OnMissingId::Skip => OnMissingId::Warn,
            other => other,
        }
    } else {
        config.on_missing_id
    };

    let ctx = crate::commands::fetch::build_resolve_context()?;
    let profile = CapabilityProfile::from_env().context("resolving capability profile")?;

    let mut valid = 0u32;
    let mut illegal = 0u32;
    let mut unresolved = 0u32;
    let mut unverifiable = 0u32;

    for entry in entries {
        // `on_missing_id = "skip"` drops id-less entries entirely —
        // before they are counted or emitted.
        if matches!(&entry, Err(ParseError::NoIdentifier { .. })) && on_missing == OnMissingId::Skip
        {
            continue;
        }
        let record = match entry {
            Ok(parsed) => {
                let ref_ = parsed.ref_;
                let entry_key = parsed.entry_key;
                match resolve_only(&ref_, &profile, &ctx).await {
                    Ok(_) => {
                        valid += 1;
                        serde_json::json!({
                            "ok": true,
                            "ref": ref_.as_input_str(),
                            "status": "valid",
                            "entry_key": entry_key,
                        })
                    }
                    Err(e) => {
                        let code: doiget_core::ErrorCode = (&e).into();
                        // A provenance-log write failure is fail-closed
                        // (docs/SECURITY.md §1.8): it is an operator-side
                        // fault, NOT a "this reference doesn't resolve"
                        // signal, so it must abort the run rather than be
                        // counted as a soft `unresolved` that CI passes.
                        if code == doiget_core::ErrorCode::LogError {
                            return Err(anyhow::anyhow!(
                                "provenance log error during verify (aborting): {e}"
                            ));
                        }
                        unresolved += 1;
                        serde_json::json!({
                            "ok": false,
                            "ref": ref_.as_input_str(),
                            "status": "unresolved",
                            "entry_key": entry_key,
                            "error": { "code": code.as_wire(), "message": e.to_string() },
                        })
                    }
                }
            }
            Err(ParseError::InvalidRef {
                raw,
                entry_key,
                source,
            }) => {
                illegal += 1;
                serde_json::json!({
                    "ok": false,
                    "ref": raw,
                    "status": "illegal",
                    "entry_key": entry_key,
                    "error": { "code": "INVALID_REF", "message": source.to_string() },
                })
            }
            Err(ParseError::NoIdentifier { entry_key }) => {
                unverifiable += 1;
                serde_json::json!({
                    "ok": false,
                    "ref": serde_json::Value::Null,
                    "status": "unverifiable",
                    "entry_key": entry_key,
                    "error": { "code": "INVALID_REF", "message": "entry has no DOI / arXiv id" },
                })
            }
            Err(ParseError::Decode { format, message }) => {
                illegal += 1;
                serde_json::json!({
                    "ok": false,
                    "status": "illegal",
                    "error": {
                        "code": "INVALID_REF",
                        "message": format!("input did not parse as {format}: {message}"),
                    },
                })
            }
            Err(ParseError::UnsupportedFormat { format }) => {
                bail!("{format} parsing is not supported for verification");
            }
            Err(_) => {
                // `ParseError` is #[non_exhaustive]; a future variant is
                // treated as a whole-input failure the operator must fix.
                bail!("reference file could not be parsed");
            }
        };
        #[allow(clippy::print_stdout)]
        {
            println!("{record}");
        }
    }

    let total = valid + illegal + unresolved + unverifiable;
    if mode != OutputMode::Quiet {
        #[allow(clippy::print_stderr)]
        {
            eprintln!(
                "verify: {total} entries — {valid} valid, {illegal} illegal, \
                 {unresolved} unresolved, {unverifiable} unverifiable{}",
                if strict { " (strict)" } else { "" }
            );
        }
    }

    let failing = illegal
        + if strict { unresolved } else { 0 }
        + if on_missing == OnMissingId::Error {
            unverifiable
        } else {
            0
        };
    if failing == 0 {
        Ok(())
    } else {
        Err(anyhow::Error::new(CliExit(failing.min(255) as i32)))
    }
}
