# Project status & remaining scope

> **Status: INFORMATIVE.** This file once held the full 0–7 phase
> implementation plan. That plan has been executed — doiget is shipped
> (**v0.2.0** on crates.io). The historical phase/slice plan and its
> deliverable checklists are preserved in git history, [`CHANGELOG.md`](../CHANGELOG.md),
> and the ADRs ([`docs/DECISIONS/`](DECISIONS/)); they are not reproduced
> here. This document describes only the **current state** and what is
> **not yet built**.

## Shipped

doiget is fully implemented and released:

- **Sources.** Tier 1 (Crossref / Unpaywall / arXiv) and Tier 2 (OpenAlex /
  Semantic Scholar / DOAJ); feature-gated Tier 3 TDM (Springer Nature OA /
  APS Harvest / Elsevier ScienceDirect), off by default.
- **CLI.** `fetch`, `batch`, `info`, `search`, `bib`, `csl`, `audit-log`,
  `provenance`, `graph`, `config`, plus the `serve` stdio MCP server.
- **MCP.** stdio-only server with the tool set specified in
  [`MCP_TOOLS.md`](MCP_TOOLS.md); citation-graph expansion (ADR-0010 caps).
- **Integrity & posture.** Hash-chained provenance log, type-gated
  capability profile, the safekey store contract (BiblioFetch.jl
  round-trip), and the documented security/legal posture.
- **Release engineering.** The tag-driven pipeline
  ([ADR-0025](DECISIONS/0025-tag-driven-release.md)): one signed workspace
  tag runs a mandatory version gate, publishes all three crates to
  crates.io via OIDC, sigstore-signs the binaries, emits an SBOM, and opens
  the GitHub Release.

The current release is **v0.2.0** (`doiget-core`, `doiget-cli`,
`doiget-mcp`). Per-version detail lives in [`CHANGELOG.md`](../CHANGELOG.md);
binding design decisions live in the ADRs.

## Remaining (planned-but-unbuilt)

One scope item is intentionally deferred and **not implemented**:

- **Optional `vault` / `obsidian` integration.** The `doiget-obsidian`
  crate is `exclude`d from the Cargo workspace; the design is one-direction,
  opt-in only, governed by
  [ADR-0008](DECISIONS/0008-crate-decomposition.md) and
  [ADR-0018](DECISIONS/0018-obsidian-one-direction.md) (Status: *Proposed —
  unshipped*). It is optional, on no committed timeline, and may never ship.

Everything else that was ever "planned" is shipped — see `CHANGELOG.md`.

## Branch & release model

Stable releases are cut from `main` (`vX.Y.Z`); betas from `next`
(`vX.Y.Z-beta.N`). The version gate, lane rules, and the maintainer runbook
are normative in [ADR-0025](DECISIONS/0025-tag-driven-release.md) and
summarized in [`CONTRIBUTING.md`](../CONTRIBUTING.md) ("Release process").
