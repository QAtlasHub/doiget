# doiget

`doiget` is a single-binary, stdio CLI that fetches academic papers via official Open
Access APIs (Crossref, Unpaywall, arXiv by default). It is the agent-facing companion to
[BiblioFetch.jl](https://github.com/sotashimozono/BiblioFetch.jl) and shares the same
on-disk paper store, so a Julia REPL session and an MCP-driven agent can operate over the
same library without coordination.

## Install

```sh
cargo install doiget-cli                                    # Tier 1 only (default)
cargo install doiget-cli --features metadata               # +OpenAlex / Semantic Scholar / DOAJ
cargo install doiget-cli --features metadata,tdm-springer  # add Springer Nature OA TDM
```

The default build compiles **Tier 1 (Open Access)** sources only. Tier 2 metadata
enrichment and Tier 3 institutional TDM connectors are individually feature-flagged and
must be opted in at build time. There is no `tdm-all` umbrella feature by design.

See [`docs/SOURCES.md`](https://github.com/QAtlasHub/doiget/blob/main/docs/SOURCES.md)
for the full source matrix, feature names, required env vars, and Terms-of-Service links.

## Status

Shipping and fully functional. OA fetch (Crossref / Unpaywall / arXiv), the
on-disk store, BibTeX / CSL export, the citation graph, and the **stdio MCP
server** (`doiget serve`) are all implemented. The installed binary is named
`doiget`. See the
[CHANGELOG](https://github.com/QAtlasHub/doiget/blob/main/CHANGELOG.md) for
the current version and history.

## Use as an MCP server

`doiget serve` runs the stdio MCP server. Register it with any MCP host
(Claude Desktop, Cursor, …):

```json
{ "mcpServers": { "doiget": { "command": "doiget", "args": ["serve"] } } }
```

MCP Registry name: `mcp-name: io.github.QAtlasHub/doiget`

## Subcommand surface

| Command | Purpose | Lands in |
|---|---|---|
| `doiget fetch <ref>` | Fetch a single paper PDF by DOI or arXiv id. | Phase 1 |
| `doiget batch <path>` | Fetch many refs from a newline-separated file. | Phase 1 |
| `doiget info <ref>` | Show metadata for a stored entry. | Phase 2 |
| `doiget list-recent [--limit N]` | List the most recently fetched entries. | Phase 2 |
| `doiget search <query>` | Search the local store by title / authors / venue. | Phase 2 |
| `doiget bib <ref>` | Export an entry as BibTeX. | Phase 2 |
| `doiget csl <ref>` | Export an entry as CSL JSON. | Phase 2 |
| `doiget audit-log [--verify]` | Inspect or recompute the SHA-256 provenance hash chain. | Phase 2 |
| `doiget serve` | Run as an MCP server over stdio. | Phase 3 |
| `doiget config <show\|path\|doctor>` | Show or doctor the resolved configuration. | Phase 1 |

`<ref>` is a DOI (e.g. `10.1103/PhysRevLett.130.200601`) or an arXiv id
(e.g. `arXiv:2401.12345`). All logging is written strictly to stderr; stdout is reserved
for the MCP JSON-RPC channel and structured tool output.

## Configuration

doiget reads its config from a TOML file resolved at runtime; secrets live in a separate
credentials file. Network politeness defaults (5 fetches/sec global cap, per-source
backoff, `User-Agent` with maintainer contact) are enforced by the binary, not by config.

- Config schema, search paths, and `doiget config doctor` semantics:
  [`docs/CONFIG.md`](https://github.com/QAtlasHub/doiget/blob/main/docs/CONFIG.md).
- Per-source feature flags, env vars, and ToS pointers:
  [`docs/SOURCES.md`](https://github.com/QAtlasHub/doiget/blob/main/docs/SOURCES.md).

## Open Access by default

doiget only attempts retrieval through (1) public Open Access sources and (2) credentials
the user has personally configured for their own institutional or personal access. Tier 2
metadata sources and Tier 3 publisher TDM endpoints are compile-time gated and additionally
require the user to set a `DOIGET_ENABLE_*` or `DOIGET_AGREE_TDM_*` env var at runtime —
opting in to a Tier 3 source is therefore a three-key gesture (Cargo feature + API key +
agreement env). doiget does not bundle any publisher API keys, does not redistribute
fetched content, and does not work around access controls. Users are responsible for
ensuring they have the right to access content via each source and for compliance with
each source's Terms of Service.

## License

MIT. The license under which doiget *retrieves* papers is separate and is determined by
each paper's own license, the publisher's API Terms of Service, and the user's own access
rights. doiget does not relicense fetched content. See
[`LICENSE`](https://github.com/QAtlasHub/doiget/blob/main/LICENSE) and
[`docs/LEGAL.md`](https://github.com/QAtlasHub/doiget/blob/main/docs/LEGAL.md).
