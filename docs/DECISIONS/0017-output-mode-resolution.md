# 0017 - Output mode resolution (flag > env > implicit > TTY > quiet)

- **Date:** 2026-05-05
- **Status:** Accepted — fully implemented (#144, 0.2.1-beta.7). The resolution ladder (`--mode` > `--json`/`--quiet` > `DOIGET_MODE` > subcommand-implicit > TTY) is wired through `crates/doiget-cli/src/commands/output.rs::resolve` and threaded into every command's `run(..)`; the load-bearing MCP / stdout-purity invariant (Slice 9 + Slice 1) remains in force, now backed by the `serve → forced Mcp` override in `commands::main::run_dispatch`. Per-mode command behaviour (Quiet stdout suppression, Json bodies for human-table commands, ERRORS.md §3 batch JSONL) is staged as follow-up issues #203 / #204 / #205.
- **Supersedes:** -
- **Source:** Discussion #14

## Context

This is a stub ADR. The full Context, Decision, and Consequences sections will be
expanded during Phase 0 from the corresponding GitHub Discussion review threads.
The summary below is sufficient for ADR INDEX.md and for cross-references in the
NORMATIVE specs to resolve.

## Decision

Output mode resolution order: --mode CLI flag > DOIGET_MODE env > subcommand-implicit (e.g., serve forces mcp) > TTY detection > quiet (default). MCP mode strictly forbids stdout writes outside JSON-RPC frames; tracing-subscriber writer is redirected to stderr globally.

## Consequences

See the source Discussion for the analyzed positives and negatives. The binding
result is captured in the Decision above and is referenced from the relevant
NORMATIVE doc(s) under docs/.

To revise this decision, write a new ADR with Status: Accepted and Supersedes: 0017,
and update this file's Status to Superseded by NNNN per CONTRIBUTING.md.
