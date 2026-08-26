# Claude Code integration

> **Status: VERIFIED.** The `claude mcp add` form below is the configuration
> this project is developed under. Last checked 2026-08-26 against
> `doiget 0.8.10`.

[Claude Code](https://www.anthropic.com/claude-code) hosts MCP servers over
stdio. `doiget serve` is the entry point — the same one
[`mcpb/manifest.json`](../../mcpb/manifest.json) and the
[MCP Registry](https://registry.modelcontextprotocol.io/) use.

Install `doiget` first (see [`../../README.md`](../../README.md) §Installation);
it must be on `PATH`, or give an absolute path below.

## One line

```sh
claude mcp add doiget -- doiget serve
```

With the environment set at the same time, and available in every project
rather than only this one:

```sh
claude mcp add doiget --scope user \
  -e DOIGET_STORE_ROOT="$HOME/papers" \
  -e DOIGET_CONTACT_EMAIL="you@institution.edu" \
  -- doiget serve
```

`--scope` is `local` (this project, private) by default; `user` is every
project, `project` writes the committed `.mcp.json` below. `claude mcp add
--help` lists the rest.

## Project-scoped `.mcp.json`

Committed at the repository root, this shares the server with everyone who
opens the project:

```json
{
  "mcpServers": {
    "doiget": {
      "type": "stdio",
      "command": "doiget",
      "args": ["serve"],
      "env": {
        "DOIGET_STORE_ROOT": "${HOME}/papers",
        "DOIGET_CONTACT_EMAIL": "you@institution.edu"
      }
    }
  }
}
```

On Windows, `command` is the full path if `doiget` is not on `PATH`, e.g.
`"C:/Users/<you>/AppData/Local/Programs/doiget/doiget"`.

## Environment

`DOIGET_STORE_ROOT` is the one to set. Without it the store root defaults to
`./papers` **relative to the process's working directory** (ADR-0036), which
for an MCP server is the host's, not yours — #369 is that mistake. Set it to
an absolute path and the store is the same one `doiget` on the command line
uses.

| Variable | Why |
|---|---|
| `DOIGET_STORE_ROOT` | Absolute path to the paper store. Set this. |
| `DOIGET_CONTACT_EMAIL` | Polite-pool contact for Crossref / Unpaywall. Without it requests go out as `doiget@localhost` from the non-polite pool, where a throttled answer is indistinguishable from "no OA copy" (#504). |
| `DOIGET_UNPAYWALL_EMAIL` | Only if it differs from the contact address. |
| `DOIGET_ENABLE_OPENALEX`, `DOIGET_ENABLE_EUROPE_PMC`, `DOIGET_ENABLE_CORE`, `DOIGET_ENABLE_OPENAIRE`, `DOIGET_ENABLE_HAL`, `DOIGET_ENABLE_DATACITE`, `DOIGET_ENABLE_S2`, `DOIGET_ENABLE_DOAJ` | Widen the search past the three always-on sources. **All off by default** (ADR-0040) — with none of them set, `no OA PDF available` means only that Crossref, Unpaywall and arXiv had nothing. |
| `DOIGET_CORE_API_KEY` | Optional free CORE key; raises that source's rate limit. |

Full precedence and the complete list: [`../CONFIG.md`](../CONFIG.md) and
[`../CAPABILITY.md`](../CAPABILITY.md).

**A build caveat, once.** Those `DOIGET_ENABLE_*` flags need the `metadata`
feature compiled in. The official release binaries and the `.mcpb` are built
`--features oa-only,citation` (which implies `metadata`), so they work there.
A binary you built yourself with the default `oa-only` will log a warning and
leave the source unavailable — `doiget_capability_profile` reports the truth
for the binary you actually have.

## Checking it worked

```
/mcp
```

should list `doiget`. Then ask the model to call `doiget_health` — it reports
the version, whether the store is writable, and where the store actually is,
which is the fastest way to catch a wrong `DOIGET_STORE_ROOT`.

## Tool surface

22 tools, enumerated with their JSON Schemas in
[`../MCP_TOOLS.md`](../MCP_TOOLS.md). `doiget_capability_profile` reports which
sources this particular build and environment may use.
