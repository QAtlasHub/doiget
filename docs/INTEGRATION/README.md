# Integration guides

> **Status: INFORMATIVE.** `doiget serve` ships; these are the host-side
> snippets. Each page states whether it has been exercised against that
> host, and the ones that have not say so.

`doiget serve` is a stdio MCP server. It is the same entry point everywhere —
`mcpb/manifest.json` runs `doiget serve`, and so does the MCP Registry entry.
The differences between hosts are only where the config file lives and what it
is called.

| File | Host | Exercised? |
|---|---|---|
| [`claude-code.md`](./claude-code.md) | Claude Code | **Yes** — this is what the project is developed under |
| [`claude-desktop.md`](./claude-desktop.md) | Claude Desktop (`.mcpb`, or manual stdio) | **Yes**, for the `.mcpb` route (shipping since 0.8.4) |
| [`cursor.md`](./cursor.md) | Cursor | No |
| [`codex.md`](./codex.md) | OpenAI Codex CLI | No |
| [`obsidian.md`](./obsidian.md) | Obsidian | Not applicable — Obsidian hosts no MCP server; the page says what does work |
| [`chain-with-paperqa.md`](./chain-with-paperqa.md) | Composition with paper-qa | No, as a chain |

## The one setting to get right

`DOIGET_STORE_ROOT`, as an absolute path.

The store root defaults to `./papers` **relative to the process's working
directory** (ADR-0036). For a CLI run that is exactly right — artifacts land
where you are working. For an MCP server it is not: the working directory
belongs to the host, so the store lands somewhere you did not choose and a
later CLI run does not see it. That is
[#369](https://github.com/QAtlasHub/doiget/issues/369).

`DOIGET_CONTACT_EMAIL` is the second one. Without it every request goes out as
`doiget@localhost` on the non-polite pool, where a throttled answer is
indistinguishable from "this paper has no OA copy"
([#504](https://github.com/QAtlasHub/doiget/issues/504)).

Everything else is off by default and documented in
[`../CAPABILITY.md`](../CAPABILITY.md).

## Also worth reading

- **Tool surface:** [`../MCP_TOOLS.md`](../MCP_TOOLS.md) — every tool with its
  JSON Schema. `doiget_capability_profile` reports the same thing at runtime
  for the build you actually have.
- **Configuration:** [`../CONFIG.md`](../CONFIG.md) — precedence and the
  `config.toml` schema.
- **Errors:** [`../ERRORS.md`](../ERRORS.md) — the closed set of error codes
  and what an agent should do with each.
- **Rust API:** [`../PUBLIC_API.md`](../PUBLIC_API.md) — for embedders linking
  `doiget-core` directly instead of going through MCP.

## Contributing a snippet

A working configuration for a host not listed here is welcome. Open a
[GitHub Discussion](https://github.com/QAtlasHub/doiget/discussions) with the
host, the config file and its path, and anything host-specific that bit you —
or a PR adding a page in the shape of the others, including an honest
"exercised?" line.
