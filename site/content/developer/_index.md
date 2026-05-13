+++
title = "Developer"
description = "Wire doiget into an agent host or embed doiget-core in a Rust program."
sort_by = "weight"
weight = 20
+++

This section is for people who want to **embed doiget** into their own
agent host (Claude Desktop, Cursor, Codex CLI, ...) or use the
`doiget-core` Rust crate directly. If you just want to fetch papers from
the command line, see [Use]({{ get_url(path='@/use/_index.md') | safe }}) instead.

## Two integration surfaces

### 1. MCP server (stdio JSON-RPC)

`doiget serve` is a Model Context Protocol server speaking JSON-RPC over
stdio. It exposes ten tools (Phase 3 baseline) named with the `doiget_`
prefix:

- `doiget_health` &mdash; operational sanity (version, schema_version, store_writable).
- `doiget_capability_profile` &mdash; which sources this instance can use.
- `doiget_metadata_only` &mdash; resolve to metadata without fetching the PDF.
- `doiget_fetch_paper` &mdash; download a single PDF.
- `doiget_batch_fetch` &mdash; up to 100 refs in one call.
- `doiget_resolve_paper` &mdash; one-shot resolution, no persistence (Phase 3).
- `doiget_info` &mdash; read a store entry's metadata (Phase 3).
- `doiget_search_local` &mdash; search store metadata (Phase 3).
- `doiget_list_recent` &mdash; last N fetched entries (Phase 3).
- `doiget_paper_pdf_path` &mdash; absolute path of a cached PDF (Phase 3).

The MCP transport is **stdio only** (ADR-0001). No HTTP, no WebSocket,
no socket. This is a permanent posture decision.

### 2. Rust library (`doiget-core`)

Embed the resolver directly via the `doiget-core` crate. The library
surface is semver-strict from `0.1.0` onward (workspace stays at
`0.0.0` until Phase 6).

```rust
use doiget_core::{Ref, CapabilityProfile, orchestrator};

let ref_ = Ref::parse("10.1103/PhysRevLett.130.200601")?;
let profile = CapabilityProfile::from_env()?;
let outcome = orchestrator::fetch_paper(&ref_, &profile, &ctx).await?;
println!("wrote: {}", outcome.path);
```

See [docs.rs/doiget-core]({{ config.extra.docs_rs_url }}) for the full
rustdoc once Phase 6 ships.

## Host integration snippets

Detailed JSON-RPC traces and host-side config for the major MCP hosts
land in Phase 3 (after `doiget serve` is feature-complete). Until then,
the canonical contract is the
[MCP tools spec]({{ config.extra.github_url }}/blob/main/docs/MCP_TOOLS.md).
