//! Output-mode resolution for the `doiget` CLI (ADR-0017, #144;
//! Amendment 1 = #219/#220).
//!
//! ADR-0017 specifies the precedence ladder
//! `--mode > --json/--quiet > DOIGET_MODE env > subcommand-implicit > TTY > quiet`.
//! CONFIG.md §5 additionally pins `doiget serve` to `mcp` mode regardless
//! of flags — a load-bearing security invariant (the stdout-purity Slice 9
//! CI job already enforces "MCP mode forbids non-JSON stdout"). The
//! `forced_implicit` parameter to [`resolve`] expresses that override: when
//! `Some(_)`, it overrides everything else.
//!
//! [`resolve`] returns a [`ResolvedOutput`] carrying the [`OutputMode`]
//! plus a `quiet_was_explicit` discriminator. The distinction is
//! load-bearing per ADR-0017 Amendment 1: artifact-producing commands
//! (`bib` / `csl` / `capabilities` / `audit-log --verify --mode json`)
//! suppress only on **explicit** Quiet (`--quiet` / `-q` /
//! `DOIGET_MODE=quiet` / `--mode quiet`), not on the non-TTY default.
//! Informational commands continue to suppress on any Quiet.
//!
//! Resolution is split into a pure function ([`resolve`]) plus a thin
//! TTY-detection wrapper ([`stdout_is_tty`]) so the ladder is fully
//! unit-testable without environment manipulation.
//!
//! # Per-mode honoring across the CLI surface
//!
//! - `Human` — default for TTY stdout. Human-readable text, the
//!   pre-#144 behaviour.
//! - `Quiet` — stdout suppressed for *informational* commands whose
//!   stdout is a status report (audit-log Human / config show / config
//!   path / provenance migrate / fetch + batch status) per #203. Errors
//!   (stderr) and exit codes are unaffected. *Artifact* commands —
//!   whose stdout IS the requested product — suppress ONLY on
//!   **explicit** Quiet, never on the non-TTY implicit fallback:
//!   export/inventory (bib / csl / capabilities) per Amendment 1, and
//!   read/inspection (info / list-recent / search / link) per
//!   Amendment 2 (#301). See [`is_artifact_command`].
//! - `Json` — structured JSON bodies for the human-table commands
//!   (#204) plus the ERRORS.md §3 JSON-Lines per-ref shape for batch
//!   (#205). Single-value-per-stdout for the table commands;
//!   line-oriented for batch.
//! - `Mcp` — JSON-RPC framing on stdout (only reachable via
//!   `doiget serve`; forced by `forced_implicit_for` in `main.rs`).
//!
//! # JSON wire conventions (a single-line note)
//!
//! Two intentional conventions live side-by-side in the codebase, and
//! they are different on purpose:
//!
//! 1. **Pretty-printed single value** for the table commands' `--mode
//!    json` bodies (info / list-recent / search / config show /
//!    audit-log / provenance migrate). Optimised for `| jq .` and
//!    human-on-a-screen reading.
//! 2. **Compact JSON-Lines** for `batch --mode json` per the
//!    ERRORS.md §3 CI persona — one record per stdout line, no embedded
//!    newlines, so a consumer can `split('\n').map(json.loads)`.
//!
//! Future-maintainer reminder: do NOT unify these by accident — they
//! serve different consumers.

use clap::ValueEnum;

/// The four output modes from `docs/CONFIG.md` §3 / ADR-0017.
///
/// - `Human`: line-oriented text, intended for a terminal.
/// - `Json`: structured machine output (where the command supports it).
/// - `Quiet`: no informational stdout; errors still go to stderr.
/// - `Mcp`: JSON-RPC framing on stdout (forbidden for non-`serve`
///   commands; entered only via the `Serve` subcommand).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[clap(rename_all = "lower")]
pub enum OutputMode {
    /// Line-oriented text output, intended for a terminal.
    Human,
    /// Structured JSON output (where the command supports it).
    Json,
    /// No informational stdout; errors still go to stderr.
    Quiet,
    /// JSON-RPC framing on stdout (only via `doiget serve`).
    Mcp,
}

/// Which short-form implication, if any, was given on the command line.
///
/// `--mode <m>` carries an explicit [`OutputMode`]; `--json` / `--quiet`
/// are short-form implications per CONFIG.md §5. Mutual exclusion among
/// the three flags is enforced at the clap layer via `conflicts_with`,
/// so [`resolve`]'s caller is guaranteed to pass at most one of the
/// three.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlagInput {
    /// `--mode <human|json|quiet|mcp>` was given.
    Explicit(OutputMode),
    /// `--json` was given (implies `Json`).
    JsonShort,
    /// `--quiet`/`-q` was given (implies `Quiet`).
    QuietShort,
    /// No mode-related flag was given.
    None,
}

/// The resolved output state per ADR-0017 Amendment 1: the
/// [`OutputMode`] the resolution ladder picked, plus a
/// `quiet_was_explicit` discriminator that distinguishes the user's
/// **explicit** request for silence (`--quiet` / `-q` /
/// `DOIGET_MODE=quiet` / `--mode quiet`) from the resolver's
/// **implicit** fallback to Quiet when stdout is not a TTY.
///
/// Per ADR-0017 Amendment 1 (extended by Amendment 2, #301),
/// *informational* commands whose stdout is a status report (audit-log
/// Human, config show/path, provenance migrate, fetch/batch status)
/// suppress on any Quiet; *artifact* commands whose stdout IS the
/// product — `bib` / `csl` / `capabilities` plus the read/inspection
/// commands `info` / `list-recent` / `search` / `link`, and
/// `audit-log --verify --mode json` — suppress only on **explicit**
/// Quiet. See [`is_artifact_command`]. The wire format of
/// [`OutputMode`] (`DOIGET_MODE` string values, the `modes` array in
/// `capabilities` JSON, the `--mode` clap values) is **unchanged**;
/// this struct lives only in-memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedOutput {
    /// The effective mode for this invocation.
    pub mode: OutputMode,
    /// `true` iff the user supplied an explicit Quiet signal
    /// (`--quiet`, `-q`, `--mode quiet`, `DOIGET_MODE=quiet`).
    /// `false` for the non-TTY fallback to Quiet, and for any
    /// non-Quiet mode.
    pub quiet_was_explicit: bool,
}

/// `true` if `name` identifies an artifact-producing subcommand whose
/// stdout output IS the deliverable (per ADR-0017 Amendment 1, extended
/// by Amendment 2). Artifact commands suppress only on **explicit** Quiet
/// ([`ResolvedOutput::quiet_was_explicit`] == `true`), never on the
/// non-TTY implicit fallback.
///
/// The set has two cohorts:
/// - **Export / inventory** (`bib` / `csl` / `capabilities`) — Amendment 1.
/// - **Read / inspection** (`info` / `list-recent` / `search` / `link`) —
///   Amendment 2 (#301). For these the stdout rendering IS the requested
///   data, not a status report, so a non-TTY caller (agent / pipe / ssh)
///   must still receive it; silencing it reads as "fetch failed" or
///   "store empty" when the data is present and correct.
///
/// `audit-log` is omitted on purpose: it is *informational* in Human
/// mode and *artifact* in Json mode; the command checks the resolved
/// mode rather than its name, so this classifier doesn't apply.
pub fn is_artifact_command(name: &str) -> bool {
    matches!(
        name,
        "bib" | "csl" | "capabilities" | "info" | "list-recent" | "search" | "link"
    )
}

/// Resolve the effective [`OutputMode`] per ADR-0017 and the
/// `quiet_was_explicit` discriminator per ADR-0017 Amendment 1.
///
/// Precedence (highest first):
///
/// 1. `forced_implicit` — a subcommand-pinned mode that overrides
///    everything (e.g. `doiget serve` → `Mcp` per CONFIG.md §5; required
///    for the Slice 9 stdout-purity invariant). Pinned modes are
///    **never** counted as explicit user Quiet; they are system policy.
/// 2. `flag` — `--mode` / `--json` / `--quiet` on the command line.
///    A `Quiet` mode reached via `--mode quiet`, `--quiet`, or `-q`
///    is **explicit**.
/// 3. `env` — `DOIGET_MODE` (parsed by [`parse_env_mode`]; unrecognised
///    values are ignored, matching CONFIG.md §4's "doiget reads only the
///    keys it knows about" posture). A `Quiet` mode reached via
///    `DOIGET_MODE=quiet` is **explicit**.
/// 4. `is_tty` — `Human` when stdout is a terminal, otherwise `Quiet`
///    (CONFIG.md §3.b's "implicit + TTY > quiet (default)"). A `Quiet`
///    mode reached this way is **implicit**.
///
/// This function is pure: no env reads, no I/O. The caller plumbs
/// `env::var("DOIGET_MODE").ok()` and an `is_tty` probe in.
pub fn resolve(
    forced_implicit: Option<OutputMode>,
    flag: FlagInput,
    env: Option<&str>,
    is_tty: bool,
) -> ResolvedOutput {
    if let Some(m) = forced_implicit {
        return ResolvedOutput {
            mode: m,
            quiet_was_explicit: false,
        };
    }
    let (mode, quiet_was_explicit) = match flag {
        FlagInput::Explicit(OutputMode::Quiet) => (OutputMode::Quiet, true),
        FlagInput::Explicit(m) => (m, false),
        FlagInput::JsonShort => (OutputMode::Json, false),
        FlagInput::QuietShort => (OutputMode::Quiet, true),
        FlagInput::None => match env.and_then(parse_env_mode) {
            Some(OutputMode::Quiet) => (OutputMode::Quiet, true),
            Some(m) => (m, false),
            None => {
                if is_tty {
                    (OutputMode::Human, false)
                } else {
                    (OutputMode::Quiet, false)
                }
            }
        },
    };
    ResolvedOutput {
        mode,
        quiet_was_explicit,
    }
}

/// Parse a `DOIGET_MODE` env-var value. Recognises the four
/// CONFIG.md §3 modes case-insensitively; returns `None` for empty,
/// whitespace-only, or unrecognised input (the resolution ladder then
/// falls through to TTY detection).
pub fn parse_env_mode(s: &str) -> Option<OutputMode> {
    match s.trim().to_ascii_lowercase().as_str() {
        "human" => Some(OutputMode::Human),
        "json" => Some(OutputMode::Json),
        "quiet" => Some(OutputMode::Quiet),
        "mcp" => Some(OutputMode::Mcp),
        _ => None,
    }
}

/// `true` if stdout is attached to a terminal. Wraps the standard
/// library's [`std::io::IsTerminal`] probe; the trait is in scope only
/// here so test code can call [`resolve`] with a synthetic `is_tty`
/// boolean without linking to `IsTerminal`.
pub fn stdout_is_tty() -> bool {
    use std::io::IsTerminal;
    std::io::stdout().is_terminal()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- forced_implicit overrides everything ---------------------------

    #[test]
    fn forced_mcp_wins_over_flag_env_and_tty() {
        // `doiget serve` MUST be `Mcp` even if the user passes
        // `--mode quiet` or sets `DOIGET_MODE=human` (CONFIG.md §5).
        let out = resolve(
            Some(OutputMode::Mcp),
            FlagInput::Explicit(OutputMode::Quiet),
            Some("human"),
            true,
        );
        assert_eq!(out.mode, OutputMode::Mcp);
        assert!(
            !out.quiet_was_explicit,
            "forced_implicit is system policy, never explicit user Quiet"
        );
    }

    // ---- flag > env > tty ---------------------------------------------

    #[test]
    fn explicit_flag_wins_over_env_and_tty() {
        let out = resolve(
            None,
            FlagInput::Explicit(OutputMode::Json),
            Some("human"),
            true,
        );
        assert_eq!(out.mode, OutputMode::Json);
        assert!(!out.quiet_was_explicit);
    }

    #[test]
    fn json_short_flag_implies_json() {
        let out = resolve(None, FlagInput::JsonShort, Some("human"), true);
        assert_eq!(out.mode, OutputMode::Json);
        assert!(!out.quiet_was_explicit);
    }

    #[test]
    fn quiet_short_flag_implies_quiet() {
        let out = resolve(None, FlagInput::QuietShort, Some("human"), true);
        assert_eq!(out.mode, OutputMode::Quiet);
        assert!(
            out.quiet_was_explicit,
            "`--quiet`/`-q` is an explicit Quiet signal"
        );
    }

    #[test]
    fn env_wins_when_no_flag() {
        let out = resolve(None, FlagInput::None, Some("json"), true);
        assert_eq!(out.mode, OutputMode::Json);
        assert!(!out.quiet_was_explicit);
    }

    #[test]
    fn env_is_case_insensitive_and_trims_whitespace() {
        assert_eq!(parse_env_mode("HUMAN"), Some(OutputMode::Human));
        assert_eq!(parse_env_mode("  Json  "), Some(OutputMode::Json));
        assert_eq!(parse_env_mode("MCP"), Some(OutputMode::Mcp));
    }

    #[test]
    fn unrecognised_env_falls_through_to_tty() {
        // `DOIGET_MODE=garbage` is ignored, ladder continues to TTY.
        let tty = resolve(None, FlagInput::None, Some("garbage"), true);
        let pipe = resolve(None, FlagInput::None, Some("garbage"), false);
        assert_eq!(tty.mode, OutputMode::Human);
        assert_eq!(pipe.mode, OutputMode::Quiet);
        assert!(
            !pipe.quiet_was_explicit,
            "pipe-default Quiet is implicit, not explicit"
        );
    }

    #[test]
    fn empty_env_falls_through_to_tty() {
        // `DOIGET_MODE=""` (empty) is treated as unset (parse_env_mode
        // returns None on an empty/whitespace string).
        assert_eq!(parse_env_mode(""), None);
        assert_eq!(parse_env_mode("   "), None);
        let tty = resolve(None, FlagInput::None, Some(""), true);
        assert_eq!(tty.mode, OutputMode::Human);
    }

    // ---- TTY tail ------------------------------------------------------

    #[test]
    fn tty_with_no_flag_no_env_yields_human() {
        let out = resolve(None, FlagInput::None, None, true);
        assert_eq!(out.mode, OutputMode::Human);
        assert!(!out.quiet_was_explicit);
    }

    #[test]
    fn no_tty_with_no_flag_no_env_yields_quiet() {
        let out = resolve(None, FlagInput::None, None, false);
        assert_eq!(out.mode, OutputMode::Quiet);
        assert!(
            !out.quiet_was_explicit,
            "non-TTY default to Quiet is implicit (#219 / #220 / ADR-0017 Am1)"
        );
    }

    // ---- env never overrides flag, never beats forced_implicit --------

    #[test]
    fn env_does_not_override_explicit_flag() {
        let out = resolve(
            None,
            FlagInput::Explicit(OutputMode::Quiet),
            Some("human"),
            true,
        );
        assert_eq!(out.mode, OutputMode::Quiet);
        assert!(
            out.quiet_was_explicit,
            "`--mode quiet` is an explicit Quiet signal"
        );
    }

    #[test]
    fn forced_implicit_overrides_env() {
        let out = resolve(Some(OutputMode::Mcp), FlagInput::None, Some("human"), true);
        assert_eq!(out.mode, OutputMode::Mcp);
    }

    // ---- ADR-0017 Amendment 1: explicit vs implicit Quiet ------------

    #[test]
    fn doiget_mode_quiet_env_is_explicit_quiet() {
        // DOIGET_MODE=quiet without any flag is treated as explicit
        // user intent — artifact commands must respect it.
        let out = resolve(None, FlagInput::None, Some("quiet"), true);
        assert_eq!(out.mode, OutputMode::Quiet);
        assert!(out.quiet_was_explicit);
    }

    #[test]
    fn non_tty_quiet_default_is_implicit_quiet() {
        // The TTY-driven fallback to Quiet is implicit — artifact
        // commands (capabilities/bib/csl) MUST still emit. This is
        // the #219/#220 LLM cold-boot fix.
        let out = resolve(None, FlagInput::None, None, /* is_tty */ false);
        assert_eq!(out.mode, OutputMode::Quiet);
        assert!(!out.quiet_was_explicit);
    }

    // ---- artifact-command classifier (ADR-0017 Am1) ------------------

    #[test]
    fn artifact_command_classifier_covers_export_and_inspection_commands() {
        // Amendment 1: export / inventory commands.
        assert!(is_artifact_command("bib"));
        assert!(is_artifact_command("csl"));
        assert!(is_artifact_command("capabilities"));
        // Amendment 2 (#301): read / inspection commands — their stdout
        // rendering IS the requested artifact, so the non-TTY implicit
        // Quiet must NOT erase it.
        assert!(is_artifact_command("info"));
        assert!(is_artifact_command("list-recent"));
        assert!(is_artifact_command("search"));
        assert!(is_artifact_command("link"));
        // audit-log is informational-vs-artifact per resolved mode,
        // not per name; the classifier does NOT match it.
        assert!(!is_artifact_command("audit-log"));
        // Informational status-only commands stay suppressible on any Quiet.
        assert!(!is_artifact_command("fetch"));
        assert!(!is_artifact_command("batch"));
        assert!(!is_artifact_command("config"));
        assert!(!is_artifact_command(""));
    }
}
