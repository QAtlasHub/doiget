# Integration guides

> **Status: INFORMATIVE (placeholder).** Concrete host-integration snippets land
> in **Phase 3** alongside the MCP server (`doiget serve`). Until then, this
> directory is a pointer rather than a recipe collection.

## Why this is empty today

Phase 0 ships specifications (see [`../MCP_TOOLS.md`](../MCP_TOOLS.md) and
[`../PUBLIC_API.md`](../PUBLIC_API.md)) but no functional MCP server. Without a
runnable `doiget serve`, integration snippets would either be untested aspirational
config or would lead a reader to a Phase 0 stub error. Phase 3 ships the actual
server and the corresponding host snippets in this directory.

## Planned files (Phase 3)

| File | Host | Status |
|---|---|---|
| `claude-desktop.md` | Claude Desktop (stdio MCP) | Phase 3 (stub) |
| `cursor.md` | Cursor | Phase 3 (stub) |
| `codex.md` | OpenAI Codex CLI | Phase 3 (stub) |
| `claude-code.md` | Claude Code (this tool) | Phase 3 (stub) |
| `obsidian.md` | Obsidian backend export | Phase 7 (optional, stub) |
| `chain-with-paperqa.md` | Composition with paper-qa for content processing | Phase 3+ (stub) |

## What to read in the meantime

- **MCP tool spec:** [`../MCP_TOOLS.md`](../MCP_TOOLS.md) — the exact tool surface
  Phase 3 will expose. Sufficient to plan a host integration in advance.
- **Public Rust API:** [`../PUBLIC_API.md`](../PUBLIC_API.md) — for embedders that
  link `doiget-core` directly rather than going through the MCP server.
- **Architecture overview:** [`../ARCHITECTURE.md`](../ARCHITECTURE.md) §4-6 — for
  how tools, transport, and the capability gate fit together.
- **Configuration:** [`../CONFIG.md`](../CONFIG.md) — env-var precedence and the
  `~/.config/doiget/` layout that any host-side wiring will need to respect.

## Contributing a snippet

If you have a working integration with an MCP host that is not in the planned list,
open a GitHub Discussion describing the host, the JSON-RPC trace, and any
host-specific config quirks. PRs adding a new file under `docs/INTEGRATION/` will
be accepted in Phase 3+ once `doiget serve` is real and the snippet is verifiable.
