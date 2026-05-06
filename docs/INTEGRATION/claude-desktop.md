# Claude Desktop integration

> **Status: PLACEHOLDER (Phase 3).** This file is a landing stub. Concrete
> configuration lands when `doiget serve` is real. See
> [`README.md`](./README.md) for the planned-files table and rationale.

[Claude Desktop](https://claude.ai/download) is Anthropic's desktop client and
hosts MCP servers over stdio. Once `doiget serve` ships in Phase 3, this guide
will document how to register `doiget` as a tool provider in Claude Desktop's
MCP configuration so that `resolve_doi`, `fetch_metadata`, and the rest of the
tool surface in [`../MCP_TOOLS.md`](../MCP_TOOLS.md) are callable from a chat.

## Configuration (Phase 3)

The example below is intentionally empty. Do not copy speculative JSON from
elsewhere — wait for the verified Phase 3 snippet.

```json
<!-- TODO Phase 3: paste verified Claude Desktop MCP server entry here. -->
```

```toml
<!-- TODO Phase 3: env-var block (see ../CONFIG.md for precedence). -->
```

## What to read in the meantime

- **Tool surface:** [`../MCP_TOOLS.md`](../MCP_TOOLS.md) — the exact tools
  Phase 3 exposes, including JSON-Schema for inputs and outputs.
- **Configuration:** [`../CONFIG.md`](../CONFIG.md) — env-var precedence and
  the `~/.config/doiget/` layout that any host wiring must respect.
- **Architecture:** [`../ARCHITECTURE.md`](../ARCHITECTURE.md) §4-6 — transport
  and capability-gate context for MCP integration.

## Contributing

If you have a working Claude Desktop integration ahead of Phase 3, open a
GitHub Discussion with the JSON-RPC trace and host-side config rather than a
PR — see [`README.md`](./README.md) §Contributing a snippet.
