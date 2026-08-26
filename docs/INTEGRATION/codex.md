# OpenAI Codex CLI integration

> **Status: UNTESTED against Codex CLI as of 2026-08-26.** `doiget serve` is a
> plain stdio MCP server, and the snippet below is the standard stdio shape,
> but this specific host has not been exercised. Report success or failure on
> [#512](https://github.com/QAtlasHub/doiget/issues/512).

[Codex CLI](https://github.com/openai/codex) reads `~/.codex/config.toml`.
MCP servers go under `[mcp_servers.<name>]`:

```toml
[mcp_servers.doiget]
command = "doiget"
args = ["serve"]

[mcp_servers.doiget.env]
DOIGET_STORE_ROOT = "/home/you/papers"
DOIGET_CONTACT_EMAIL = "you@institution.edu"
```

Use an absolute `command` path if `doiget` is not on `PATH`.

## Environment

Identical to every other host — see [`claude-code.md`](./claude-code.md)
§Environment, [`../CONFIG.md`](../CONFIG.md) and
[`../CAPABILITY.md`](../CAPABILITY.md).

## Checking it worked

Ask for `doiget_health`, then `doiget_capability_profile`.

Tool surface: [`../MCP_TOOLS.md`](../MCP_TOOLS.md).
