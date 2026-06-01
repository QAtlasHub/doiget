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
//! stdout (#203 / CONFIG.md §5); every other mode emits the same JSON.
//!
//! # Wire-format stability (whole module)
//!
//! Every `pub` struct / enum below carries `#[non_exhaustive]`. Adding
//! a field is non-breaking; renaming or removing one is a
//! compile-time break for downstream Rust consumers and a
//! `[BREAKING]`-class change for JSON consumers (CHANGELOG must call
//! it out). The per-item `#[non_exhaustive]` attributes intentionally
//! carry no inline comment; this module-doc says it once.

use anyhow::{Context, Result};
use serde::Serialize;

/// Top-level capability inventory. Serialised to stdout as one JSON
/// value. Field names are part of the public wire format: renaming
/// any field is a semver minor with a CHANGELOG `\[BREAKING\]` callout
/// (same discipline as `EntryInfo` / `MigrationReport` in #213).
#[allow(missing_docs)] // Field names ARE the schema; documented externally in #214.
#[non_exhaustive]
#[derive(Debug, Serialize)]
pub struct Capabilities {
    /// `CARGO_PKG_VERSION` for this build.
    pub version: &'static str,
    /// Cargo features compiled into this binary. Contains `"oa-only"`
    /// in stock release builds (the default feature). Empty only when
    /// the crate was built with `--no-default-features` and **no
    /// other features enabled**; a build like
    /// `cargo build --no-default-features --features citation`
    /// yields `["citation"]`, not `[]`.
    pub features: Vec<&'static str>,
    /// All four [`super::output::OutputMode`] values; the parser accepts these for
    /// `--mode`. Mirrors `CONFIG.md` §5 (CLI flags).
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
    /// Number of user-extension allowlist hosts loaded from
    /// `<config_dir>/doiget/config.toml` per ADR-0028 D2. `0` if the
    /// config file is missing, contains no `[[network.additional_hosts]]`,
    /// or fails to parse — run `doiget config doctor` to diagnose parse
    /// failures. Exposed so an LLM can confirm at cold-boot whether the
    /// curated allowlist has been extended on this host.
    pub user_extension_count: usize,
}

/// What kind of value (if any) a [`FlagSpec`] carries.
///
/// Typed (not `&'static str`) so a typo can't slip into the wire
/// format and the `Enum`-implies-`values`-present invariant is
/// expressible at the type layer (see #215 for the design pass). Serialises
/// as the lowercased variant name: `"bool"`, `"enum"`, `"string"`.
#[non_exhaustive]
#[derive(Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FlagKind {
    /// Boolean switch (no value).
    Bool,
    /// Value-bounded flag — `values` carries the accepted set.
    Enum,
    /// Any non-`Bool`, non-`Enum` flag. Today every such flag emits
    /// `"string"`; richer typing (`Path` / `Int` etc.) is intentionally
    /// out of scope until a real consumer needs it — `#[non_exhaustive]`
    /// reserves space without commitment.
    String,
}

#[allow(missing_docs)] // Field names ARE the schema; documented externally in #214.
#[non_exhaustive]
#[derive(Debug, Serialize)]
pub struct FlagSpec {
    /// e.g. `--mode`, `--json`, `-q`.
    pub name: String,
    /// Boolean / enum / free-string discriminator. See [`FlagKind`].
    pub kind: FlagKind,
    /// `clap` `help` text.
    pub help: Option<String>,
    /// For `kind == FlagKind::Enum`: the accepted values, harvested
    /// from clap's `PossibleValuesParser`. Owned (not `&'static`) so
    /// the helper works for any future enum flag, not just `--mode`
    /// (see #215).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<String>>,
}

#[allow(missing_docs)] // Field names ARE the schema; documented externally in #214.
#[non_exhaustive]
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

/// What kind of positional argument an [`ArgSpec`] describes.
///
/// Currently every entry is `Positional`; the typed enum reserves
/// space for future variants (e.g. `Stdin` markers) without breaking
/// existing JSON consumers. Serialises as `"positional"`.
#[non_exhaustive]
#[derive(Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ArgKind {
    /// A required-or-optional positional argument on the subcommand.
    Positional,
}

#[allow(missing_docs)] // Field names ARE the schema; documented externally in #214.
#[non_exhaustive]
#[derive(Debug, Serialize)]
pub struct ArgSpec {
    pub name: String,
    /// Always [`ArgKind::Positional`] today. Kept as a discriminator
    /// so the JSON shape can grow new arg kinds later without
    /// renaming fields (see #215 for the design pass).
    pub kind: ArgKind,
    pub help: Option<String>,
    /// `true` when the arg has no default and no `Option<T>` wrapper.
    pub required: bool,
}

/// How a subcommand interacts with `--mode json`.
///
/// Wire shape: every variant serialises to an object with a `status`
/// discriminant, so a consumer sees uniform `{"status":"…", …}`
/// records (`#[serde(tag = "status")]`). Before #215 the previous
/// mixed string/object representation forced consumers to handle two
/// JSON shapes for sibling variants.
///
/// **Tuple variants not permitted.** `#[serde(tag = "status")]`
/// requires the tag to live in the same flat object as variant
/// fields; tuple variants are incompatible with internally-tagged
/// representation. Future variants MUST use named fields.
#[non_exhaustive] // Adding a future variant is non-breaking for JSON consumers.
#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum JsonMode {
    /// The command's primary output IS the requested artifact, not
    /// informational chatter. `--mode` is informational here; the
    /// exact stdout shape (e.g. JSON for `csl` / `graph` /
    /// `capabilities` and the JSON-RPC stream from `serve`; BibTeX
    /// for `bib`; PDF-on-disk + stderr summary for `fetch`; a
    /// `--dry-run` JSON plan in the dry-run variants) is fixed by
    /// the subcommand and may vary across flags. **Consult
    /// `examples` for the per-flag stdout form** rather than
    /// assuming JSON.
    Artifact,
    /// Under `--mode json` the command emits a structured JSON body
    /// on stdout; otherwise the human form (e.g. `info`,
    /// `list-recent`, `audit-log`, `provenance migrate`, `batch`).
    Supported,
    // NOTE: a `Deferred { tracking: &'static str }` variant was
    // sketched during #214's design phase but never instantiated by
    // any subcommand. Removed in the #215 self-review pass to avoid
    // shipping an unused wire shape; `#[non_exhaustive]` keeps the
    // door open to add it back non-breakingly when a real consumer
    // emerges.
}

#[allow(missing_docs)] // Field names ARE the schema; documented externally in #214.
#[non_exhaustive]
#[derive(Debug, Serialize)]
pub struct EnvVar {
    pub name: &'static str,
    /// `(none)` when no built-in default.
    pub default: &'static str,
    pub help: &'static str,
}

#[allow(missing_docs)] // Field names ARE the schema; documented externally in #214.
#[non_exhaustive]
#[derive(Debug, Serialize)]
pub struct McpTool {
    pub name: &'static str,
    /// Anchor-style reference into `docs/MCP_TOOLS.md`.
    pub schema_ref: &'static str,
}

#[allow(missing_docs)] // Field names ARE the schema; documented externally in #214.
#[non_exhaustive]
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
        name: "DOIGET_CACHE_ROOT",
        default: "$HOME/.cache/doiget",
        help: "Root of the on-disk HTTP / metadata cache. CONFIG.md §4.",
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
    McpTool {
        // ADR-0030 D6: parse a CSL-JSON / (future) BibTeX file and
        // fetch each resolvable entry; each result row carries the
        // source bibliography's `entry_key` so a Zotero / Mendeley
        // plugin can bridge the fetched PDF back to the originating
        // reference.
        name: "doiget_batch_from_bibliography",
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
///
/// **Maintenance:** `feature_gated` MUST be kept in sync with the
/// corresponding `#[cfg(feature = …)]` annotation in `main.rs`. There
/// is no compile-time check; the `every_test_cli_subcommand_has_metadata`
/// regression test does not cover feature-gating directly — it only
/// asserts metadata exists. Add a CI matrix entry (`--features
/// citation`) when introducing new gated subcommands so the e2e
/// assertion list catches drift (see #215). Alternatively, add a
/// unit test that asserts `metadata_for("graph").unwrap().feature_gated
/// == Some("citation")` to lock the gate at the lib-test layer.
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
            json_mode: JsonMode::Artifact,
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
            json_mode: JsonMode::Artifact,
            feature_gated: None,
        },
        "csl" => SubcommandMeta {
            examples: &["doiget csl 10.1234/foo"],
            json_mode: JsonMode::Artifact,
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
            json_mode: JsonMode::Artifact,
            feature_gated: None,
        },
        "graph" => SubcommandMeta {
            examples: &[
                "DOIGET_ENABLE_OPENALEX=1 doiget graph 10.1234/foo",
                "DOIGET_ENABLE_OPENALEX=1 doiget graph 10.1234/foo --depth 2 --total 50",
            ],
            json_mode: JsonMode::Artifact,
            feature_gated: Some("citation"),
        },
        "version" => SubcommandMeta {
            examples: &[
                "doiget version",
                "doiget version --check",
                "doiget version --check --mode json",
            ],
            json_mode: JsonMode::Supported,
            feature_gated: None,
        },
        "capabilities" => SubcommandMeta {
            examples: &["doiget capabilities | jq ."],
            // The whole point of capabilities IS JSON output.
            json_mode: JsonMode::Artifact,
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
        user_extension_count: user_extension_count(),
    }
}

/// Count valid `[[network.additional_hosts]]` entries in
/// `<config_dir>/doiget/config.toml` (ADR-0028 D2). Returns `0` on any
/// failure — missing config, parse error, unresolvable config dir.
/// Diagnose failures via `doiget config doctor`; here we only need a
/// best-effort cold-boot signal for the inventory.
fn user_extension_count() -> usize {
    let cfg_dir = match super::fetch::config_dir_utf8() {
        Ok(p) => p,
        Err(_) => return 0,
    };
    let path = cfg_dir.join("doiget").join("config.toml");
    match doiget_core::user_extension::load(&path) {
        Ok(hosts) => hosts.len(),
        Err(_) => 0,
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
        // Clap auto-adds `--help` (and `--version` on the root) to
        // every subcommand. They're not positional and not
        // `is_global_set()`, so they would otherwise leak into every
        // subcommand's `flags[]` as `kind: "string"`. Filter on the
        // action against the known built-in variants.
        //
        // **Maintenance:** `clap::ArgAction` is itself
        // `#[non_exhaustive]` upstream. A future clap release that
        // adds a new built-in action (e.g. a hypothetical
        // `HelpMarkdown`) would fall through this `matches!` and
        // reappear in `flags[]`. Re-audit this filter on every clap
        // minor-version bump.
        if matches!(
            a.get_action(),
            clap::ArgAction::Help
                | clap::ArgAction::HelpShort
                | clap::ArgAction::HelpLong
                | clap::ArgAction::Version
        ) {
            continue;
        }
        if a.is_positional() {
            args.push(ArgSpec {
                name: a.get_id().to_string(),
                kind: ArgKind::Positional,
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
    // Boolean switches → `Bool`; value-enum flags → `Enum` with the
    // accepted values harvested from clap directly; everything else
    // → `String`. The `possible_values()` harvest covers any future
    // enum flag without code change (see #215).
    let possible: Option<Vec<String>> = a
        .get_value_parser()
        .possible_values()
        .map(|it| it.map(|pv| pv.get_name().to_owned()).collect());
    let (kind, values) = if matches!(
        a.get_action(),
        clap::ArgAction::SetTrue | clap::ArgAction::SetFalse
    ) {
        (FlagKind::Bool, None)
    } else if let Some(vs) = possible {
        (FlagKind::Enum, Some(vs))
    } else {
        (FlagKind::String, None)
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

/// Run the `doiget capabilities` subcommand.
///
/// `capabilities` is an **artifact** command per ADR-0017 Amendment 1:
/// its stdout output IS the deliverable (the inventory JSON an LLM
/// reads on cold-boot). It honors only **explicit** Quiet —
/// `--quiet` / `-q` / `--mode quiet` / `DOIGET_MODE=quiet` — and emits
/// the inventory on every other path. The `quiet_was_explicit`
/// discriminator is what distinguishes the two cases:
///
/// | mode               | quiet_was_explicit | behaviour          |
/// |--------------------|--------------------|--------------------|
/// | non-`Quiet`        | -                  | emit               |
/// | `Quiet` (explicit) | `true`             | suppress           |
/// | `Quiet` (non-TTY)  | `false`            | **emit** (#219)    |
///
/// The non-TTY case is the one #219 / #220 report: an LLM tool
/// executor captures stdout, so `stdout_is_tty()` is `false`, the
/// resolver falls through to `Quiet`, but the caller wants the JSON
/// inventory exactly because it's about to be machine-parsed. The
/// table's bottom row is the fix.
///
/// The caller passes the live `clap::Command` so the clap walk
/// operates on the binary's actual `Cli` tree (which the lib half of
/// this crate can't reach directly — the `Cli` struct lives in
/// `main.rs`).
pub fn run(
    cli: &clap::Command,
    mode: super::output::OutputMode,
    quiet_was_explicit: bool,
) -> Result<()> {
    // ADR-0017 Amendment 1: artifact command — suppress ONLY on
    // explicit Quiet, never on the non-TTY implicit fallback.
    if mode == super::output::OutputMode::Quiet && quiet_was_explicit {
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
        let cmd = Command::new("doiget")
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
                    .arg(Arg::new("ref").required(true))
                    .arg(
                        Arg::new("dry-run")
                            .long("dry-run")
                            .action(ArgAction::SetTrue),
                    ),
            )
            .subcommand(
                Command::new("batch")
                    .about("Fetch many refs")
                    .arg(Arg::new("path").required(true))
                    .arg(
                        Arg::new("dry-run")
                            .long("dry-run")
                            .action(ArgAction::SetTrue),
                    ),
            )
            .subcommand(
                Command::new("info")
                    .about("Show metadata")
                    .arg(Arg::new("ref").required(true)),
            )
            .subcommand(Command::new("list-recent").about("List recent"))
            .subcommand(
                Command::new("search")
                    .about("Search local")
                    .arg(Arg::new("query").required(true)),
            )
            .subcommand(
                Command::new("bib")
                    .about("BibTeX export")
                    .arg(Arg::new("ref").required(true)),
            )
            .subcommand(
                Command::new("csl")
                    .about("CSL export")
                    .arg(Arg::new("ref").required(true)),
            )
            .subcommand(
                Command::new("audit-log")
                    .about("Audit log")
                    .arg(Arg::new("verify").long("verify").action(ArgAction::SetTrue)),
            )
            .subcommand(Command::new("provenance").about("Provenance ops"))
            .subcommand(
                Command::new("config")
                    .about("Config")
                    .arg(Arg::new("action").required(true)),
            )
            .subcommand(Command::new("serve").about("MCP server"));
        // `graph` is `#[cfg(feature = "citation")]` in main.rs; mirror
        // the gate so the shadow CLI matches the production surface
        // (see #215).
        #[cfg(feature = "citation")]
        let cmd = cmd.subcommand(
            Command::new("graph")
                .about("Citation graph")
                .arg(Arg::new("ref").required(true)),
        );
        cmd.subcommand(Command::new("capabilities").about("Capabilities"))
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
            "user_extension_count",
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
                // `graph` examples carry a `DOIGET_ENABLE_OPENALEX=1`
                // env prefix before `doiget …`. Allow either form.
                assert!(
                    ex.starts_with("doiget ") || ex.contains(" doiget "),
                    "example `{ex}` for `{}` must invoke `doiget` somewhere",
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

    // Exact-set parity guard against drift between the static
    // `ENV_VARS` table and the documented surface (#215). The expected set is the SOURCE OF TRUTH at test time;
    // adding a new DOIGET_* env var requires updating both ENV_VARS
    // and this list in lockstep. CHANGELOG records cross-PR changes.
    #[test]
    fn env_vars_exact_set_matches_expected() {
        let actual: std::collections::BTreeSet<&str> = ENV_VARS.iter().map(|ev| ev.name).collect();
        let expected: std::collections::BTreeSet<&str> = [
            // CONFIG.md §4 documented:
            "DOIGET_STORE_ROOT",
            "DOIGET_CACHE_ROOT",
            "DOIGET_LOG_PATH",
            "DOIGET_LOG_RETENTION_DAYS",
            "DOIGET_USER_AGENT",
            "DOIGET_UNPAYWALL_EMAIL",
            "DOIGET_MODE",
            // Code-reachable but documented in code-level docs or
            // CAPABILITY.md (not CONFIG.md §4):
            "DOIGET_CONTACT_EMAIL",
            "DOIGET_ENABLE_OPENALEX",
            // Test/wiremock-override base URLs:
            "DOIGET_ARXIV_BASE",
            "DOIGET_CROSSREF_BASE",
            "DOIGET_UNPAYWALL_BASE",
        ]
        .into_iter()
        .collect();
        assert_eq!(
            actual, expected,
            "ENV_VARS table drifted from the expected canonical set; \
             update both `ENV_VARS` and this test together (and CONFIG.md §4 \
             if the new var is user-documented)."
        );
    }

    // Exact-set parity guard against drift between the static
    // `MCP_TOOLS` table and `docs/MCP_TOOLS.md` §1 (#215).
    #[test]
    fn mcp_tools_exact_set_matches_expected() {
        let actual: std::collections::BTreeSet<&str> = MCP_TOOLS.iter().map(|t| t.name).collect();
        let expected: std::collections::BTreeSet<&str> = [
            "doiget_resolve_paper",
            "doiget_fetch_paper",
            "doiget_metadata_only",
            "doiget_batch_fetch",
            "doiget_info",
            "doiget_search_local",
            "doiget_list_recent",
            "doiget_paper_pdf_path",
            "doiget_capability_profile",
            "doiget_health",
            "doiget_expand_citation_graph",
            "doiget_bibtex_export",
            "doiget_csl_export",
            "doiget_batch_from_bibliography",
        ]
        .into_iter()
        .collect();
        assert_eq!(
            actual, expected,
            "MCP_TOOLS table drifted from the expected set; update both \
             `MCP_TOOLS` and this test together (and docs/MCP_TOOLS.md §1)."
        );
    }

    // Pin the `#[serde(tag = "status")]` wire shape: every variant
    // serialises to a `{"status":"…", …}` object. Accidentally
    // removing the `tag` attribute (or renaming the discriminant)
    // would silently degrade the wire format; this test catches it
    // (#215 N1).
    #[test]
    fn json_mode_serialises_with_status_discriminant() {
        let s = serde_json::to_string(&JsonMode::Artifact).expect("serialise");
        assert_eq!(
            s, r#"{"status":"artifact"}"#,
            "Artifact must emit a status-tagged object"
        );
        let s = serde_json::to_string(&JsonMode::Supported).expect("serialise");
        assert_eq!(s, r#"{"status":"supported"}"#);
    }

    // `arg_to_flag_spec` was generalised in #215 to harvest the
    // accepted values from clap's `PossibleValuesParser` instead of
    // hard-coding `--mode`. Pin the contract: the `--mode` entry in
    // `global_flags` MUST report `kind: Enum` with all four mode
    // strings. A future regression that silently degrades `--mode`
    // to `kind: String, values: None` would otherwise pass every
    // existing test (#215 N3).
    #[test]
    fn mode_flag_carries_enum_kind_and_all_four_values() {
        let global = &caps().global_flags;
        let mode = global
            .iter()
            .find(|f| f.name == "--mode")
            .expect("--mode flag is in global_flags");
        assert!(
            matches!(mode.kind, FlagKind::Enum),
            "--mode kind MUST be Enum, got {:?}",
            mode.kind
        );
        let vs = mode.values.as_ref().expect("--mode carries values");
        let mut sorted = vs.clone();
        sorted.sort();
        assert_eq!(sorted, vec!["human", "json", "mcp", "quiet"]);
    }

    // `compile_time_features()` pushes string literals that must
    // exactly match the Cargo feature names in `Cargo.toml`. A
    // typo in the literal (`"oa_only"` vs `"oa-only"`) would
    // silently invert the inventory's `features` field for every
    // consumer. The default build has `oa-only` active; assert
    // the literal round-trips (#215 A9).
    #[test]
    fn compile_time_features_contains_oa_only_under_default() {
        // `cfg!(feature = "oa-only")` is true in the default test
        // build; if a future maintainer disables the default feature
        // for the test target, this test becomes meaningless but
        // does not cause a false failure.
        if cfg!(feature = "oa-only") {
            let f = compile_time_features();
            assert!(
                f.contains(&"oa-only"),
                "oa-only feature was enabled at compile time but \
                 `compile_time_features()` did not list it: {f:?}"
            );
        }
    }

    #[test]
    fn version_is_cargo_pkg_version() {
        assert_eq!(caps().version, env!("CARGO_PKG_VERSION"));
    }

    /// ADR-0028 D2: `user_extension_count` must reflect the number of
    /// `[[network.additional_hosts]]` entries actually present in
    /// `<config_dir>/doiget/config.toml`. The test points every
    /// config-dir env var at a tempdir, writes a 2-host config, and
    /// asserts the inventory reports `2`. Drift here would silently
    /// hide user-curated allowlist hosts from the cold-boot JSON.
    #[test]
    #[serial_test::serial]
    fn user_extension_count_reflects_config_toml_entries() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let cfg_root = camino::Utf8Path::from_path(tmp.path()).expect("utf8 tempdir");
        let doiget_dir = cfg_root.join("doiget");
        std::fs::create_dir_all(doiget_dir.as_std_path()).expect("mk dir");
        let config_toml = doiget_dir.join("config.toml");
        std::fs::write(
            config_toml.as_std_path(),
            "[[network.additional_hosts]]\n\
             host = \"example.org\"\n\
             \n\
             [[network.additional_hosts]]\n\
             host = \"*.example.net\"\n\
             note = \"university OA mirror\"\n",
        )
        .expect("write config.toml");

        let _x = EnvGuard::set("XDG_CONFIG_HOME", cfg_root.as_str());
        let _a = EnvGuard::unset("APPDATA");
        let _h = EnvGuard::unset("HOME");
        let _u = EnvGuard::unset("USERPROFILE");

        let cli = test_cli();
        let caps = build_capabilities(&cli);
        assert_eq!(
            caps.user_extension_count, 2,
            "expected 2 user-extension hosts, got {}",
            caps.user_extension_count
        );
    }

    /// Companion: with no config file (and a resolvable config dir),
    /// the count is `0` — the curated allowlist is the entire surface.
    /// Confirms the `Ok(vec![])` not-found path in `user_extension::load`
    /// flows through unchanged.
    #[test]
    #[serial_test::serial]
    fn user_extension_count_is_zero_without_config_toml() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let cfg_root = camino::Utf8Path::from_path(tmp.path()).expect("utf8 tempdir");

        let _x = EnvGuard::set("XDG_CONFIG_HOME", cfg_root.as_str());
        let _a = EnvGuard::unset("APPDATA");
        let _h = EnvGuard::unset("HOME");
        let _u = EnvGuard::unset("USERPROFILE");

        let caps = build_capabilities(&test_cli());
        assert_eq!(caps.user_extension_count, 0);
    }

    /// Minimal env-guard local to this tests module; mirrors the
    /// pattern in `commands::config::tests` (each module keeps its
    /// own copy so they stay leaf-level cheap).
    struct EnvGuard {
        var: &'static str,
        prior: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(var: &'static str, value: &str) -> Self {
            let prior = std::env::var_os(var);
            std::env::set_var(var, value);
            EnvGuard { var, prior }
        }
        fn unset(var: &'static str) -> Self {
            let prior = std::env::var_os(var);
            std::env::remove_var(var);
            EnvGuard { var, prior }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.prior {
                Some(v) => std::env::set_var(self.var, v),
                None => std::env::remove_var(self.var),
            }
        }
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
