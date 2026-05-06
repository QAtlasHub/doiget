# Chaining with paper-qa

> **Status: PLACEHOLDER (Phase 3).** This file is a landing stub. Concrete
> configuration lands when `doiget serve` is real. See
> [`README.md`](./README.md) for the planned-files table and rationale.
> Note: per the planned-files table this composition guide is scoped to
> **Phase 3+** — `doiget serve` ships first, then a verified chain recipe.

[paper-qa](https://github.com/Future-House/paper-qa) is a retrieval-augmented
question-answering system over scientific papers. The chain pattern this
guide will document keeps `doiget`'s role narrow — DOI resolution, metadata,
and content fetch via the tools in [`../MCP_TOOLS.md`](../MCP_TOOLS.md) — and
hands the resulting documents to paper-qa for embedding, retrieval, and
answer synthesis. Composition keeps each tool single-purpose and avoids
duplicating retrieval logic inside `doiget`.

## Configuration (Phase 3)

The example below is intentionally empty. The chain shape depends on the
final Phase 3 tool surface and on paper-qa's MCP/CLI ingestion entry point.

```toml
<!-- TODO Phase 3: paste verified doiget + paper-qa chain config here. -->
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

If you have a working `doiget` + paper-qa chain ahead of Phase 3, open a
GitHub Discussion with the orchestration script and any host-specific quirks
rather than a PR — see [`README.md`](./README.md) §Contributing a snippet.
