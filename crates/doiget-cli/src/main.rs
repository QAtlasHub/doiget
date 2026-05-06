//! doiget CLI binary.
//!
//! Phase 0 ships the CLI skeleton: `doiget --help` works, and each subcommand
//! returns a Phase-1-pending error message. Real implementations land in Phase 1+.
//!
//! Phase 1 progressively replaces the Phase-0 bail-out per subcommand. The
//! `config` subcommand is the first to land — see `commands/config.rs`.

mod commands;

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "doiget",
    version,
    about = "Fetch academic papers via official Open Access APIs.",
    long_about = "doiget is an OA-first paper fetcher and stdio MCP server.\n\
                  See README.md and docs/ for the full specification.\n\
                  This is the Phase 0 skeleton; subcommands are not yet implemented."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Fetch a single paper PDF by DOI or arXiv id.
    Fetch {
        /// DOI (e.g. "10.1234/example") or arXiv id (e.g. "arXiv:2401.12345").
        ref_: String,
    },
    /// Fetch many refs from a newline-separated text file.
    Batch {
        /// Path to a file containing one ref per line.
        path: String,
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
    /// Run as an MCP server over stdio.
    Serve,
    /// Show or doctor the resolved configuration.
    Config {
        /// `show` / `path` / `doctor`
        action: String,
    },
}

fn main() -> anyhow::Result<()> {
    // Logging — strictly to stderr. See docs/SECURITY.md §3 / ADR-0001.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    match cli.command {
        None => {
            anyhow::bail!("no subcommand. Run `doiget --help` for available commands.");
        }
        Some(Command::Config { action }) => commands::config::run(action),
        Some(_cmd) => {
            anyhow::bail!(
                "doiget {} (Phase 0): subcommands are not yet implemented. \
                 See docs/PHASES.md.",
                doiget_core::VERSION
            );
        }
    }
}
