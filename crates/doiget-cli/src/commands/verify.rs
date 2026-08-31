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
//! - **absent** — a well-formed id that the metadata source
//!   authoritatively reports does not exist (HTTP 404 / 410, surfaced as
//!   `ErrorCode::NotFound`). Network-independent and reproducible, so
//!   it is a definite dead reference and **always** counts toward the
//!   exit code — independent of `--strict`.
//! - **unreachable** — a well-formed id whose resolution failed for any
//!   transient reason (transport / DNS / TLS error, 429, 5xx, timeout).
//!   This is tolerated by default (a flaky network must not fail a build
//!   over a reference that is probably fine) and fails the run only under
//!   `--strict` (the network-stable lane that demands every id resolve).
//! - **unverifiable** — the entry carried no DOI / arXiv id at all.
//!   Warning by default; fails under `--strict` / `on_missing_id="error"`.
//!
//! The split between **absent** and **unreachable** is the load-bearing
//! distinction: it lets the default mode catch a genuinely dead DOI while
//! still passing when the network merely hiccuped on a real id.
//!
//! Exit code = number of failing entries, capped at 255 (mirrors
//! `doiget batch`). "Failing" = illegal + absent always; plus unreachable
//! when `--strict`; plus unverifiable when `--strict` **or**
//! `on_missing_id = "error"`. JSON-Lines (one record per entry) is written
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

/// Outcome class for one bibliography entry.
///
/// Single source of truth for the JSON-Lines `status` string
/// ([`Self::as_wire`]) and the exit-code policy ([`Self::is_failing`]),
/// so the wire format and the fail rule cannot drift apart as the
/// taxonomy evolves.
#[derive(Clone, Copy, PartialEq, Eq)]
enum VerifyStatus {
    /// Resolved to real metadata.
    Valid,
    /// Malformed id / unparsable input — a definite source error.
    Illegal,
    /// Authoritatively does not exist (`ErrorCode::NotFound`).
    Absent,
    /// Well-formed id, transient resolution failure.
    Unreachable,
    /// Entry carried no DOI / arXiv id.
    Unverifiable,
}

impl VerifyStatus {
    /// Every status, in summary-display order.
    const ALL: [VerifyStatus; 5] = [
        VerifyStatus::Valid,
        VerifyStatus::Illegal,
        VerifyStatus::Absent,
        VerifyStatus::Unreachable,
        VerifyStatus::Unverifiable,
    ];

    /// The public JSON-Lines `status` field value.
    fn as_wire(self) -> &'static str {
        match self {
            VerifyStatus::Valid => "valid",
            VerifyStatus::Illegal => "illegal",
            VerifyStatus::Absent => "absent",
            VerifyStatus::Unreachable => "unreachable",
            VerifyStatus::Unverifiable => "unverifiable",
        }
    }

    /// Stable index into a per-status counts array.
    fn index(self) -> usize {
        match self {
            VerifyStatus::Valid => 0,
            VerifyStatus::Illegal => 1,
            VerifyStatus::Absent => 2,
            VerifyStatus::Unreachable => 3,
            VerifyStatus::Unverifiable => 4,
        }
    }

    /// Does this outcome count toward the non-zero exit code?
    ///
    /// `illegal` + `absent` are definite, network-independent source
    /// errors → always fail. `unreachable` is transient → fails only in
    /// the network-stable `--strict` lane. `unverifiable` (no id) fails
    /// only when the id-less policy is `Error` (which `--strict` forces).
    fn is_failing(self, strict: bool, on_missing: OnMissingId) -> bool {
        match self {
            VerifyStatus::Valid => false,
            VerifyStatus::Illegal | VerifyStatus::Absent => true,
            VerifyStatus::Unreachable => strict,
            VerifyStatus::Unverifiable => on_missing == OnMissingId::Error,
        }
    }
}

/// Entry point for `doiget verify <path> [--format] [--strict]`.
pub async fn run(path: String, format: String, cli_strict: bool, mode: OutputMode) -> Result<()> {
    let fmt = parse_format(&format)?;
    // #477: the misuse form, not a raw `anyhow` dump. `docs/ERRORS.md` §4
    // classes an unusable argument as misuse (exit 2), and the closed
    // `ErrorCode` set has no member for "your input file is missing" -- it
    // describes fetch outcomes. Inventing one would be a wire change to a
    // NORMATIVE closed set and deserves its own decision, so this uses the
    // bare `error:` misuse shape the CLI already uses elsewhere (e.g.
    // `config --network` applied to the wrong action).
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => {
            super::output::print_err(format_args!(
                "error: failed to read reference file {path}: {e}"
            ));
            return Err(anyhow::Error::new(super::fetch::CliExit(2)));
        }
    };
    let entries = parse_input(&text, fmt, Some(Utf8Path::new(&path)));

    // Resolve effective policy: CLI `--strict` is the strictest setting,
    // forcing both unreachable and id-less entries to fail and overriding
    // the `[verify]` config.
    let config = load_verify_config();
    let strict = cli_strict || config.strict;
    let on_missing = if cli_strict {
        // CLI --strict is the strictest setting: id-less entries fail.
        OnMissingId::Error
    } else if strict {
        // strict came from `[verify] strict = true`: unreachable ids fail.
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

    // One counter per VerifyStatus, indexed by `VerifyStatus::index`.
    let mut counts = [0u32; VerifyStatus::ALL.len()];

    for entry in entries {
        // `on_missing_id = "skip"` drops id-less entries entirely —
        // before they are counted or emitted.
        if matches!(&entry, Err(ParseError::NoIdentifier { .. })) && on_missing == OnMissingId::Skip
        {
            continue;
        }
        let (status, record) = match entry {
            Ok(parsed) => {
                let ref_ = parsed.ref_;
                let entry_key = parsed.entry_key;
                match resolve_only(&ref_, &profile, &ctx).await {
                    Ok(_) => (
                        VerifyStatus::Valid,
                        serde_json::json!({
                            "ok": true,
                            "ref": ref_.as_input_str(),
                            "status": VerifyStatus::Valid.as_wire(),
                            "entry_key": entry_key,
                        }),
                    ),
                    Err(e) => {
                        let code: doiget_core::ErrorCode = (&e).into();
                        // A provenance-log write failure is fail-closed
                        // (docs/SECURITY.md §1.8): it is an operator-side
                        // fault, NOT a "this reference doesn't resolve"
                        // signal, so it must abort the run rather than be
                        // counted as a soft outcome that CI passes.
                        if code == doiget_core::ErrorCode::LogError {
                            return Err(anyhow::anyhow!(
                                "provenance log error during verify (aborting): {e}"
                            ));
                        }
                        // An InternalError is a bug, not a property of the
                        // reference; aborting (rather than silently bucketing
                        // it as a tolerable `unreachable`) surfaces it.
                        if code == doiget_core::ErrorCode::InternalError {
                            return Err(anyhow::anyhow!(
                                "internal error during verify (aborting; please report): {e}"
                            ));
                        }
                        // A NotFound (HTTP 404/410/451 or a source-specific
                        // absence) is an authoritative dead reference. Every
                        // other resolve error is transient (unreachable). See
                        // the module-level taxonomy.
                        let status = if code == doiget_core::ErrorCode::NotFound {
                            VerifyStatus::Absent
                        } else {
                            VerifyStatus::Unreachable
                        };
                        (
                            status,
                            serde_json::json!({
                                "ok": false,
                                "ref": ref_.as_input_str(),
                                "status": status.as_wire(),
                                "entry_key": entry_key,
                                "error": { "code": code.as_wire(), "message": e.to_string() },
                            }),
                        )
                    }
                }
            }
            Err(ParseError::InvalidRef {
                raw,
                entry_key,
                source,
            }) => (
                VerifyStatus::Illegal,
                serde_json::json!({
                    "ok": false,
                    "ref": raw,
                    "status": VerifyStatus::Illegal.as_wire(),
                    "entry_key": entry_key,
                    "error": { "code": "INVALID_REF", "message": source.to_string() },
                }),
            ),
            // #500: still unverifiable, but for a reason the reader can act on
            // -- and the action is not "fix the bibliography".
            Err(ParseError::UnsupportedIdentifier {
                kind,
                value,
                entry_key,
            }) => (
                VerifyStatus::Unverifiable,
                serde_json::json!({
                    "ok": false,
                    "ref": serde_json::Value::Null,
                    "status": VerifyStatus::Unverifiable.as_wire(),
                    "entry_key": entry_key,
                    "error": {
                        "code": "NOT_IMPLEMENTED",
                        "message": doiget_core::refs::unsupported_identifier_claim(
                            kind, &value,
                        ),
                    },
                }),
            ),
            Err(ParseError::NoIdentifier { entry_key }) => (
                VerifyStatus::Unverifiable,
                serde_json::json!({
                    "ok": false,
                    "ref": serde_json::Value::Null,
                    "status": VerifyStatus::Unverifiable.as_wire(),
                    "entry_key": entry_key,
                    "error": { "code": "INVALID_REF", "message": "entry has no DOI / arXiv id" },
                }),
            ),
            Err(ParseError::Decode { format, message }) => (
                VerifyStatus::Illegal,
                serde_json::json!({
                    "ok": false,
                    "status": VerifyStatus::Illegal.as_wire(),
                    "error": {
                        "code": "INVALID_REF",
                        "message": format!("input did not parse as {format}: {message}"),
                    },
                }),
            ),
            Err(ParseError::UnsupportedFormat { format }) => {
                bail!("{format} parsing is not supported for verification");
            }
            Err(_) => {
                // `ParseError` is #[non_exhaustive]; a future variant is
                // treated as a whole-input failure the operator must fix.
                bail!("reference file could not be parsed");
            }
        };
        counts[status.index()] += 1;
        #[allow(clippy::print_stdout)]
        {
            println!("{record}");
        }
    }

    let total = counts.iter().copied().fold(0u32, u32::saturating_add);
    if mode != OutputMode::Quiet {
        #[allow(clippy::print_stderr)]
        {
            eprintln!(
                "verify: {total} entries — {} valid, {} illegal, {} absent, \
                 {} unreachable, {} unverifiable{}",
                counts[VerifyStatus::Valid.index()],
                counts[VerifyStatus::Illegal.index()],
                counts[VerifyStatus::Absent.index()],
                counts[VerifyStatus::Unreachable.index()],
                counts[VerifyStatus::Unverifiable.index()],
                if strict { " (strict)" } else { "" }
            );
        }
    }

    // Sum the counts of every status whose policy marks it failing.
    let failing = VerifyStatus::ALL
        .iter()
        .filter(|s| s.is_failing(strict, on_missing))
        .map(|s| counts[s.index()])
        .fold(0u32, u32::saturating_add);
    if failing == 0 {
        Ok(())
    } else {
        Err(anyhow::Error::new(CliExit(failing.min(255) as i32)))
    }
}
