//! doiget CLI binary.
//!
//! `doiget` is an OA-first paper fetcher and stdio MCP server. The full
//! shipped subcommand surface is wired through [`run_dispatch`]: `fetch`,
//! `batch`, `bib`, `csl`, `info`, `search`, `list-recent`, `audit-log`,
//! `provenance`, `config`, `serve`, and (under `--features citation`)
//! `graph`. `serve` runs the rmcp-based MCP server in `doiget-mcp` over
//! stdio (ADR-0001).

use clap::{Parser, Subcommand};
use doiget_cli::commands::output::{self, FlagInput, OutputMode};

/// `doiget provenance ...` action selector. Ships only the v1→v2
/// migration in Slice 4 (ADR-0024); further actions (e.g. `compact`,
/// `rotate`) land in later slices.
#[derive(Subcommand, Debug)]
enum ProvenanceAction {
    /// Migrate the provenance log from v1 to v2 (one-shot, idempotent,
    /// dry-runnable per ADR-0024).
    Migrate {
        /// Preview the migration without touching disk. Prints the
        /// resulting [`MigrationReport`](doiget_core::provenance::MigrationReport)
        /// summary and exits.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Parser, Debug)]
#[command(
    name = "doiget",
    version,
    about = "Fetch academic papers via official Open Access APIs.",
    long_about = "doiget is an OA-first paper fetcher and stdio MCP server.\n\
                  \n\
                  Subcommands:\n\
                  \x20 fetch        Fetch a single paper PDF by DOI or arXiv id\n\
                  \x20 batch        Fetch many refs from a newline-separated file\n\
                  \x20 bib          Export a stored entry as BibTeX\n\
                  \x20 csl          Export a stored entry as CSL JSON\n\
                  \x20 info         Show metadata for a stored entry\n\
                  \x20 search       Search the local store by title / authors / venue\n\
                  \x20 list-recent  List the most recently fetched entries\n\
                  \x20 audit-log    Inspect or verify the provenance log\n\
                  \x20 provenance   Provenance-log lifecycle ops (migrate v1 -> v2)\n\
                  \x20 config       Show or doctor the resolved configuration\n\
                  \x20 serve        Run as an MCP server over stdio\n\
                  \x20 graph        Expand a DOI's citation neighborhood via OpenAlex\n\
                  \x20              (requires --features citation + DOIGET_ENABLE_OPENALEX)\n\
                  \x20 capabilities Emit a JSON inventory of the binary's full surface\n\
                  \x20              (for LLM cold-boot; #214)\n\
                  \n\
                  See README.md and docs/ for the full specification."
)]
struct Cli {
    /// Output mode (`human` | `json` | `quiet` | `mcp`). Highest-precedence
    /// signal in the ADR-0017 resolution ladder. Conflicts with `--json`
    /// and `--quiet`. `doiget serve` ignores this and always runs in `mcp`
    /// (CONFIG.md §5 — load-bearing security invariant for stdout purity).
    #[arg(
        long,
        global = true,
        value_enum,
        conflicts_with_all = ["json", "quiet"],
    )]
    mode: Option<OutputMode>,

    /// Short form of `--mode json` (CONFIG.md §5). Conflicts with `--mode`
    /// and `--quiet`.
    #[arg(long, global = true, conflicts_with_all = ["mode", "quiet"])]
    json: bool,

    /// Short form of `--mode quiet` (CONFIG.md §5). Conflicts with
    /// `--mode` and `--json`.
    #[arg(short = 'q', long, global = true, conflicts_with_all = ["mode", "json"])]
    quiet: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Fetch a single paper PDF by DOI or arXiv id.
    Fetch {
        /// DOI (e.g. "10.1234/example") or arXiv id (e.g. "arXiv:2401.12345").
        ref_: String,
        /// Build a fetch plan and emit it as JSON on stdout without
        /// touching the network, the store, or the provenance log
        /// (ADR-0022). The `plan.pdf_sources[].candidate_hosts` list is
        /// the static allowlist for the resolver, not a prediction of
        /// the single host the real fetch would hit (ADR-0022 §4).
        #[arg(long)]
        dry_run: bool,
    },
    /// Fetch many refs from a newline-separated text file.
    Batch {
        /// Path to a file containing one ref per line.
        path: String,
        /// Emit one fetch-plan JSON envelope per ref on stdout without
        /// touching the network, the store, or the provenance log
        /// (ADR-0022). Per-ref parse failures still cause a non-zero
        /// exit so a malformed batch is visible.
        #[arg(long)]
        dry_run: bool,
    },
    /// Show metadata for a stored entry.
    Info {
        /// DOI or arXiv id.
        ref_: String,
    },
    /// List the most recently fetched entries.
    ListRecent {
        /// Number of entries to show.
        #[arg(default_value_t = 10)]
        limit: usize,
    },
    /// Search the local store by title / authors / venue.
    Search {
        /// Query string.
        query: String,
    },
    /// Export an entry as BibTeX.
    Bib {
        /// DOI or arXiv id.
        ref_: String,
    },
    /// Export an entry as CSL JSON.
    Csl {
        /// DOI or arXiv id.
        ref_: String,
    },
    /// Inspect or verify the provenance log.
    AuditLog {
        /// Recompute the SHA-256 hash chain and report mismatches.
        #[arg(long)]
        verify: bool,
    },
    /// Provenance-log lifecycle operations (migrate v1 → v2 per
    /// ADR-0024).
    Provenance {
        #[command(subcommand)]
        action: ProvenanceAction,
    },
    /// Run as an MCP server over stdio.
    Serve,
    /// Emit a single JSON inventory of the binary's full surface
    /// (subcommands, args, env vars, modes, MCP tools, features).
    /// Designed for LLM cold-boot in one round-trip. See #214.
    Capabilities,
    /// Show or doctor the resolved configuration.
    Config {
        /// `show` / `path` / `doctor`
        action: String,
    },
    /// Expand a DOI's citation neighborhood via OpenAlex (BFS,
    /// ADR-0010 hard caps). Requires `--features citation` AND
    /// `DOIGET_ENABLE_OPENALEX` in env.
    #[cfg(feature = "citation")]
    Graph {
        /// DOI seed. arXiv ids are rejected (OpenAlex's
        /// `referenced_works` is DOI-keyed).
        ref_: String,
        /// Max BFS depth (1..=3). Default = 3 (ADR-0010 maximum).
        #[arg(long)]
        depth: Option<u32>,
        /// Max total nodes (1..=100). Default = 100.
        #[arg(long)]
        total: Option<u32>,
        /// Max children per parent (1..=20). Default = 20.
        #[arg(long)]
        per_paper: Option<u32>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Logging — strictly to stderr. See docs/SECURITY.md §3 / ADR-0001.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    let result: anyhow::Result<()> = run_dispatch(cli).await;

    // Issue #119: a `CliExit` carries a `docs/ERRORS.md` §4 process
    // exit code and means the human-readable `error[CODE]:` line was
    // ALREADY printed to stderr by the command. `main` owns the actual
    // process exit (doing it inside the command would kill in-process
    // integration tests). Every other error keeps the default anyhow
    // behaviour (Debug chain to stderr, exit 1).
    match result {
        Ok(()) => Ok(()),
        Err(err) => match err.downcast_ref::<doiget_cli::commands::fetch::CliExit>() {
            Some(doiget_cli::commands::fetch::CliExit(code)) => {
                std::process::exit(*code);
            }
            None => Err(err),
        },
    }
}

/// Build the `FlagInput` from the three mutually-exclusive global
/// flags. Clap's `conflicts_with_all` guarantees at most one is set, so
/// the ordering of the if-arms below is irrelevant to correctness.
fn flag_input_from(cli: &Cli) -> FlagInput {
    if let Some(m) = cli.mode {
        FlagInput::Explicit(m)
    } else if cli.json {
        FlagInput::JsonShort
    } else if cli.quiet {
        FlagInput::QuietShort
    } else {
        FlagInput::None
    }
}

/// Compute the `forced_implicit` mode from the subcommand. Only `serve`
/// pins a mode (`Mcp`) — CONFIG.md §5 / ADR-0017 / SECURITY.md §3: the
/// MCP server emits JSON-RPC frames on stdout and a `--mode quiet` /
/// `--mode human` override there would break the protocol, so the
/// override is unconditional.
fn forced_implicit_for(command: &Option<Command>) -> Option<OutputMode> {
    match command {
        Some(Command::Serve) => Some(OutputMode::Mcp),
        _ => None,
    }
}

async fn run_dispatch(cli: Cli) -> anyhow::Result<()> {
    // Resolve the effective output mode ONCE per invocation per ADR-0017.
    // The pure resolver lives in `commands::output`; this site is the
    // single I/O-touching layer that reads env + probes the TTY.
    // ADR-0017 Amendment 1: the resolver returns ResolvedOutput
    // carrying mode + quiet_was_explicit; informational commands receive
    // `out.mode` (their existing OutputMode signature), artifact
    // commands additionally consume `out.quiet_was_explicit` so the
    // non-TTY fallback to Quiet does NOT silence them (#219 / #220).
    let out = output::resolve(
        forced_implicit_for(&cli.command),
        flag_input_from(&cli),
        std::env::var("DOIGET_MODE").ok().as_deref(),
        output::stdout_is_tty(),
    );
    let mode = out.mode;

    match cli.command {
        None => {
            anyhow::bail!("no subcommand. Run `doiget --help` for available commands.");
        }
        // Phase 1 subcommands. All command modules live in the library half
        // of this crate (see `src/lib.rs`) so integration tests can drive them
        // in-process.
        //
        // Each command receives the resolved `mode`. Per-mode behaviour
        // (Quiet stdout suppression, Json bodies for human-table
        // commands) is tracked in follow-up issues #203 / #204 / #205;
        // this PR only wires the threading and the `serve→Mcp` invariant.
        Some(Command::AuditLog { verify }) => doiget_cli::commands::audit_log::run(verify, mode),
        Some(Command::Provenance { action }) => match action {
            ProvenanceAction::Migrate { dry_run } => {
                doiget_cli::commands::provenance::migrate(dry_run, mode)
            }
        },
        Some(Command::Config { action }) => doiget_cli::commands::config::run(action, mode),
        Some(Command::Info { ref_ }) => doiget_cli::commands::info::run(ref_, mode),
        Some(Command::ListRecent { limit }) => doiget_cli::commands::list_recent::run(limit, mode),
        Some(Command::Search { query }) => doiget_cli::commands::search::run(query, mode),
        Some(Command::Fetch { ref_, dry_run }) => {
            doiget_cli::commands::fetch::run_with_options(ref_, dry_run, mode).await
        }
        Some(Command::Batch { path, dry_run }) => {
            doiget_cli::commands::batch::run_with_options(path, dry_run, mode).await
        }
        Some(Command::Bib { ref_ }) => doiget_cli::commands::bib::run(ref_, mode),
        Some(Command::Csl { ref_ }) => doiget_cli::commands::csl::run(ref_, mode),
        // Phase 3 (MCP foundation). The MCP server runs on stdio per
        // ADR-0001. The `tracing_subscriber` installed at the top of
        // `main` is already redirected to stderr, so any rmcp / tool
        // tracing output will not collide with JSON-RPC frames on stdout.
        // See docs/SECURITY.md §3 / docs/MCP_TOOLS.md §8.
        //
        // The resolver above forces `mode == Mcp` here (CONFIG.md §5);
        // the mcp server itself hard-codes JSON-RPC framing on stdout
        // regardless, so the `mode` value is informational at this site.
        Some(Command::Serve) => {
            debug_assert_eq!(mode, OutputMode::Mcp, "serve must resolve to Mcp");
            let profile = doiget_core::CapabilityProfile::from_env()?;
            doiget_mcp::Server::new(profile).run().await
        }
        // #214 / #219 (ADR-0017 Amendment 1): single-shot inventory
        // for LLM cold-boot. We pass the live `clap::Command` AST so
        // the subcommand list cannot drift from the parser.
        // `capabilities` is an *artifact* command — it suppresses
        // stdout ONLY on **explicit** Quiet (`--quiet`/`-q`/
        // `DOIGET_MODE=quiet`/`--mode quiet`), never on the non-TTY
        // implicit fallback. The `quiet_was_explicit` discriminator
        // is what closes the LLM cold-boot deadlock reported in
        // #219 / #220.
        Some(Command::Capabilities) => {
            let cli_cmd = <Cli as clap::CommandFactory>::command();
            doiget_cli::commands::capabilities::run(&cli_cmd, mode, out.quiet_was_explicit)
        }
        // Phase 4 / Slice 16. Feature-gated to keep default release
        // binaries free of the OpenAlex-only citation walker.
        #[cfg(feature = "citation")]
        Some(Command::Graph {
            ref_,
            depth,
            total,
            per_paper,
        }) => doiget_cli::commands::graph::run(ref_, depth, total, per_paper, mode).await,
    }
}
