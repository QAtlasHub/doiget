# doiget

> A fast, single-binary CLI + MCP server that turns DOIs and arXiv ids into local PDFs.
> Designed to be the **agent-facing companion** to [BiblioFetch.jl](https://github.com/sotashimozono/BiblioFetch.jl).

**Status: Design phase.** Architecture, scope, and tech choices are being decided in [GitHub Discussions](https://github.com/sotashimozono/doiget/discussions).

## Why

- BiblioFetch.jl is great for Julia researchers but has a high distribution barrier for LLM agent users
- doiget targets the same store layout (`~/papers/` + TOML metadata) but is:
  - distributed as a single static binary
  - first-class on Model Context Protocol (MCP)
  - <100ms cold start
  - tokio-based async fan-out for batch fetches

## Coexistence

doiget and BiblioFetch.jl will share the same on-disk store format. Use whichever fits your workflow:

- BiblioFetch.jl — Julia REPL, research vault, citation graph
- doiget — agent / MCP, batch operations, container deployments

License: MIT
