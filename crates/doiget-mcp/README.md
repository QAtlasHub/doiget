# doiget-mcp

> stdio MCP server for [doiget](https://github.com/sotashimozono/doiget). Exposed
> by the `doiget serve` subcommand from `doiget-cli` so an MCP host (Claude Desktop,
> Cursor, Codex, Claude Code) can resolve DOIs and arXiv ids, fetch Open Access PDFs
> through official publisher APIs, and inspect the local store.

## Status

**Phase 0 skeleton.** `Server::new(...)` constructs a server bound to a resolved
`doiget_core::CapabilityProfile`, but `Server::run` currently returns an error of the
form `"doiget-mcp vX.Y.Z (Phase 0): MCP server is not yet implemented"`. The JSON-RPC
loop, tool dispatch, and the 9-tool surface land in **Phase 3**. See
[`docs/PHASES.md`](../../docs/PHASES.md) for the implementation plan.

This crate is published to crates.io ahead of Phase 3 so that downstream tooling
(packagers, capability profile consumers, and the `doiget-cli` `serve` subcommand) can
take a stable dependency on the type surface.

## Why stdio only

doiget speaks **stdio JSON-RPC only**. There is no HTTP / SSE / WebSocket transport,
and there will not be one. This is a permanent architectural decision recorded in
[ADR-0001 — MCP transport is stdio only](../../docs/DECISIONS/0001-stdio-only.md), and
it is enforced structurally:

- No Cargo feature exists for an HTTP transport.
- `cargo-deny` bans HTTP server crates (`axum`, `actix-web`, `warp`, `tide`, `hyper`
  server) workspace-wide.
- `clippy::print_stdout` is `deny` in this crate so stdout stays a JSON-RPC frame
  channel only. Logs go to stderr via `tracing-subscriber`.

The defense-in-depth payoff: no listening port, no auth surface, no SSRF vector, no
multi-tenant accounting. doiget remains a strictly local tool that the user runs on
their own machine under their own credentials.

## Tool surface (planned, Phase 3)

The 9-tool baseline shipped at Phase 3 (full spec in
[`docs/MCP_TOOLS.md`](../../docs/MCP_TOOLS.md)):

| Tool | Purpose |
|---|---|
| `doiget_resolve_paper` | Resolve DOI / arXiv id to authoritative metadata. |
| `doiget_fetch_paper` | Resolve and download a single PDF to the store. |
| `doiget_batch_fetch` | Up to 100 refs in one call. |
| `doiget_info` | Retrieve a store entry's metadata. |
| `doiget_search_local` | Search store metadata (title / authors / venue). |
| `doiget_list_recent` | Last N fetched entries. |
| `doiget_paper_pdf_path` | Return the local path of a cached PDF. Does not read content. |
| `doiget_capability_profile` | Report which sources this instance is allowed to use. |
| `doiget_health` | Operational sanity (store writable, version, schema). |

Phase 4 adds `doiget_expand_citation_graph`, `doiget_bibtex_export`, and
`doiget_csl_export`. Permanent exclusions (no `doiget_delete_paper`,
`doiget_set_credentials`, `doiget_run_shell`, `doiget_fetch_url`) are listed in
[`docs/MCP_TOOLS.md`](../../docs/MCP_TOOLS.md) §6.

## Embedding in an MCP host

End users do not depend on this crate directly. MCP hosts launch the server by
configuring the `doiget` binary's `serve` subcommand, e.g. for Claude Desktop:

```jsonc
{
  "mcpServers": {
    "doiget": {
      "command": "doiget",
      "args": ["serve"]
    }
  }
}
```

stdin EOF triggers a 5-second graceful shutdown. See
[`docs/MCP_TOOLS.md`](../../docs/MCP_TOOLS.md) §8 for lifecycle details and
[`docs/INTEGRATION/`](../../docs/INTEGRATION/) for per-host integration notes.

## License

MIT. See [`LICENSE`](../../LICENSE) at the workspace root.
