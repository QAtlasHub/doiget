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

use std::sync::Arc;

use anyhow::{bail, Context, Result};
use camino::Utf8Path;

use doiget_core::orchestrator::resolve_only;
use doiget_core::refs::{parse_input, Format, ParseError};
use doiget_core::source::FetchContext;
use doiget_core::{CapabilityProfile, RateLimits};

use super::fetch::CliExit;
use super::output::OutputMode;

/// Build a metadata-resolution context: HTTP client, rate limiter, and
/// provenance log resolved from the environment. Mirrors
/// `resolve_citation::build_context`; verify never persists to the store,
/// so no store handle is constructed.
fn build_context() -> Result<FetchContext> {
    let session_id = crate::commands::fetch::new_session_id();
    let log_path = crate::commands::fetch::resolve_log_path()?;
    let http = Arc::new(crate::commands::fetch::build_http_client()?);
    let rate_limiter = Arc::new(doiget_core::rate_limiter::RateLimiter::new(
        RateLimits::HARD_CODED,
    ));
    let log = Arc::new(
        doiget_core::provenance::ProvenanceLog::open(log_path, session_id.clone())
            .context("failed to open provenance log for verify")?,
    );
    Ok(FetchContext {
        http,
        rate_limiter,
        log,
        session_id,
    })
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
pub async fn run(path: String, format: String, strict: bool, mode: OutputMode) -> Result<()> {
    let fmt = parse_format(&format)?;
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read reference file {path}"))?;
    let entries = parse_input(&text, fmt, Some(Utf8Path::new(&path)));

    let ctx = build_context()?;
    let profile = CapabilityProfile::from_env().context("resolving capability profile")?;

    let mut valid = 0u32;
    let mut illegal = 0u32;
    let mut unresolved = 0u32;
    let mut unverifiable = 0u32;

    for entry in entries {
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
                        unresolved += 1;
                        let code: doiget_core::ErrorCode = (&e).into();
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

    let failing = illegal + if strict { unresolved + unverifiable } else { 0 };
    if failing == 0 {
        Ok(())
    } else {
        Err(anyhow::Error::new(CliExit(failing.min(255) as i32)))
    }
}
