# Claude Desktop integration

> **Status: VERIFIED for the `.mcpb` route** (shipping since 0.8.4; attached to
> every GitHub Release). The manual JSON route below is the same stdio
> invocation and is included for people who would rather not install an
> extension. Last checked 2026-08-26.

[Claude Desktop](https://claude.ai/download) hosts MCP servers over stdio.

## Recommended: install the `.mcpb` extension

Each [GitHub Release](https://github.com/QAtlasHub/doiget/releases) attaches
`doiget-<version>.mcpb`. Download it and open it — Claude Desktop installs it
as an extension. The bundle carries the binaries for macOS, Windows and Linux
and picks the right one, so there is no PATH to configure.

It asks for one setting, **Paper store location**, which becomes
`DOIGET_STORE_ROOT`. Point it at a real directory you own; the default is
`~/Documents/doiget-papers`.

Requires Claude Desktop **0.10.0 or newer**
(`mcpb/manifest.json` → `compatibility.claude_desktop`).

## Manual: `claude_desktop_config.json`

Settings → Developer → Edit Config. The file lives at:

| OS | Path |
|---|---|
| macOS | `~/Library/Application Support/Claude/claude_desktop_config.json` |
| Windows | `%APPDATA%\Claude\claude_desktop_config.json` |
| Linux | `~/.config/Claude/claude_desktop_config.json` |

```json
{
  "mcpServers": {
    "doiget": {
      "command": "/usr/local/bin/doiget",
      "args": ["serve"],
      "env": {
        "DOIGET_STORE_ROOT": "/Users/you/papers",
        "DOIGET_CONTACT_EMAIL": "you@institution.edu"
      }
    }
  }
}
```

Use an **absolute** `command` path. A desktop app does not inherit the `PATH`
your shell has, so a bare `doiget` frequently fails to launch with no visible
error.

Restart Claude Desktop after editing.

## Environment

`DOIGET_STORE_ROOT` matters more here than anywhere else: the working
directory of a GUI-launched process is not a directory you chose, so the
`./papers` default (ADR-0036) lands somewhere arbitrary. That was #369.

The remaining variables are the same as for Claude Code — see
[`claude-code.md`](./claude-code.md) §Environment,
[`../CONFIG.md`](../CONFIG.md) and [`../CAPABILITY.md`](../CAPABILITY.md).

## Checking it worked

Ask for `doiget_health`. It reports the version, store writability and the
resolved store path.

## Verifying the download

Every release asset except the `.mcpb` currently ships `.sha256` and
`.cosign.bundle` sidecars; [#483](https://github.com/QAtlasHub/doiget/issues/483)
adds them for the `.mcpb` too. Until it lands, verify the raw binary if you
need a signature check, and install that manually per above.
