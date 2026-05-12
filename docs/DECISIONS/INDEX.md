# Architecture Decision Records

> **Status: NORMATIVE (index).** Lists every accepted ADR. Each ADR file is itself
> NORMATIVE for its scope. Superseded ADRs are kept in place for history and marked
> `Status: Superseded by NNNN`.

ADR format follows [Michael Nygard's template](https://cognitect.com/blog/2011/11/15/documenting-architecture-decisions):
**Context · Decision · Consequences · Status**.

To revise a decision, write a new ADR that supersedes the old one.
See [`../../CONTRIBUTING.md`](../../CONTRIBUTING.md) §"ADR workflow".

## Index

| # | Title | Status | Source Discussion |
|---|---|---|---|
| 0001 | MCP transport is stdio only | Accepted | #4 |
| 0002 | TDM sources are compile-time feature-gated | Proposed | #5 |
| 0003 | PDF content processing is permanently out of scope | Proposed | #9 |
| 0004 | BiblioFetch.jl coexistence — shared store contract | Proposed | #1 / #2 |
| 0005 | CapabilityProfile gates source invocation at the type level | Proposed | #16 / #17 |
| 0006 | Provenance log is JSON Lines + SHA-256 hash chain (fail-closed) | Proposed | #12 / #17 |
| 0007 | safekey algorithm with 100 reference test vectors | Proposed | #1 §Contract 4 / #17 |
| 0008 | Workspace is split into doiget-core / -cli / -mcp (+ -obsidian opt) | Proposed | #14 / #18 |
| 0009 | MVP source list is Tier 1 only (Crossref / Unpaywall / arXiv) | Proposed | #3 |
| 0010 | Citation graph hard-cap (depth=3, total=100, per-paper=20) | Proposed | #6 |
| 0011 | Phase plan v1 — MVP at 5 weeks, Phase 5 deferred-by-default | Proposed | #7 |
| 0012 | MCP tool naming + structured ok-false errors | Proposed | #8 |
| 0013 | CI baseline (9 workflows, OIDC publish, sigstore signing) | Proposed | #10 |
| 0014 | Docs split into NORMATIVE / INFORMATIVE with ADR change-control | Proposed | #11 |
| 0015 | No telemetry / phone-home / self-update | Proposed | #12 |
| 0016 | Common foundation crates + deny list | Proposed | #13 |
| 0017 | Output mode resolution (flag > env > implicit > TTY > quiet) | Proposed | #14 |
| 0018 | Obsidian export is one-direction, optional Phase 7 | Proposed | #15 |
| 0019 | Eight-safeguard legal posture (5 social + 3 technical) | Proposed | #16 |
| 0020 | reqwest TLS feature stack (rustls-only, webpki bundled) | Proposed | PR #30 / PR #49 |
| 0021 | Canonical-tuple identity for fetched papers (spec-only) | Proposed | #12 |
| 0022 | Dry-run mode for fetch operations | Proposed | #12 |
| 0023 | Structured `denial_context` on error envelopes | Proposed | #12 |

## Conventions

- Filename: `NNNN-<short-slug>.md` (e.g. `0001-stdio-only.md`).
- All ADRs reference relevant NORMATIVE docs (`../LEGAL.md`, etc.).
- An ADR is created **before** any code change that locks the decision.
- Once accepted, an ADR is never edited in place. To change a decision, write a new
  ADR with `Supersedes: NNNN` and update the old ADR's `Status:` to
  `Superseded by NNNN`.

## Why ADRs

If [GitHub Discussions](https://github.com/sotashimozono/doiget/discussions) is ever
deleted (the maintainer's stated worst-case for this repo), the ADRs preserve the
binding decisions in the source tree itself. The Discussions link is for historical
context only.
