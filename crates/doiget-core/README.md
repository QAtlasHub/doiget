# doiget-core

Foundation library for [doiget](https://github.com/QAtlasHub/doiget): an
Open Access first paper-fetcher with strict capability gating, fail-closed
provenance logging, and a [BiblioFetch.jl](https://github.com/sotashimozono/BiblioFetch.jl)-compatible
store layout. `doiget-core` defines the semver-locked types (`Ref`, `Doi`,
`ArxivId`, `Safekey`, `ErrorCode`), the hard-coded constants (rate caps, size
caps, schema version), and the `CapabilityProfile` resolver that gates which
sources may be invoked at runtime. It performs no I/O, owns no async runtime,
and bundles no HTTP client of its own — those concerns live in the binary
crates that depend on it.

## Status

**Phase 0 skeleton.** The crate currently ships only the type surface and the
hard-coded constants from [`docs/LEGAL.md`](https://github.com/QAtlasHub/doiget/blob/main/docs/LEGAL.md)
§6 safeguard 8 and [`docs/SECURITY.md`](https://github.com/QAtlasHub/doiget/blob/main/docs/SECURITY.md)
§1. Real `Source::fetch` implementations, the resolver, the store writer, and
the provenance log all land in Phase 1; see
[`docs/PHASES.md`](https://github.com/QAtlasHub/doiget/blob/main/docs/PHASES.md)
for the rollout schedule.

The public Rust API surface is **semver-locked** per
[`docs/PUBLIC_API.md`](https://github.com/QAtlasHub/doiget/blob/main/docs/PUBLIC_API.md):
breaking changes to any item listed there require a major bump and an ADR.
Phase 0 may freely split `lib.rs` into submodules without a major bump — the
guarantee is on the public identifier set, not the file layout.

## Where this sits in the workspace

`doiget-core` is the foundation library of a three-crate workspace:

| Crate | Role |
|---|---|
| **`doiget-core`** | Types, constants, `CapabilityProfile`, `Source` / `Store` traits |
| `doiget-cli`      | Single-binary CLI (`doiget fetch`, `doiget batch`, `doiget serve`, ...) |
| `doiget-mcp`      | Stdio JSON-RPC MCP server, 9 tools, wraps `doiget-core` |

Both `doiget-cli` and `doiget-mcp` depend on this crate. End users almost
always want the binary crate `doiget`; depend on `doiget-core` directly only
if you are building a Rust integration that consumes the type surface.

See [`docs/ARCHITECTURE.md`](https://github.com/QAtlasHub/doiget/blob/main/docs/ARCHITECTURE.md)
for the full system diagram.

## Binding specs

The following documents are **NORMATIVE** — implementation in this crate must
match them exactly, and changes flow through ADR review:

- [`docs/PUBLIC_API.md`](https://github.com/QAtlasHub/doiget/blob/main/docs/PUBLIC_API.md)
  — semver-locked public Rust API surface
- [`docs/CAPABILITY.md`](https://github.com/QAtlasHub/doiget/blob/main/docs/CAPABILITY.md)
  — `CapabilityProfile` env-var resolution rules and tier gating
- [`docs/SAFEKEY.md`](https://github.com/QAtlasHub/doiget/blob/main/docs/SAFEKEY.md)
  — deterministic filesystem-safe key algorithm and reference test vectors

Additional informative context lives in
[`docs/ARCHITECTURE.md`](https://github.com/QAtlasHub/doiget/blob/main/docs/ARCHITECTURE.md),
[`docs/SECURITY.md`](https://github.com/QAtlasHub/doiget/blob/main/docs/SECURITY.md),
and [`docs/SCOPE.md`](https://github.com/QAtlasHub/doiget/blob/main/docs/SCOPE.md).

## License

MIT (per the workspace `Cargo.toml` `license` field). See the workspace
[`LICENSE`](https://github.com/QAtlasHub/doiget/blob/main/LICENSE) file.
The license under which doiget retrieves papers is **separate** and is
determined by each paper's own license, the publisher's API Terms of Service,
and the user's own access rights — `doiget-core` does not relicense fetched
content.
