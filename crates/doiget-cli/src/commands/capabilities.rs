//! `doiget capabilities` — single-shot inventory JSON for LLM cold-boot
//! (#214).
//!
//! Emits a single JSON value describing the **full surface** of this
//! `doiget` binary: subcommands (walked from the live `clap::Command`
//! tree so the inventory cannot drift from the parser), positional args
//! and named flags per subcommand, global flags, the four
//! [`super::output::OutputMode`] values, hand-maintained env-var + example tables, the
//! `doiget_*` MCP tool list, compile-time features, and a `docs` map
//! pointing at the canonical spec files.
//!
//! Design rationale: the existing `--help` output lists subcommand
//! names but the rest of doiget's surface (env vars, MCP tools, JSON
//! schemas, ADR refs) is scattered across `docs/`. An LLM cold-booted
//! into doiget — no repo access, no follow-up doc reads — cannot
//! discover those via `--help` alone. This subcommand closes that gap
//! with one round-trip.
//!
//! # Output mode
//!
//! `doiget capabilities` is a **product-output** command per the
//! ADR-0017 convention (`--mode` is informational; the JSON inventory
//! is the artefact). `--mode quiet` is the one mode that suppresses
//! stdout (#203 / CONFIG.md §3); every other mode emits the same JSON.

use anyhow::{Context, Result};
use serde::Serialize;

/// Top-level capability inventory. Serialised to stdout as one JSON
/// value. Field names are part of the public wire format: renaming
/// any field is a semver minor with a CHANGELOG `\[BREAKING\]` callout
/// (same discipline as `EntryInfo` / `MigrationReport` in #213).
#[allow(missing_docs)] // Field names ARE the schema; documented externally in #214.
#[derive(Debug, Serialize)]
pub struct Capabilities {
    /// `CARGO_PKG_VERSION` for this build.
    pub version: &'static str,
    /// Cargo features compiled into this binary. Empty when only the
    /// `oa-only` default is enabled.
    pub features: Vec<&'static str>,
    /// All four [`super::output::OutputMode`] values; the parser accepts these for
    /// `--mode`. Mirrors `CONFIG.md` §3.
    pub modes: &'static [&'static str],
    /// Global flags that apply to every subcommand.
    pub global_flags: Vec<FlagSpec>,
    /// One entry per CLI subcommand (clap-walked).
    pub subcommands: Vec<SubcommandSpec>,
    /// `DOIGET_*` env vars from CONFIG.md §4.
    pub env_vars: &'static [EnvVar],
    /// MCP tools exposed by `doiget serve` (hand-coded; the source of
    /// truth is `docs/MCP_TOOLS.md` §1).
    pub mcp_tools: &'static [McpTool],
    /// Canonical doc paths an LLM can pull for deeper context.
    pub docs: Docs,
}

#[allow(missing_docs)] // Field names ARE the schema; documented externally in #214.
#[derive(Debug, Serialize)]
pub struct FlagSpec {
    /// e.g. `--mode`, `--json`, `-q`.
    pub name: String,
    /// `bool` for boolean switches, `enum` for value-bounded flags,
    /// `string` / `path` otherwise.
    pub kind: &'static str,
    /// `clap` `help` text.
    pub help: Option<String>,
    /// For `enum` kind only: accepted values.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub values: Option<&'static [&'static str]>,
}

#[allow(missing_docs)] // Field names ARE the schema; documented externally in #214.
#[derive(Debug, Serialize)]
pub struct SubcommandSpec {
    pub name: String,
    pub summary: Option<String>,
    pub args: Vec<ArgSpec>,
    pub flags: Vec<FlagSpec>,
    /// Hand-maintained canonical invocations.
    pub examples: &'static [&'static str],
    /// How this command interacts with `--mode json`. See [`JsonMode`].
    pub json_mode: JsonMode,
    /// Cargo feature this subcommand is gated behind, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feature_gated: Option<&'static str>,
}

#[allow(missing_docs)] // Field names ARE the schema; documented externally in #214.
#[derive(Debug, Serialize)]
pub struct ArgSpec {
    pub name: String,
    /// `positional` | `flag-value`.
    pub kind: &'static str,
    pub help: Option<String>,
    /// `true` when the arg has no default and no `Option<T>` wrapper.
    pub required: bool,
}

#[allow(missing_docs)] // Variants are documented inline; the enum-level allow silences clippy.
#[derive(Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JsonMode {
    /// The command's primary output IS JSON regardless of `--mode`
    /// (e.g. `csl`, `graph`, `*-dry-run`).
    Product,
    /// The command emits a structured JSON body when `--mode json` is
    /// active; otherwise human (e.g. `info`, `list-recent`, `audit-log`).
    Supported,
    /// Not yet — JSON body honoring is tracked as the issue named
    /// inside.
    Deferred {
        /// The follow-up issue tracking the JSON honoring for this
        /// command (e.g. `"#210"`).
        tracking: &'static str,
    },
}

#[allow(missing_docs)] // Field names ARE the schema; documented externally in #214.
#[derive(Debug, Serialize)]
pub struct EnvVar {
    pub name: &'static str,
    /// `(none)` when no built-in default.
    pub default: &'static str,
    pub help: &'static str,
}

#[allow(missing_docs)] // Field names ARE the schema; documented externally in #214.
#[derive(Debug, Serialize)]
pub struct McpTool {
    pub name: &'static str,
    /// Anchor-style reference into `docs/MCP_TOOLS.md`.
    pub schema_ref: &'static str,
}

#[allow(missing_docs)] // Field names ARE the schema; documented externally in #214.
#[derive(Debug, Serialize)]
pub struct Docs {
    pub config: &'static str,
    pub errors: &'static str,
    pub scope: &'static str,
    pub mcp: &'static str,
    pub sources: &'static str,
    pub redirect_allowlist: &'static str,
    pub provenance_log: &'static str,
}

// ---------------------------------------------------------------------------
// Static tables
// ---------------------------------------------------------------------------

const MODES: &[&str] = &["human", "json", "quiet", "mcp"];

const ENV_VARS: &[EnvVar] = &[
    EnvVar {
        name: "DOIGET_STORE_ROOT",
        default: "$HOME/papers",
        help: "Root of the on-disk paper store. CONFIG.md §4.",
    },
    EnvVar {
        name: "DOIGET_LOG_PATH",
        default: "<config_dir>/doiget/access.jsonl",
        help: "JSON-Lines provenance log file path (PROVENANCE_LOG.md §3).",
    },
    EnvVar {
        name: "DOIGET_LOG_RETENTION_DAYS",
        default: "90",
        help: "Rotated-segment retention window (0 disables pruning). #140 / PROVENANCE_LOG.md §6.",
    },
    EnvVar {
        name: "DOIGET_MODE",
        default: "(none)",
        help: "Output mode (`human`/`json`/`quiet`/`mcp`). ADR-0017 ladder rung 3.",
    },
    EnvVar {
        name: "DOIGET_CONTACT_EMAIL",
        default: "(none)",
        help: "Contact email for polite User-Agent header (CONFIG.md §4).",
    },
    EnvVar {
        name: "DOIGET_UNPAYWALL_EMAIL",
        default: "(falls back to DOIGET_CONTACT_EMAIL)",
        help: "Unpaywall-specific contact email.",
    },
    EnvVar {
        name: "DOIGET_USER_AGENT",
        default: "(default polite UA)",
        help: "Override the User-Agent header for all outbound requests.",
    },
    EnvVar {
        name: "DOIGET_ENABLE_OPENALEX",
        default: "(off)",
        help: "Enable the OpenAlex citation graph source (graph subcommand prerequisite).",
    },
    EnvVar {
        name: "DOIGET_ARXIV_BASE",
        default: "https://export.arxiv.org/",
        help: "arXiv API base URL — primarily for testing/wiremock override.",
    },
    EnvVar {
        name: "DOIGET_CROSSREF_BASE",
        default: "https://api.crossref.org/",
        help: "Crossref API base URL.",
    },
    EnvVar {
        name: "DOIGET_UNPAYWALL_BASE",
        default: "https://api.unpaywall.org/",
        help: "Unpaywall API base URL.",
    },
];

const MCP_TOOLS: &[McpTool] = &[
    McpTool {
        name: "doiget_resolve_paper",
        schema_ref: "docs/MCP_TOOLS.md#1-tool-list",
    },
    McpTool {
        name: "doiget_fetch_paper",
        schema_ref: "docs/MCP_TOOLS.md#1-tool-list",
    },
    McpTool {
        name: "doiget_metadata_only",
        schema_ref: "docs/MCP_TOOLS.md#11-doiget_metadata_only-normative",
    },
    McpTool {
        name: "doiget_batch_fetch",
        schema_ref: "docs/MCP_TOOLS.md#1-tool-list",
    },
    McpTool {
        name: "doiget_info",
        schema_ref: "docs/MCP_TOOLS.md#1-tool-list",
    },
    McpTool {
        name: "doiget_search_local",
        schema_ref: "docs/MCP_TOOLS.md#1-tool-list",
    },
    McpTool {
        name: "doiget_list_recent",
        schema_ref: "docs/MCP_TOOLS.md#1-tool-list",
    },
    McpTool {
        name: "doiget_paper_pdf_path",
        schema_ref: "docs/MCP_TOOLS.md#1-tool-list",
    },
    McpTool {
        name: "doiget_capability_profile",
        schema_ref: "docs/MCP_TOOLS.md#1-tool-list",
    },
    McpTool {
        name: "doiget_health",
        schema_ref: "docs/MCP_TOOLS.md#1-tool-list",
    },
    McpTool {
        name: "doiget_expand_citation_graph",
        schema_ref: "docs/MCP_TOOLS.md#1-tool-list",
    },
    McpTool {
        name: "doiget_bibtex_export",
        schema_ref: "docs/MCP_TOOLS.md#1-tool-list",
    },
    McpTool {
        name: "doiget_csl_export",
        schema_ref: "docs/MCP_TOOLS.md#1-tool-list",
    },
];

const DOCS: Docs = Docs {
    config: "docs/CONFIG.md",
    errors: "docs/ERRORS.md",
    scope: "docs/SCOPE.md",
    mcp: "docs/MCP_TOOLS.md",
    sources: "docs/SOURCES.md",
    redirect_allowlist: "docs/REDIRECT_ALLOWLIST.md",
    provenance_log: "docs/PROVENANCE_LOG.md",
};

/// Per-subcommand hand-maintained metadata. The clap walk provides
/// name + summary + args + flags; this table adds examples,
/// `json_mode` semantics, and feature-gating that clap doesn't
/// expose. A regression unit test asserts every clap-visible
/// subcommand has an entry here (otherwise the test fails loudly).
struct SubcommandMeta {
    examples: &'static [&'static str],
    json_mode: JsonMode,
    feature_gated: Option<&'static str>,
}

fn metadata_for(subcommand: &str) -> Option<SubcommandMeta> {
    let m = match subcommand {
        "fetch" => SubcommandMeta {
            examples: &[
                "doiget fetch 10.1234/foo",
                "doiget fetch arxiv:2401.12345",
                "doiget fetch 10.1234/foo --dry-run",
            ],
            // The success summary is on stderr (ADR-0001); the
            // dry-run plan is JSON product output (ADR-0022).
            json_mode: JsonMode::Product,
            feature_gated: None,
        },
        "batch" => SubcommandMeta {
            examples: &[
                "doiget batch refs.txt",
                "doiget batch refs.txt --dry-run",
                "doiget batch refs.txt --json",
            ],
            // `--json` emits the ERRORS.md §3 JSONL per-ref shape (#205).
            json_mode: JsonMode::Supported,
            feature_gated: None,
        },
        "info" => SubcommandMeta {
            examples: &[
                "doiget info 10.1234/foo",
                "doiget info arxiv:2401.12345 --json",
            ],
            json_mode: JsonMode::Supported,
            feature_gated: None,
        },
        "list-recent" => SubcommandMeta {
            examples: &[
                "doiget list-recent",
                "doiget list-recent 20",
                "doiget list-recent --json",
            ],
            json_mode: JsonMode::Supported,
            feature_gated: None,
        },
        "search" => SubcommandMeta {
            examples: &[
                "doiget search 'quantum entanglement'",
                "doiget search renormalization --json",
            ],
            json_mode: JsonMode::Supported,
            feature_gated: None,
        },
        "bib" => SubcommandMeta {
            examples: &["doiget bib 10.1234/foo", "doiget bib arxiv:2401.12345"],
            // BibTeX output is the product; `--mode` is informational.
            json_mode: JsonMode::Product,
            feature_gated: None,
        },
        "csl" => SubcommandMeta {
            examples: &["doiget csl 10.1234/foo"],
            json_mode: JsonMode::Product,
            feature_gated: None,
        },
        "audit-log" => SubcommandMeta {
            examples: &[
                "doiget audit-log --verify",
                "doiget audit-log --verify --json",
                "doiget --quiet audit-log --verify   # exit code only",
            ],
            json_mode: JsonMode::Supported,
            feature_gated: None,
        },
        "provenance" => SubcommandMeta {
            examples: &[
                "doiget provenance migrate --dry-run",
                "doiget provenance migrate",
                "doiget provenance migrate --dry-run --json",
            ],
            json_mode: JsonMode::Supported,
            feature_gated: None,
        },
        "config" => SubcommandMeta {
            examples: &[
                "doiget config show",
                "doiget config show --json",
                "doiget config path",
                "doiget config doctor",
            ],
            json_mode: JsonMode::Supported,
            feature_gated: None,
        },
        "serve" => SubcommandMeta {
            examples: &["doiget serve   # stdio MCP server (ADR-0001)"],
            // serve always runs in mcp mode; the protocol output is
            // JSON-RPC, which is product.
            json_mode: JsonMode::Product,
            feature_gated: None,
        },
        "graph" => SubcommandMeta {
            examples: &[
                "DOIGET_ENABLE_OPENALEX=1 doiget graph 10.1234/foo",
                "DOIGET_ENABLE_OPENALEX=1 doiget graph 10.1234/foo --depth 2 --total 50",
            ],
            json_mode: JsonMode::Product,
            feature_gated: Some("citation"),
        },
        "capabilities" => SubcommandMeta {
            examples: &["doiget capabilities | jq ."],
            // The whole point of capabilities IS JSON output.
            json_mode: JsonMode::Product,
            feature_gated: None,
        },
        // clap auto-adds `help`; we silently ignore it (it's not a
        // domain subcommand).
        "help" => return None,
        _ => return None,
    };
    Some(m)
}

// ---------------------------------------------------------------------------
// Build
// ---------------------------------------------------------------------------

/// Build the [`Capabilities`] inventory from `cli` (the clap parser
/// for this binary, supplied by the caller because the `Cli` struct
/// lives in `main.rs` and is not exposed in the library crate). The
/// caller is `commands::main::run_dispatch` via `Cli::command()`.
pub fn build_capabilities(cli: &clap::Command) -> Capabilities {
    let global_flags = collect_global_flags(cli);
    let subcommands = cli
        .get_subcommands()
        .filter_map(|sub| build_subcommand(sub, cli))
        .collect::<Vec<_>>();
    Capabilities {
        version: env!("CARGO_PKG_VERSION"),
        features: compile_time_features(),
        modes: MODES,
        global_flags,
        subcommands,
        env_vars: ENV_VARS,
        mcp_tools: MCP_TOOLS,
        docs: DOCS,
    }
}

fn compile_time_features() -> Vec<&'static str> {
    let mut feats: Vec<&'static str> = Vec::new();
    if cfg!(feature = "oa-only") {
        feats.push("oa-only");
    }
    if cfg!(feature = "metadata") {
        feats.push("metadata");
    }
    if cfg!(feature = "citation") {
        feats.push("citation");
    }
    if cfg!(feature = "tdm-elsevier") {
        feats.push("tdm-elsevier");
    }
    if cfg!(feature = "tdm-aps") {
        feats.push("tdm-aps");
    }
    if cfg!(feature = "tdm-springer") {
        feats.push("tdm-springer");
    }
    feats
}

fn collect_global_flags(cmd: &clap::Command) -> Vec<FlagSpec> {
    cmd.get_arguments()
        .filter(|a| a.is_global_set())
        .map(arg_to_flag_spec)
        .collect()
}

fn build_subcommand(sub: &clap::Command, root: &clap::Command) -> Option<SubcommandSpec> {
    let name = sub.get_name();
    let meta = metadata_for(name)?;
    let (args, flags) = split_args_and_flags(sub, root);
    Some(SubcommandSpec {
        name: name.to_string(),
        summary: sub.get_about().map(|s| s.to_string()),
        args,
        flags,
        examples: meta.examples,
        json_mode: meta.json_mode,
        feature_gated: meta.feature_gated,
    })
}

fn split_args_and_flags(
    sub: &clap::Command,
    root: &clap::Command,
) -> (Vec<ArgSpec>, Vec<FlagSpec>) {
    // The root's global args appear in every subcommand's iterator;
    // suppress them from per-subcommand `flags` (they're already in
    // `global_flags`).
    let global_names: std::collections::HashSet<&str> = root
        .get_arguments()
        .filter(|a| a.is_global_set())
        .map(|a| a.get_id().as_str())
        .collect();
    let mut args = Vec::new();
    let mut flags = Vec::new();
    for a in sub.get_arguments() {
        if global_names.contains(a.get_id().as_str()) {
            continue;
        }
        if a.is_positional() {
            args.push(ArgSpec {
                name: a.get_id().to_string(),
                kind: "positional",
                help: a.get_help().map(|s| s.to_string()),
                required: a.is_required_set(),
            });
        } else {
            flags.push(arg_to_flag_spec(a));
        }
    }
    (args, flags)
}

fn arg_to_flag_spec(a: &clap::Arg) -> FlagSpec {
    let name = a
        .get_long()
        .map(|s| format!("--{s}"))
        .or_else(|| a.get_short().map(|c| format!("-{c}")))
        .unwrap_or_else(|| a.get_id().to_string());
    // Best-effort kind classification. Boolean switches show up as
    // `bool`; value-enum flags show up as `enum` with the accepted
    // values; everything else is `string`.
    let kind = if matches!(
        a.get_action(),
        clap::ArgAction::SetTrue | clap::ArgAction::SetFalse
    ) {
        "bool"
    } else if a.get_value_parser().possible_values().is_some() {
        "enum"
    } else {
        "string"
    };
    let values: Option<&'static [&'static str]> = if kind == "enum" && name == "--mode" {
        Some(MODES)
    } else {
        None
    };
    FlagSpec {
        name,
        kind,
        help: a.get_help().map(|s| s.to_string()),
        values,
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Run the `doiget capabilities` subcommand. Honors [`super::output::OutputMode`]:
/// `Quiet` suppresses stdout (#203); every other mode emits the same
/// pretty-printed JSON inventory. The caller passes the live
/// `clap::Command` so the clap walk operates on the binary's actual
/// `Cli` tree (which the lib half of this crate can't reach
/// directly — the `Cli` struct lives in `main.rs`).
pub fn run(cli: &clap::Command, mode: super::output::OutputMode) -> Result<()> {
    // `Quiet` is the one mode that suppresses (per ADR-0017 / #203).
    // Every other mode emits the same pretty JSON: `capabilities` is a
    // product-output command.
    if mode == super::output::OutputMode::Quiet {
        return Ok(());
    }
    let caps = build_capabilities(cli);
    let s = serde_json::to_string_pretty(&caps).context("serialise capabilities inventory")?;
    // `print_stdout` workspace-deny; localised allow at the
    // sanctioned product-output sink. See `commands/csl.rs`'s pattern.
    #[allow(clippy::print_stdout)]
    {
        println!("{s}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    /// Mirrors the `Cli` struct in `main.rs` for lib-test reach.
    /// `commands::capabilities` is library-level; the binary-only
    /// `Cli` struct can't be reached from here, so we re-derive a
    /// shadow whose subcommand list is identical. The
    /// `cli_shadow_matches_main_cli` integration test in
    /// `tests/capabilities_e2e.rs` runs the real binary and asserts
    /// the wire output matches.
    fn test_cli() -> clap::Command {
        use clap::{Arg, ArgAction, Command};
        let mode_values = ["human", "json", "quiet", "mcp"];
        Command::new("doiget")
            .arg(
                Arg::new("mode")
                    .long("mode")
                    .global(true)
                    .value_parser(clap::builder::PossibleValuesParser::new(mode_values))
                    .help("Output mode (human|json|quiet|mcp)."),
            )
            .arg(
                Arg::new("json")
                    .long("json")
                    .global(true)
                    .action(ArgAction::SetTrue)
                    .help("Short for `--mode json`."),
            )
            .arg(
                Arg::new("quiet")
                    .long("quiet")
                    .short('q')
                    .global(true)
                    .action(ArgAction::SetTrue)
                    .help("Short for `--mode quiet`."),
            )
            .subcommand(
                Command::new("fetch")
                    .about("Fetch a single paper PDF")
                    .arg(Arg::new("ref").required(true)),
            )
            .subcommand(Command::new("batch").about("Fetch many refs"))
            .subcommand(Command::new("info").about("Show metadata"))
            .subcommand(Command::new("list-recent").about("List recent"))
            .subcommand(Command::new("search").about("Search local"))
            .subcommand(Command::new("bib").about("BibTeX export"))
            .subcommand(Command::new("csl").about("CSL export"))
            .subcommand(Command::new("audit-log").about("Audit log"))
            .subcommand(Command::new("provenance").about("Provenance ops"))
            .subcommand(Command::new("config").about("Config"))
            .subcommand(Command::new("serve").about("MCP server"))
            .subcommand(Command::new("capabilities").about("Capabilities"))
    }

    fn caps() -> Capabilities {
        build_capabilities(&test_cli())
    }

    #[test]
    fn capabilities_serialises_to_valid_json() {
        let s = serde_json::to_string_pretty(&caps()).expect("serialise");
        let v: serde_json::Value = serde_json::from_str(&s).expect("parse round-trip");
        for key in [
            "version",
            "features",
            "modes",
            "global_flags",
            "subcommands",
            "env_vars",
            "mcp_tools",
            "docs",
        ] {
            assert!(
                v.get(key).is_some(),
                "top-level key `{key}` missing from capabilities JSON: {v}"
            );
        }
    }

    #[test]
    fn modes_field_matches_output_mode_enum() {
        // Tied to `OutputMode { Human, Json, Quiet, Mcp }`.
        assert_eq!(caps().modes, &["human", "json", "quiet", "mcp"]);
    }

    #[test]
    fn env_vars_all_use_doiget_prefix() {
        for ev in ENV_VARS {
            assert!(
                ev.name.starts_with("DOIGET_"),
                "env var name MUST use DOIGET_ prefix, got `{}`",
                ev.name
            );
        }
    }

    #[test]
    fn mcp_tools_all_use_doiget_prefix() {
        for t in MCP_TOOLS {
            assert!(
                t.name.starts_with("doiget_"),
                "MCP tool name MUST use doiget_ prefix, got `{}`",
                t.name
            );
        }
    }

    #[test]
    fn subcommand_examples_reference_the_subcommand_name() {
        for sub in &caps().subcommands {
            for ex in sub.examples {
                assert!(
                    ex.starts_with("doiget "),
                    "example `{ex}` for `{}` should start with `doiget `",
                    sub.name
                );
                assert!(
                    ex.contains(&sub.name),
                    "example `{ex}` does not mention subcommand `{}`",
                    sub.name
                );
            }
        }
    }

    #[test]
    fn version_is_cargo_pkg_version() {
        assert_eq!(caps().version, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn every_test_cli_subcommand_has_metadata() {
        // Regression at the lib layer: anything we add to the shadow
        // `test_cli` must also be in `metadata_for`. The real
        // `Cli::command()` is exercised by the e2e test in
        // `tests/capabilities_e2e.rs`.
        for sub in test_cli().get_subcommands() {
            let name = sub.get_name();
            if name == "help" {
                continue;
            }
            assert!(
                metadata_for(name).is_some(),
                "subcommand `{name}` lacks metadata in `metadata_for`"
            );
        }
    }
}
