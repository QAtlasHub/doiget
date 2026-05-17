# doiget

> A single-binary CLI + stdio MCP server that turns DOIs and arXiv ids into local PDFs through official, OA-first APIs.
> Designed as the **agent-facing companion** to [BiblioFetch.jl](https://github.com/sotashimozono/BiblioFetch.jl).

**Status:** Shipping — **v0.1.3** on a live `release-plz` pipeline. Tier 1 + Tier 2
sources, the stdio MCP server, citation-graph expansion, gated TDM sources, and
OIDC/sigstore release automation are all implemented. See [CHANGELOG.md](CHANGELOG.md)
for the per-slice history and [docs/PHASES.md](docs/PHASES.md) for the phase plan.

## Posture

doiget is a general-purpose automation tool for retrieving papers via official publisher APIs.
By design, doiget only attempts retrieval through:

1. Public Open Access sources (default — Crossref, Unpaywall, arXiv).
2. Credentials the user has personally configured for their own institutional or personal subscriptions (opt-in, compile-time gated).

**doiget does not** work around any access control mechanism, redistribute papers, host
content, operate as a SaaS, or bundle any publisher API keys.

Users are responsible for ensuring they have the right to access the content they request and
for compliance with each source's Terms of Service.

See [docs/LEGAL.md](docs/LEGAL.md) and [docs/SCOPE.md](docs/SCOPE.md).

## Documentation

| Reader | Entry point |
|---|---|
| CLI user | This README, then `doiget --help`, then [docs/CONFIG.md](docs/CONFIG.md) and [docs/ERRORS.md](docs/ERRORS.md) for non-trivial flags / exit codes |
| Agent / MCP host integrator | [docs/MCP_TOOLS.md](docs/MCP_TOOLS.md) + [docs/INTEGRATION/README.md](docs/INTEGRATION/README.md) |
| Library user (Rust) | [docs/PUBLIC_API.md](docs/PUBLIC_API.md) + crates.io rustdoc |
| Contributor | [CONTRIBUTING.md](CONTRIBUTING.md) → [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) → [docs/DECISIONS/](docs/DECISIONS/) |
| Publisher legal team | [docs/LEGAL.md](docs/LEGAL.md) + [CONTACT.md](CONTACT.md) |
| Security researcher | [docs/SECURITY.md](docs/SECURITY.md) + [docs/PROVENANCE_LOG.md](docs/PROVENANCE_LOG.md) + [docs/CAPABILITY.md](docs/CAPABILITY.md) |
| BiblioFetch.jl user | [docs/MIGRATION.md](docs/MIGRATION.md) + [docs/STORE.md](docs/STORE.md) + [docs/SAFEKEY.md](docs/SAFEKEY.md) |

Architecture: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
Threat model: [docs/SECURITY.md](docs/SECURITY.md)
Permanent non-goals: [docs/SCOPE.md](docs/SCOPE.md)
Phase plan: [docs/PHASES.md](docs/PHASES.md)
ADRs: [docs/DECISIONS/](docs/DECISIONS/)

## Quick start (will be available after Phase 1)

```sh
# Install (after Phase 6 release)
cargo install doiget

# Fetch a paper by DOI (Open Access only by default)
doiget fetch 10.1103/PhysRevLett.130.200601

# Fetch by arXiv id
doiget fetch arXiv:2401.12345

# Batch fetch
doiget batch refs.txt

# Inspect what was fetched
doiget info 10.1103/PhysRevLett.130.200601

# Run as MCP server (stdio)
doiget serve
```

Default features fetch only Open Access PDFs through Crossref / Unpaywall / arXiv.
Institutional TDM access (Springer OA, APS Harvest, Elsevier ScienceDirect TDM) is **not** in
the default published binary; it must be opted in at build time per publisher.
See [docs/SOURCES.md](docs/SOURCES.md).

## Coexistence with BiblioFetch.jl

doiget and BiblioFetch.jl share the same on-disk store format (TOML metadata + PDF files
under `~/papers/`). The shared schema, locking protocol, and atomic write contract are
specified in [docs/STORE.md](docs/STORE.md). Reference test vectors for the shared safekey
algorithm are in [docs/SAFEKEY.md](docs/SAFEKEY.md).

| Tool | Best for |
|---|---|
| BiblioFetch.jl | Julia REPL, research vault, citation graph exploration |
| doiget | Agents / MCP hosts, batch operations, scripted pipelines, container deployments |

## License

MIT for the doiget source code and binaries (see [LICENSE](LICENSE)).

The license under which doiget retrieves papers is **separate** and is determined by each
paper's own license, the publisher's API Terms of Service, and the user's own access rights.
doiget does not relicense fetched content.

## Contact

Takedown requests, security disclosures, and other formal correspondence:
[CONTACT.md](CONTACT.md).
