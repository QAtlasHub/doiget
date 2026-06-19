//! doiget CLI binary.
//!
//! `doiget` is an OA-first paper fetcher and stdio MCP server. The full
//! shipped subcommand surface is wired through [`run_dispatch`]: `fetch`,
//! `batch`, `bib`, `csl`, `info`, `search`, `list-recent`, `audit-log`,
//! `provenance`, `config`, `serve`, and (under `--features citation`)
//! `graph`. `serve` runs the rmcp-based MCP server in `doiget-mcp` over
//! stdio (ADR-0001).

use camino::Utf8PathBuf;
use clap::{Parser, Subcommand, ValueEnum};
use doiget_cli::commands::output::{self, FlagInput, OutputMode};

/// `--color` value (#211 / CONFIG.md §5).
///
/// Honored by future ANSI emitters; `Auto` decides per-stderr-TTY at the
/// emission site. The `NO_COLOR` cross-tool convention
/// (<https://no-color.org/>) is honored by the *consumer-side* resolver,
/// not at this write boundary: `apply_global_overrides` writes
/// `DOIGET_COLOR` from the flag value unconditionally, and a future
/// ANSI emitter MUST check `NO_COLOR` (present and non-empty, regardless
/// of value) before consulting `DOIGET_COLOR`. The split keeps the
/// write boundary stupid-simple and centralises the NO_COLOR
/// precedence in the emitter's own resolver where it is closer to the
/// actual rendering site.
///
/// This slice ships the *surface*: the flag is accepted and the resolved
/// value is exposed via `DOIGET_COLOR` so any future ANSI consumer can
/// read it through the same env-driven layer as the rest of the config.
/// No consumer currently emits ANSI to be gated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[clap(rename_all = "lower")]
enum OutputColor {
    /// Color when stderr is a terminal, monochrome otherwise. This is
    /// also the default when `--color` is not given; explicit
    /// `--color auto` is therefore a no-op vs the default unless
    /// `NO_COLOR` is set, in which case the *consumer-side* resolver
    /// still forces monochrome regardless of `--color`.
    Auto,
    /// Always emit ANSI, even on a pipe. Note that `NO_COLOR` (present
    /// + non-empty) still wins at the consumer-side resolver.
    Always,
    /// Never emit ANSI, even on a terminal.
    Never,
}

impl OutputColor {
    /// String representation written to `DOIGET_COLOR`.
    ///
    /// The match arms intentionally mirror the `#[clap(rename_all =
    /// "lower")]` parser-side spelling so a round trip through clap →
    /// `as_env_value` → env → future consumer is byte-identical.
    /// The pair is pinned by
    /// `tests::output_color_env_strings_match_clap_parser_side`, which
    /// rejects each variant's string against clap's own
    /// `ValueEnum::to_possible_value().get_name()` — if `rename_all`
    /// is changed (or a variant renamed), the test fires before any
    /// env consumer observes the drift.
    fn as_env_value(self) -> &'static str {
        match self {
            OutputColor::Auto => "auto",
            OutputColor::Always => "always",
            OutputColor::Never => "never",
        }
    }
}

/// Custom value parser for path-shaped CLI arguments.
///
/// Rejects empty strings and NUL bytes at parse time. NUL would
/// otherwise panic later in `std::env::set_var` (silent abort under
/// `panic = "abort"`); the empty-string case was a documented silent
/// failure path from the review pass (`apply_global_overrides`
/// would have written `DOIGET_*=""` which downstream resolvers treat
/// differently from an unset key).
///
/// Returning `Utf8PathBuf` honors the workspace `clippy.toml`
/// disallowed-types policy (`std::path::PathBuf` is banned) and
/// validates UTF-8 at the parse boundary instead of on first use.
fn parse_utf8_path(raw: &str) -> Result<Utf8PathBuf, String> {
    if raw.is_empty() {
        return Err("path must not be empty".to_string());
    }
    if raw.contains('\0') {
        return Err("path must not contain NUL bytes".to_string());
    }
    Ok(Utf8PathBuf::from(raw))
}

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
                  \x20 cite         Resolve a ref live and print clean BibTeX (doi2bib-style)\n\
                  \x20 csl          Export a stored entry as CSL JSON\n\
                  \x20 text         Extract a paper's full text from ar5iv (arXiv id)\n\
                  \x20 tex-source   Fetch raw LaTeX source from arXiv source API (arXiv id)\n\
                  \x20 link         Resolve a DOI to its arXiv preprint (OpenAlex)\n\
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

    /// Override the on-disk paper store root. CONFIG.md §5 / #211.
    /// Precedence: this flag > `DOIGET_STORE_ROOT` env > default
    /// (`$HOME/papers` on POSIX, `%USERPROFILE%\papers` on Windows).
    /// Wins by overwriting `DOIGET_STORE_ROOT` for the lifetime of
    /// this process before any command resolver reads it. Empty
    /// strings and NUL bytes are rejected at parse time by
    /// `parse_utf8_path`.
    #[arg(long, global = true, value_name = "PATH", value_parser = parse_utf8_path)]
    store_root: Option<Utf8PathBuf>,

    /// Override the provenance-log file path. CONFIG.md §5 / #211.
    /// Precedence: this flag > `DOIGET_LOG_PATH` env > default
    /// (`<config_dir>/doiget/access.jsonl`). Same wins-by-env-overwrite
    /// pattern as `--store-root` so existing resolvers
    /// (`commands::fetch::resolve_log_path` / `commands::audit_log`)
    /// pick the value up uniformly. Empty strings and NUL bytes are
    /// rejected at parse time by `parse_utf8_path`.
    #[arg(long, global = true, value_name = "PATH", value_parser = parse_utf8_path)]
    log_path: Option<Utf8PathBuf>,

    /// Force / suppress ANSI escapes on stderr output. CONFIG.md §5 /
    /// #211. Precedence: this flag > consumer-side `NO_COLOR` check >
    /// default (`auto`). See [`OutputColor`] for the precedence
    /// rationale — `NO_COLOR` is honored at the emission boundary,
    /// not at this write site.
    ///
    /// **Surface-only in this slice**: doiget currently emits no ANSI,
    /// so the flag is accepted, validated, and exposed via the
    /// `DOIGET_COLOR` env var for future emitters; nothing renders
    /// differently today.
    #[arg(long, global = true, value_enum)]
    color: Option<OutputColor>,

    /// Show the per-ref informational progress line on stderr.
    /// Conflicts with `--no-progress`. CONFIG.md §5 / #211.
    ///
    /// **Surface-only in this slice**: the `fetch` / `batch` per-ref
    /// stderr summary is unchanged in this PR; consumers that read
    /// `DOIGET_PROGRESS=1` will respect it in a follow-up.
    ///
    /// The pair `progress` / `no_progress` encodes a three-state
    /// value (true / false / unset) — see [`Cli::progress_choice`]
    /// for the typed view that callers should consume.
    #[arg(long, global = true, conflicts_with = "no_progress")]
    progress: bool,

    /// Suppress the per-ref informational progress line on stderr.
    /// Conflicts with `--progress`. See [`Cli::progress_choice`].
    #[arg(long, global = true, conflicts_with = "progress")]
    no_progress: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

impl Cli {
    /// Collapse the `progress` / `no_progress` bool pair into the
    /// three-state value it actually represents (review pass I7).
    ///
    /// Returns `Some(true)` for `--progress`, `Some(false)` for
    /// `--no-progress`, and `None` when neither flag was given.
    /// The `(true, true)` case is unreachable thanks to clap's
    /// `conflicts_with`, but the helper is unconditional so callers
    /// don't need to remember which flag the if-arm prioritises.
    fn progress_choice(&self) -> Option<bool> {
        match (self.progress, self.no_progress) {
            (true, _) => Some(true),
            (_, true) => Some(false),
            _ => None,
        }
    }
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
        /// Skip refs that already have a PDF in the store
        /// (`<store>/<safekey>.pdf` exists). Metadata-only entries are
        /// NOT skipped — they may now succeed via the preprint fallback
        /// or allowlist updates. Use after a partial batch run to
        /// re-attempt only failures without wasting API budget on
        /// already-fetched refs (issue #324).
        #[arg(long)]
        only_failed: bool,
        /// Seconds to sleep between individual fetches. Useful for hosts
        /// that throttle below the HTTP 429 threshold (issue #326).
        #[arg(long, value_name = "SECS")]
        delay: Option<f64>,
        /// Override the User-Agent header for every HTTP request in this
        /// batch. Default: `doiget/<version> (+https://github.com/sotashimozono/doiget)`.
        #[arg(long, value_name = "STRING")]
        user_agent: Option<String>,
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
    /// Search for papers. Default: external discovery over OpenAlex
    /// (`/works?search=`, ADR-0031) — turn a topic into ranked candidate
    /// papers with abstracts. Use `--local` to scan the local store
    /// (title / authors / venue) instead.
    Search {
        /// Query string (a topic for discovery, or a substring for `--local`).
        /// May be empty when `--local --tag` is given (matches all tagged entries).
        query: String,
        /// Scan the local store instead of external discovery.
        #[arg(long, conflicts_with = "external")]
        local: bool,
        /// Explicit form of the default external discovery (mutually
        /// exclusive with `--local`).
        #[arg(long, conflicts_with = "local")]
        external: bool,
        /// Filter local-store results to entries tagged with this tag
        /// (case-sensitive). Requires `--local`.
        #[arg(long, requires = "local")]
        tag: Option<String>,
        /// External: maximum results. Must be 1..=200 (OpenAlex's per-page
        /// cap); an out-of-range value is rejected, not clamped.
        #[arg(long, default_value_t = 25)]
        limit: usize,
        /// External: only works published in or after this year.
        #[arg(long)]
        from_year: Option<i32>,
        /// External: only works published in or before this year.
        #[arg(long)]
        to_year: Option<i32>,
        /// External: restrict to open-access works.
        #[arg(long)]
        oa_only: bool,
        /// External: only works cited strictly more than this many times.
        #[arg(long)]
        min_citations: Option<u64>,
        /// External: minimum field-and-year-normalized impact (FWCI). A
        /// quality FILTER (not a sort) — `fwci:>{f}`. See #290.
        #[arg(long)]
        min_fwci: Option<f64>,
        /// External: minimum within-cohort citation percentile (0–100),
        /// i.e. top-X% among same-year works (`cited_by_percentile_year`).
        /// Combine with `--from-year` for "recent and already standing
        /// out". See #290.
        #[arg(long)]
        min_percentile: Option<u8>,
        /// External: filter to an author by name (resolved to an OpenAlex
        /// author ID via `/authors?search=`).
        #[arg(long)]
        author: Option<String>,
        /// External: filter to a venue / journal by name (resolved to an
        /// OpenAlex source ID via `/sources?search=`).
        #[arg(long)]
        venue: Option<String>,
        /// External: filter to a publisher by name (resolved to an
        /// OpenAlex publisher ID via `/publishers?search=`).
        #[arg(long)]
        publisher: Option<String>,
        /// External: result ordering.
        #[arg(long, value_enum, default_value = "relevance")]
        sort: doiget_cli::commands::search::SortArg,
    },
    /// Export stored entries as BibTeX. A single ref, the whole store
    /// (`--all`), or a ref list (`--from-file`) — all rendered offline
    /// from the local store (#305).
    Bib {
        /// DOI or arXiv id. Omit when using `--all` or `--from-file`.
        ref_: Option<String>,
        /// Export every entry in the store as one deduplicated `.bib`.
        #[arg(long)]
        all: bool,
        /// Export the refs listed in FILE (one per line, or CSL-JSON /
        /// BibTeX), each rendered from the store; missing entries are
        /// skipped (and counted toward the exit code).
        #[arg(long, value_name = "FILE", value_parser = parse_utf8_path)]
        from_file: Option<camino::Utf8PathBuf>,
    },
    /// Resolve a ref live and print a clean BibTeX entry (doi2bib-style).
    /// Falls back to the local store when the live resolve fails, so an
    /// already-fetched ref always cites (#305).
    Cite {
        /// DOI or arXiv id.
        ref_: String,
        /// Skip the live resolve and render from the local store only
        /// (errors if the ref was never fetched).
        #[arg(long)]
        offline: bool,
    },
    /// Export stored entries as CSL JSON. A single ref, the whole store
    /// (`--all`), or a ref list (`--from-file`) — all rendered offline
    /// from the local store (#305).
    Csl {
        /// DOI or arXiv id. Omit when using `--all` or `--from-file`.
        ref_: Option<String>,
        /// Export every store entry as one deduplicated CSL JSON array.
        #[arg(long)]
        all: bool,
        /// Export the refs listed in FILE (one per line, or CSL-JSON /
        /// BibTeX), each rendered from the store; missing entries are
        /// skipped (and counted toward the exit code).
        #[arg(long, value_name = "FILE", value_parser = parse_utf8_path)]
        from_file: Option<camino::Utf8PathBuf>,
    },
    /// Extract a paper's full text from ar5iv as sectioned plain text
    /// (the #281 "read" step; ADR-0032). Takes an arXiv id; the PDF blob
    /// is never opened. A bare DOI reports `NO_OA_AVAILABLE` (pass the
    /// arXiv id). Tier-1 OA, always-on.
    Text {
        /// arXiv id (e.g. "arxiv:2401.12345"). A DOI is reported as
        /// having no full-text source yet (#281 item 5).
        ref_: String,
        /// Cap the section body text to this many characters (the title and
        /// section headings are not counted; truncation is flagged on
        /// `truncated`). Omit for the full text.
        #[arg(long)]
        max_chars: Option<usize>,
        /// Bypass the on-disk text cache (always re-fetch from ar5iv).
        #[arg(long)]
        no_cache: bool,
    },
    /// Discover the frontier neighbourhood of a seed paper: surfaces papers
    /// that **cite the seed**, ranked by age-normalized impact (fwci),
    /// excluding papers already in the local store (#295).
    Frontier {
        /// Seed DOI (e.g. "10.1103/PhysRevB.1").
        doi: String,
        /// Maximum results to return (1–200; default 25).
        #[arg(long, default_value_t = 25)]
        limit: usize,
        /// Only include works published on or after this year.
        #[arg(long)]
        from_year: Option<i32>,
    },
    /// Fetch the raw LaTeX source of an arXiv paper directly from the arXiv
    /// source API (`export.arxiv.org/src/<id>`). More reliable than `text`
    /// (ar5iv HTML) for papers not yet processed by LaTeXML. PDF-only
    /// submissions yield `TEXT_UNAVAILABLE`. Tier-1 OA, always-on.
    TexSource {
        /// arXiv id (e.g. "arxiv:2401.12345"). A DOI reports `NO_OA_AVAILABLE`.
        ref_: String,
        /// Cap the returned LaTeX source to this many characters (truncation is
        /// flagged, never silent). Omit for the full source.
        #[arg(long)]
        max_chars: Option<usize>,
        /// Bypass the on-disk TeX source cache (always re-fetch).
        #[arg(long)]
        no_cache: bool,
    },
    /// Resolve a DOI to its arXiv preprint + identity cluster (OpenAlex;
    /// #281 item 5). Reports whether the same work has a free arXiv
    /// preprint, for reading or dedup. arXiv → DOI is a follow-up.
    Link {
        /// DOI (e.g. "10.1103/PhysRevB.1"). A non-DOI ref is rejected.
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
    /// Print the current version. With `--check`, query GitHub Releases for
    /// the latest stable tag and report whether an update is available.
    Version {
        /// Check GitHub Releases for a newer stable release.
        #[arg(long)]
        check: bool,
    },
    /// Verify that every DOI / arXiv reference in a bibliography file
    /// (.bib / CSL-JSON / plain refs) resolves to real metadata. Does
    /// not download PDFs or write to the store. Emits one JSON-Lines
    /// record per entry; exits non-zero on failures.
    Verify {
        /// Path to the bibliography file to verify.
        path: String,
        /// Input format; auto-detected from extension / content by default.
        #[arg(long, default_value = "auto")]
        format: String,
        /// Treat unresolved and id-less entries as failures (exit
        /// non-zero), not just warnings. Use in a network-stable CI lane.
        #[arg(long)]
        strict: bool,
    },
    /// Structurally validate a BibTeX bibliography (duplicate keys,
    /// missing fields, blank entries, `$$` title math) WITHOUT resolving
    /// DOIs or touching the network. Read-only; emits one JSON-Lines
    /// finding per issue and exits non-zero on errors.
    Lint {
        /// Path to the BibTeX file to lint.
        path: String,
        /// Promote warnings to errors so any finding fails the run.
        #[arg(long)]
        strict: bool,
    },
    /// Assign or remove tags and collections on a stored entry (#294).
    ///
    /// Positional `<tag>...` arguments add tags. Use `--remove` to remove.
    /// Use `--collection` / `--remove-collection` for collection membership.
    /// Use `--list` to show current tags, collections, and annotation.
    Tag {
        /// DOI or arXiv id.
        ref_: String,
        /// Tags to add (idempotent). Positional — may repeat.
        #[arg(value_name = "TAG")]
        tags: Vec<String>,
        /// Tags to remove.
        #[arg(long = "remove", value_name = "TAG")]
        remove: Vec<String>,
        /// Collections to join (idempotent).
        #[arg(long = "collection", value_name = "COL")]
        collection: Vec<String>,
        /// Collections to leave.
        #[arg(long = "remove-collection", value_name = "COL")]
        remove_collection: Vec<String>,
        /// Show current tags, collections, and annotation; no mutation.
        #[arg(long)]
        list: bool,
    },
    /// Set or clear the freeform annotation for a stored entry (#294).
    Annotate {
        /// DOI or arXiv id.
        ref_: String,
        /// Annotation text (replaces any previous annotation).
        text: Option<String>,
        /// Clear the annotation (remove it from the TOML).
        #[arg(long)]
        clear: bool,
    },
    /// Resolve a bibliographic citation string to ranked DOI candidates.
    #[command(name = "resolve-citation")]
    ResolveCitation {
        /// Bibliographic citation query string (e.g. "Onsager 1944").
        query: String,
        /// Maximum number of candidates to return (default: 5).
        #[arg(long, default_value_t = 5)]
        limit: u8,
    },
    /// Batch resolve bibliographic citation strings from stdin.
    #[command(name = "batch-resolve-citations")]
    BatchResolveCitations {
        /// Maximum number of candidates to return per citation (default: 5).
        #[arg(long, default_value_t = 5)]
        limit: u8,
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

/// Apply the four CONFIG.md §5 global flags (#211) by overwriting the
/// matching process env vars before any command resolver reads them.
///
/// This keeps the precedence ladder `flag > env > default` correct
/// uniformly across every command without threading a new
/// `ResolvedFlags` value through eleven `run(..)` signatures: the
/// existing env-driven resolvers (`resolve_store_root`,
/// `resolve_log_path`, the future `--color` / `--progress`
/// consumers) keep reading env and will see the flag value when one
/// is given.
///
/// `--color` and `--progress` / `--no-progress` are *surface-only* in
/// this slice — they are accepted and exposed via `DOIGET_COLOR` /
/// `DOIGET_PROGRESS` for future consumers, but doiget does not yet
/// emit ANSI escapes or per-ref progress lines that would respect
/// them. `NO_COLOR=1` (per <https://no-color.org/>) still wins over
/// `--color always` because the consumer-side resolver checks
/// `NO_COLOR` first by the standard convention.
///
/// **Thread safety:** `std::env::set_var` is safe on edition 2021 /
/// Rust 1.86 (the workspace MSRV; cf. `Cargo.toml [workspace.package]
/// edition`). Under edition 2024 it becomes `unsafe fn` (the
/// `setenv` POSIX call is documented as non-thread-safe), and the
/// workspace `-F unsafe-code` lint will need a localized
/// `#[allow(unsafe_code)]` here when migrating. Tracked under #211
/// follow-up.
///
/// We invoke this from `run_dispatch` (see call site below) before
/// any `.await` in the function. The `#[tokio::main]` runtime has
/// already constructed its multi-thread worker pool by that point,
/// but the workers are parked on the work queue and do not read
/// `environ`; the active thread is the binary's startup thread. The
/// concurrent-read soundness condition for `setenv` is therefore met.
/// If a future change introduces an async background task that reads
/// `DOIGET_STORE_ROOT` (or any other key this function writes), the
/// application of overrides must move ahead of the runtime
/// construction (i.e. above the `#[tokio::main]` boundary).
///
/// The function intentionally has no error path: each flag value has
/// already passed `parse_utf8_path` (empty and NUL rejected) or
/// clap's `ValueEnum` (closed set of strings), so `set_var` cannot
/// panic on the value. The function is fire-and-forget by design.
fn apply_global_overrides(cli: &Cli) {
    if let Some(v) = cli.store_root.as_deref() {
        std::env::set_var("DOIGET_STORE_ROOT", v.as_str());
    }
    if let Some(v) = cli.log_path.as_deref() {
        std::env::set_var("DOIGET_LOG_PATH", v.as_str());
    }
    if let Some(c) = cli.color {
        std::env::set_var("DOIGET_COLOR", c.as_env_value());
    }
    // `--progress` / `--no-progress` collapse into a typed
    // three-state via `Cli::progress_choice` (review pass I7). When
    // neither is given we leave `DOIGET_PROGRESS` untouched so a
    // user-set env value survives and the future consumer can fall
    // back to its own TTY-based default.
    if let Some(on) = cli.progress_choice() {
        std::env::set_var("DOIGET_PROGRESS", if on { "1" } else { "0" });
    }
}

async fn run_dispatch(cli: Cli) -> anyhow::Result<()> {
    // CONFIG.md §5 / #211: apply global path / display flags by
    // overwriting their env counterparts BEFORE any resolver reads
    // them. The precedence
    //
    //   flag (this fn) > env > default
    //
    // is implemented by `apply_global_overrides`'s
    // `--flag-wins-by-setting-env-first` pattern: when a flag is
    // supplied, its value replaces the env value; when no flag is
    // supplied, the env value (or the resolver's default) stands.
    apply_global_overrides(&cli);

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
        Some(Command::Info { ref_ }) => {
            doiget_cli::commands::info::run(ref_, mode, out.quiet_was_explicit)
        }
        Some(Command::ListRecent { limit }) => {
            doiget_cli::commands::list_recent::run(limit, mode, out.quiet_was_explicit)
        }
        Some(Command::Search {
            query,
            local,
            external: _,
            tag,
            limit,
            from_year,
            to_year,
            oa_only,
            min_citations,
            min_fwci,
            min_percentile,
            author,
            venue,
            publisher,
            sort,
        }) => {
            let ext = doiget_cli::commands::search::ExternalArgs {
                limit,
                from_year,
                to_year,
                oa_only,
                min_citations,
                min_fwci,
                min_percentile,
                author,
                venue,
                publisher,
                sort,
            };
            doiget_cli::commands::search::run(query, local, tag, ext, mode, out.quiet_was_explicit)
                .await
        }
        Some(Command::Version { check }) => doiget_cli::commands::version::run(check, mode).await,
        Some(Command::Verify {
            path,
            format,
            strict,
        }) => doiget_cli::commands::verify::run(path, format, strict, mode).await,
        Some(Command::Lint { path, strict }) => doiget_cli::commands::lint::run(path, strict, mode),
        Some(Command::Tag {
            ref_,
            tags,
            remove,
            collection,
            remove_collection,
            list,
        }) => doiget_cli::commands::tag::run(
            ref_,
            tags,
            remove,
            collection,
            remove_collection,
            list,
            mode,
            out.quiet_was_explicit,
        ),
        Some(Command::Annotate { ref_, text, clear }) => {
            doiget_cli::commands::tag::run_annotate(ref_, text, clear)
        }
        Some(Command::ResolveCitation { query, limit }) => {
            doiget_cli::commands::resolve_citation::run(query, limit, mode).await
        }
        Some(Command::BatchResolveCitations { limit }) => {
            doiget_cli::commands::resolve_citation::run_batch(limit, mode).await
        }
        Some(Command::Fetch { ref_, dry_run }) => {
            doiget_cli::commands::fetch::run_with_options(ref_, dry_run, mode).await
        }
        Some(Command::Batch {
            path,
            dry_run,
            only_failed,
            delay,
            user_agent,
        }) => {
            doiget_cli::commands::batch::run_with_options(
                path,
                dry_run,
                only_failed,
                delay,
                user_agent,
                mode,
            )
            .await
        }
        Some(Command::Bib {
            ref_,
            all,
            from_file,
        }) => doiget_cli::commands::bib::run(ref_, all, from_file, mode),
        Some(Command::Cite { ref_, offline }) => {
            doiget_cli::commands::cite::run(ref_, offline, mode).await
        }
        Some(Command::Csl {
            ref_,
            all,
            from_file,
        }) => doiget_cli::commands::csl::run(ref_, all, from_file, mode),
        Some(Command::Text {
            ref_,
            max_chars,
            no_cache,
        }) => {
            doiget_cli::commands::text::run(ref_, max_chars, no_cache, mode, out.quiet_was_explicit)
                .await
        }
        Some(Command::Frontier {
            doi,
            limit,
            from_year,
        }) => {
            doiget_cli::commands::frontier::run(doi, limit, from_year, mode, out.quiet_was_explicit)
                .await
        }
        Some(Command::TexSource {
            ref_,
            max_chars,
            no_cache,
        }) => {
            doiget_cli::commands::tex_source::run(
                ref_,
                max_chars,
                no_cache,
                mode,
                out.quiet_was_explicit,
            )
            .await
        }
        Some(Command::Link { ref_ }) => {
            doiget_cli::commands::link::run(ref_, mode, out.quiet_was_explicit).await
        }
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

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use clap::Parser;
    use serial_test::serial;

    /// `--local` and `--external` are mutually exclusive (ADR-0031 D5).
    /// `--external`'s `conflicts_with = "local"` makes the conflict
    /// symmetric so the parse fails regardless of flag order.
    #[test]
    fn search_local_and_external_conflict() {
        assert!(
            Cli::try_parse_from(["doiget", "search", "--local", "--external", "q"]).is_err(),
            "`search --local --external` must be a parse error"
        );
        assert!(
            Cli::try_parse_from(["doiget", "search", "--external", "--local", "q"]).is_err(),
            "conflict must hold regardless of flag order"
        );
    }

    /// RAII guard restoring a single env var to its pre-test value (or
    /// removing it if previously unset). The `apply_global_overrides`
    /// tests mutate `DOIGET_STORE_ROOT` / `DOIGET_LOG_PATH` /
    /// `DOIGET_COLOR` / `DOIGET_PROGRESS`; without restoration a
    /// subsequent test on the same process would see stale state.
    struct EnvGuard {
        key: &'static str,
        prev: Option<String>,
    }

    impl EnvGuard {
        fn save(key: &'static str) -> Self {
            Self {
                key,
                prev: std::env::var(key).ok(),
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    fn parse_cli(args: &[&str]) -> Cli {
        // Prepend the binary name; clap requires argv[0].
        let mut argv = vec!["doiget"];
        argv.extend_from_slice(args);
        Cli::parse_from(argv)
    }

    // ---- store_root precedence (flag > env) -----------------------------

    #[test]
    #[serial]
    fn store_root_flag_overwrites_env() {
        let _g = EnvGuard::save("DOIGET_STORE_ROOT");
        std::env::set_var("DOIGET_STORE_ROOT", "/env/path");
        let cli = parse_cli(&["--store-root", "/flag/path", "capabilities"]);
        apply_global_overrides(&cli);
        assert_eq!(
            std::env::var("DOIGET_STORE_ROOT").unwrap(),
            "/flag/path",
            "--store-root MUST win over DOIGET_STORE_ROOT env"
        );
    }

    #[test]
    #[serial]
    fn store_root_env_preserved_when_no_flag() {
        let _g = EnvGuard::save("DOIGET_STORE_ROOT");
        std::env::set_var("DOIGET_STORE_ROOT", "/env/path");
        let cli = parse_cli(&["capabilities"]);
        apply_global_overrides(&cli);
        assert_eq!(
            std::env::var("DOIGET_STORE_ROOT").unwrap(),
            "/env/path",
            "DOIGET_STORE_ROOT env MUST survive when --store-root is not given"
        );
    }

    // ---- log_path precedence (flag > env) -------------------------------

    #[test]
    #[serial]
    fn log_path_flag_overwrites_env() {
        let _g = EnvGuard::save("DOIGET_LOG_PATH");
        std::env::set_var("DOIGET_LOG_PATH", "/env/log.jsonl");
        let cli = parse_cli(&["--log-path", "/flag/log.jsonl", "capabilities"]);
        apply_global_overrides(&cli);
        assert_eq!(
            std::env::var("DOIGET_LOG_PATH").unwrap(),
            "/flag/log.jsonl",
            "--log-path MUST win over DOIGET_LOG_PATH env"
        );
    }

    #[test]
    #[serial]
    fn log_path_env_preserved_when_no_flag() {
        // Symmetric to `store_root_env_preserved_when_no_flag`: when
        // `--log-path` is not given, a user-set `DOIGET_LOG_PATH` env
        // value MUST NOT be blanked or clobbered by
        // `apply_global_overrides`. A regression that swapped the
        // `if let Some(v) = …` guard for an unconditional `set_var`
        // would be silently caught here. Review pass I2.
        let _g = EnvGuard::save("DOIGET_LOG_PATH");
        std::env::set_var("DOIGET_LOG_PATH", "/env/log.jsonl");
        let cli = parse_cli(&["capabilities"]);
        apply_global_overrides(&cli);
        assert_eq!(
            std::env::var("DOIGET_LOG_PATH").unwrap(),
            "/env/log.jsonl",
            "DOIGET_LOG_PATH env MUST survive when --log-path is not given"
        );
    }

    // ---- color flag -> DOIGET_COLOR env ---------------------------------

    #[test]
    #[serial]
    fn color_flag_writes_doiget_color_env() {
        let _g = EnvGuard::save("DOIGET_COLOR");
        std::env::remove_var("DOIGET_COLOR");
        for (arg, expected) in [("auto", "auto"), ("always", "always"), ("never", "never")] {
            let cli = parse_cli(&["--color", arg, "capabilities"]);
            apply_global_overrides(&cli);
            assert_eq!(
                std::env::var("DOIGET_COLOR").unwrap(),
                expected,
                "--color {arg} MUST write {expected} to DOIGET_COLOR"
            );
        }
    }

    #[test]
    #[serial]
    fn color_unset_when_no_flag_leaves_env_untouched() {
        let _g = EnvGuard::save("DOIGET_COLOR");
        std::env::remove_var("DOIGET_COLOR");
        let cli = parse_cli(&["capabilities"]);
        apply_global_overrides(&cli);
        assert!(
            std::env::var("DOIGET_COLOR").is_err(),
            "absent --color MUST leave DOIGET_COLOR unset"
        );
    }

    #[test]
    #[serial]
    fn color_env_preserved_when_no_flag() {
        // Sentinel-preservation symmetric to `neither_progress_…`. A
        // user who has pre-set `DOIGET_COLOR` in their shell must see
        // that value survive a `doiget` invocation that does NOT
        // include `--color`. Review pass I3 — the previous test only
        // exercised the unset → unset path, which is satisfied by a
        // regression that blanks the env on every invocation.
        let _g = EnvGuard::save("DOIGET_COLOR");
        std::env::set_var("DOIGET_COLOR", "sentinel");
        let cli = parse_cli(&["capabilities"]);
        apply_global_overrides(&cli);
        assert_eq!(
            std::env::var("DOIGET_COLOR").unwrap(),
            "sentinel",
            "absent --color MUST NOT clobber a user-set DOIGET_COLOR env"
        );
    }

    // ---- ValueEnum drift guard (A1 follow-up) ---------------------------

    #[test]
    fn output_color_env_strings_match_clap_parser_side() {
        // `as_env_value` is a hand-written match; the clap parser-side
        // strings come from `#[clap(rename_all = "lower")]` +
        // `ValueEnum`. If anyone changes `rename_all` (or renames a
        // variant) without updating `as_env_value`, the round trip
        // `--color <s> → OutputColor → as_env_value → DOIGET_COLOR`
        // would silently drift. This test makes the dependency
        // mechanical: clap's `to_possible_value().get_name()` is the
        // canonical parser-side spelling, and we assert each variant's
        // `as_env_value` matches it exactly.
        for variant in [OutputColor::Auto, OutputColor::Always, OutputColor::Never] {
            let parser_side = variant
                .to_possible_value()
                .expect("clap value-enum exposes every non-skipped variant");
            assert_eq!(
                variant.as_env_value(),
                parser_side.get_name(),
                "as_env_value MUST mirror clap's rename_all-driven name for {variant:?}"
            );
        }
    }

    // ---- progress flags -> DOIGET_PROGRESS env --------------------------

    #[test]
    #[serial]
    fn progress_flag_writes_one() {
        let _g = EnvGuard::save("DOIGET_PROGRESS");
        std::env::remove_var("DOIGET_PROGRESS");
        let cli = parse_cli(&["--progress", "capabilities"]);
        apply_global_overrides(&cli);
        assert_eq!(std::env::var("DOIGET_PROGRESS").unwrap(), "1");
    }

    #[test]
    #[serial]
    fn no_progress_flag_writes_zero() {
        let _g = EnvGuard::save("DOIGET_PROGRESS");
        std::env::remove_var("DOIGET_PROGRESS");
        let cli = parse_cli(&["--no-progress", "capabilities"]);
        apply_global_overrides(&cli);
        assert_eq!(std::env::var("DOIGET_PROGRESS").unwrap(), "0");
    }

    #[test]
    #[serial]
    fn neither_progress_nor_no_progress_leaves_env_untouched() {
        let _g = EnvGuard::save("DOIGET_PROGRESS");
        std::env::set_var("DOIGET_PROGRESS", "sentinel");
        let cli = parse_cli(&["capabilities"]);
        apply_global_overrides(&cli);
        assert_eq!(
            std::env::var("DOIGET_PROGRESS").unwrap(),
            "sentinel",
            "absent --progress/--no-progress MUST NOT clobber a user-set DOIGET_PROGRESS env"
        );
    }

    // ---- parse_utf8_path (I6 empty, A4 NUL) -----------------------------

    #[test]
    fn parse_utf8_path_accepts_normal_path() {
        let p = parse_utf8_path("/tmp/papers").expect("normal path");
        assert_eq!(p.as_str(), "/tmp/papers");
    }

    #[test]
    fn parse_utf8_path_accepts_windows_style_path() {
        // The parser is intentionally non-validating beyond UTF-8,
        // empty, and NUL — any other path-shaped string passes through
        // (the platform's own path resolution decides validity).
        let p = parse_utf8_path("C:\\Users\\me\\papers").expect("windows path");
        assert_eq!(p.as_str(), "C:\\Users\\me\\papers");
    }

    #[test]
    fn parse_utf8_path_rejects_empty_string() {
        // Review pass I6: prevents silently writing `DOIGET_*=""`.
        let err = parse_utf8_path("").expect_err("empty rejected");
        assert!(
            err.contains("empty"),
            "error message MUST identify the empty-string condition, got: {err}"
        );
    }

    #[test]
    fn parse_utf8_path_rejects_nul_byte() {
        // Review pass A4: `std::env::set_var` panics on NUL bytes
        // (and the workspace is `panic = abort` in release, so the
        // process would die silently). Reject at the parse boundary.
        let err = parse_utf8_path("a/b\0/c").expect_err("NUL rejected");
        assert!(
            err.to_ascii_lowercase().contains("nul"),
            "error message MUST identify the NUL condition, got: {err}"
        );
    }

    #[test]
    fn cli_rejects_empty_path_flag_value_at_parse_time() {
        // Clap returns Err from `try_parse_from` when a value_parser
        // rejects the input. The parse failure exit code (2) is
        // exercised at the e2e layer; here we only assert the parse
        // attempt itself fails.
        let res = Cli::try_parse_from(["doiget", "--store-root", "", "capabilities"]);
        assert!(
            res.is_err(),
            "--store-root with empty value MUST parse-fail"
        );
    }

    #[test]
    fn cli_rejects_nul_in_path_flag_value_at_parse_time() {
        let res = Cli::try_parse_from(["doiget", "--log-path", "a/b\0/c", "capabilities"]);
        assert!(res.is_err(), "--log-path with NUL byte MUST parse-fail");
    }
}
