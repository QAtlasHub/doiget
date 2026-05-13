+++
title = "Architecture"
description = "Workspace layout, crate dependency rules, and core trait surface. Binding spec lives in docs/ARCHITECTURE.md."
weight = 100
+++

The canonical architecture document is
[`docs/ARCHITECTURE.md`]({{ config.extra.github_url }}/blob/main/docs/ARCHITECTURE.md)
in the repository. This page is a thin orientation — the binding
content lives there.

## One-paragraph summary

doiget is a Rust workspace of three crates (`doiget-core`,
`doiget-cli`, `doiget-mcp`) plus an optional fourth
(`doiget-obsidian`). The library crate `doiget-core` defines the
abstract `Source` and `Store` traits and provides Open Access source
implementations. A runtime `CapabilityProfile` resolved from
environment variables gates which sources are allowed for the current
invocation. CLI subcommands consume `doiget-core` directly. The MCP
server is a separate library that wraps `doiget-core` and is invoked
from `doiget-cli` via the `serve` subcommand. Every fetch passes
through a fail-closed provenance log (JSON Lines + SHA-256 hash chain)
before reaching the store.

## Crate dependency rules

Forbidden directions (CI-enforced):

- `doiget-core` → `doiget-cli` (lib must not depend on bin).
- `doiget-core` → `doiget-mcp` (lib must not depend on server).
- `doiget-mcp` → `doiget-cli` (server must not depend on CLI).

## Further reading

- [`docs/ARCHITECTURE.md`]({{ config.extra.github_url }}/blob/main/docs/ARCHITECTURE.md) &mdash; binding spec with mermaid system diagram.
- [Phase plan]({{ get_url(path='@/contribute/phases.md') | safe }}) &mdash; what work is in flight.
- [`docs/DECISIONS/`]({{ config.extra.github_url }}/tree/main/docs/DECISIONS) &mdash; the 24 ADRs that bind major design choices.
