+++
title = "Use"
description = "Fetch papers by DOI or arXiv id from the command line."
sort_by = "weight"
weight = 10
+++

This section is for people who want to **use the `doiget` CLI directly**
to fetch papers locally. If you want to wire doiget into an agent host or
embed it in a Rust program, see the [Developer]({{ get_url(path='@/developer/_index.md') | safe }}) section.
If you want to contribute to doiget, see [Contribute]({{ get_url(path='@/contribute/_index.md') | safe }}).

## What `doiget` does

Given a DOI or arXiv id, it tries the official metadata APIs (Crossref,
Unpaywall, arXiv) and writes the resulting PDF (and a metadata TOML) to a
local store under `~/papers/`. By default it only touches Open Access
sources; institutional TDM access is opt-in at build time per publisher.

## Quick start

```sh
# install (after Phase 6 release lands)
cargo install doiget-cli

# fetch a paper by DOI
doiget fetch 10.1103/PhysRevLett.130.200601

# fetch by arXiv id
doiget fetch arXiv:2401.12345

# batch fetch
doiget batch refs.txt

# inspect what landed locally
doiget info 10.1103/PhysRevLett.130.200601
```

Default features fetch only Open Access PDFs through Crossref / Unpaywall
/ arXiv. See [posture &amp; legal]({{ get_url(path='@/use/posture-and-legal.md') | safe }})
for what doiget will and will not do.

## Coexistence with BiblioFetch.jl

doiget shares its on-disk store format (TOML metadata + PDF files under
`~/papers/`) with [BiblioFetch.jl](https://github.com/sotashimozono/BiblioFetch.jl).
If you already use BiblioFetch.jl, doiget will reuse the same vault.

| Tool | Best for |
|---|---|
| BiblioFetch.jl | Julia REPL, research vault, citation graph exploration |
| doiget | Agents / MCP hosts, batch operations, scripted pipelines, container deployments |
