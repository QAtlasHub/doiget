//! Output-mode resolution for the `doiget` CLI (ADR-0017, #144).
//!
//! ADR-0017 specifies the precedence ladder
//! `--mode > --json/--quiet > DOIGET_MODE env > subcommand-implicit > TTY > quiet`.
//! CONFIG.md §5 additionally pins `doiget serve` to `mcp` mode regardless
//! of flags — a load-bearing security invariant (the stdout-purity Slice 9
//! CI job already enforces "MCP mode forbids non-JSON stdout"). The
//! `forced_implicit` parameter to [`resolve`] expresses that override: when
//! `Some(_)`, it overrides everything else.
//!
//! Resolution is split into a pure function ([`resolve`]) plus a thin
//! TTY-detection wrapper ([`stdout_is_tty`]) so the ladder is fully
//! unit-testable without environment manipulation.
//!
//! # Per-mode honoring across the CLI surface
//!
//! - `Human` — default for TTY stdout. Human-readable text, the
//!   pre-#144 behaviour.
//! - `Quiet` — informational stdout suppressed across the six
//!   info-emitting commands (audit-log / info / list-recent / search /
//!   config show / config path / provenance migrate) per #203.
//!   Errors (stderr) and exit codes are unaffected. Product-output
//!   commands (bib / csl / graph / *-dry-run / batch JSONL) are NOT
//!   suppressed.
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

/// Resolve the effective [`OutputMode`] per ADR-0017.
///
/// Precedence (highest first):
///
/// 1. `forced_implicit` — a subcommand-pinned mode that overrides
///    everything (e.g. `doiget serve` → `Mcp` per CONFIG.md §5; required
///    for the Slice 9 stdout-purity invariant).
/// 2. `flag` — `--mode` / `--json` / `--quiet` on the command line.
/// 3. `env` — `DOIGET_MODE` (parsed by [`parse_env_mode`]; unrecognised
///    values are ignored, matching CONFIG.md §4's "doiget reads only the
///    keys it knows about" posture).
/// 4. `is_tty` — `Human` when stdout is a terminal, otherwise `Quiet`
///    (CONFIG.md §3.b's "implicit + TTY > quiet (default)").
///
/// This function is pure: no env reads, no I/O. The caller plumbs
/// `env::var("DOIGET_MODE").ok()` and an `is_tty` probe in.
pub fn resolve(
    forced_implicit: Option<OutputMode>,
    flag: FlagInput,
    env: Option<&str>,
    is_tty: bool,
) -> OutputMode {
    if let Some(m) = forced_implicit {
        return m;
    }
    match flag {
        FlagInput::Explicit(m) => m,
        FlagInput::JsonShort => OutputMode::Json,
        FlagInput::QuietShort => OutputMode::Quiet,
        FlagInput::None => env.and_then(parse_env_mode).unwrap_or(if is_tty {
            OutputMode::Human
        } else {
            OutputMode::Quiet
        }),
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
        let m = resolve(
            Some(OutputMode::Mcp),
            FlagInput::Explicit(OutputMode::Quiet),
            Some("human"),
            true,
        );
        assert_eq!(m, OutputMode::Mcp);
    }

    // ---- flag > env > tty ---------------------------------------------

    #[test]
    fn explicit_flag_wins_over_env_and_tty() {
        let m = resolve(
            None,
            FlagInput::Explicit(OutputMode::Json),
            Some("human"),
            true,
        );
        assert_eq!(m, OutputMode::Json);
    }

    #[test]
    fn json_short_flag_implies_json() {
        let m = resolve(None, FlagInput::JsonShort, Some("human"), true);
        assert_eq!(m, OutputMode::Json);
    }

    #[test]
    fn quiet_short_flag_implies_quiet() {
        let m = resolve(None, FlagInput::QuietShort, Some("human"), true);
        assert_eq!(m, OutputMode::Quiet);
    }

    #[test]
    fn env_wins_when_no_flag() {
        let m = resolve(None, FlagInput::None, Some("json"), true);
        assert_eq!(m, OutputMode::Json);
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
        assert_eq!(tty, OutputMode::Human);
        assert_eq!(pipe, OutputMode::Quiet);
    }

    #[test]
    fn empty_env_falls_through_to_tty() {
        // `DOIGET_MODE=""` (empty) is treated as unset (parse_env_mode
        // returns None on an empty/whitespace string).
        assert_eq!(parse_env_mode(""), None);
        assert_eq!(parse_env_mode("   "), None);
        let tty = resolve(None, FlagInput::None, Some(""), true);
        assert_eq!(tty, OutputMode::Human);
    }

    // ---- TTY tail ------------------------------------------------------

    #[test]
    fn tty_with_no_flag_no_env_yields_human() {
        let m = resolve(None, FlagInput::None, None, true);
        assert_eq!(m, OutputMode::Human);
    }

    #[test]
    fn no_tty_with_no_flag_no_env_yields_quiet() {
        let m = resolve(None, FlagInput::None, None, false);
        assert_eq!(m, OutputMode::Quiet);
    }

    // ---- env never overrides flag, never beats forced_implicit --------

    #[test]
    fn env_does_not_override_explicit_flag() {
        let m = resolve(
            None,
            FlagInput::Explicit(OutputMode::Quiet),
            Some("human"),
            true,
        );
        assert_eq!(m, OutputMode::Quiet);
    }

    #[test]
    fn forced_implicit_overrides_env() {
        let m = resolve(Some(OutputMode::Mcp), FlagInput::None, Some("human"), true);
        assert_eq!(m, OutputMode::Mcp);
    }
}
