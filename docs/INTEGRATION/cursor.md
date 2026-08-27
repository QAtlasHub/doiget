# Cursor integration

> **Status: UNTESTED against Cursor as of 2026-08-26.** `doiget serve` is a
> plain stdio MCP server with no host-specific behaviour, and the snippet
> below is the standard stdio shape — but nobody has run it in Cursor and
> reported back. If you do, please say so on
> [#512](https://github.com/QAtlasHub/doiget/issues/512) and this banner
> comes off.

[Cursor](https://www.cursor.com/) supports MCP servers. Configuration is a
`mcp.json` file: `.cursor/mcp.json` inside a project, or `~/.cursor/mcp.json`
for every project.

```json
{
  "mcpServers": {
    "doiget": {
      "command": "doiget",
      "args": ["serve"],
      "env": {
        "DOIGET_STORE_ROOT": "/Users/you/papers",
        "DOIGET_CONTACT_EMAIL": "you@institution.edu"
      }
    }
  }
}
```

Use an absolute `command` path if `doiget` is not on the `PATH` the editor
inherits.

## Environment

Identical to every other host — see [`claude-code.md`](./claude-code.md)
§Environment. `DOIGET_STORE_ROOT` is the one that silently misbehaves if
omitted (ADR-0036, #369).

## Checking it worked

Have the agent call `doiget_health`. If Cursor's own MCP panel reports the
server as connected but no tool is callable, `doiget_capability_profile` will
say what this build is allowed to do.

Tool surface: [`../MCP_TOOLS.md`](../MCP_TOOLS.md).
