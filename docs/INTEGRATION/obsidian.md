# Obsidian backend export

> **Status: PLACEHOLDER (Phase 3).** This file is a landing stub. Concrete
> configuration lands when `doiget serve` is real. See
> [`README.md`](./README.md) for the planned-files table and rationale.
> Note: per the planned-files table, an Obsidian backend export is scoped to
> **Phase 7 (optional)** — this stub exists so future contributors have an
> obvious landing place rather than a 404.

[Obsidian](https://obsidian.md/) is a markdown-based knowledge base. The
Phase 7 Obsidian backend export will, when shipped, let `doiget` write
metadata and citation records into an Obsidian vault as plain Markdown notes
with frontmatter, so that the same data exposed by the MCP tools in
[`../MCP_TOOLS.md`](../MCP_TOOLS.md) can be browsed inside Obsidian.

## Configuration (Phase 3)

The example below is intentionally empty. The Obsidian backend export is a
Phase 7 deliverable; even the Phase 3 wiring is not yet implemented.

```toml
<!-- TODO Phase 3: paste verified Obsidian export config block here. -->
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

If you have a working Obsidian export workflow ahead of Phase 7, open a
GitHub Discussion describing the vault layout and frontmatter shape rather
than a PR — see [`README.md`](./README.md) §Contributing a snippet.
