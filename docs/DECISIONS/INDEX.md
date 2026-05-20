# Architecture Decision Records

> **Status: NORMATIVE (index).** Lists every accepted ADR. Each ADR file is itself
> NORMATIVE for its scope. Superseded ADRs are kept in place for history and marked
> `Status: Superseded by NNNN`.

ADR format follows [Michael Nygard's template](https://cognitect.com/blog/2011/11/15/documenting-architecture-decisions):
**Context · Decision · Consequences · Status**.

To revise a decision, write a new ADR that supersedes the old one.
See [`../../CONTRIBUTING.md`](../../CONTRIBUTING.md) §"ADR workflow".

## Index

Status column reconciled 2026-05-17 against `CHANGELOG.md` slices (issue #150).
"Accepted" rows note the implementing slice/PR in the ADR file's `Status:` line.

| # | Title | Status | Implemented by | Source Discussion |
|---|---|---|---|---|
| 0001 | MCP transport is stdio only | Accepted | posture-lint + Slice 9 stdout-purity | #4 |
| 0002 | TDM sources are compile-time feature-gated | Accepted | Slices 17/18/19 | #5 |
| 0003 | PDF content processing is permanently out of scope | Accepted | standing policy (SCOPE.md #1 + posture-lint) | #9 |
| 0004 | BiblioFetch.jl coexistence — shared store contract | Accepted | Phase 1 store + #121 (`0.1.2`) | #1 / #2 |
| 0005 | CapabilityProfile gates source invocation at the type level | Accepted | PR #64/#65 (Phase 1) | #16 / #17 |
| 0006 | Provenance log is JSON Lines + SHA-256 hash chain (fail-closed) | Accepted | PR #61 + Slice 4 | #12 / #17 |
| 0007 | safekey algorithm with 100 reference test vectors | Accepted | PR #39 + Slice 3 | #1 §Contract 4 / #17 |
| 0008 | Workspace is split into doiget-core / -cli / -mcp (+ -obsidian opt) | Accepted | Phase 0 workspace | #14 / #18 |
| 0009 | MVP source list is Tier 1 only (Crossref / Unpaywall / arXiv) | Accepted | PR #67/#68/#69 (Phase 1) | #3 |
| 0010 | Citation graph hard-cap (depth=3, total=100, per-paper=20) | Accepted | Slice 14 | #6 |
| 0011 | Phase plan v1 — MVP at 5 weeks, Phase 5 deferred-by-default | Accepted | executed through Phase 6 | #7 |
| 0012 | MCP tool naming + structured ok-false errors | Accepted | Slices 1/2/7/8/15 | #8 |
| 0013 | CI baseline (9 workflows, OIDC publish, sigstore signing) | Accepted (release portion Superseded by 0025, PR #166) | Phase 0 + Slices 9/22 + `5a108dd` | #10 |
| 0014 | Docs split into NORMATIVE / INFORMATIVE with ADR change-control | Accepted | Phase 0 (in force) | #11 |
| 0015 | No telemetry / phone-home / self-update | Accepted | standing policy (SCOPE.md #10/#11 + posture-lint) | #12 |
| 0016 | Common foundation crates + deny list | Accepted | Phase 0 (deny.toml) | #13 |
| 0017 | Output mode resolution (flag > env > implicit > TTY > quiet) | Accepted | Slice 9 stdout-purity + Slice 1 (see ADR note) | #14 |
| 0018 | Obsidian export is one-direction, optional Phase 7 | Proposed (Phase 7, unshipped) | — | #15 |
| 0019 | Eight-safeguard legal posture (5 social + 3 technical) | Accepted | standing posture (Phase 1 onward) | #16 |
| 0020 | reqwest TLS feature stack (rustls-only; ring provider) | Accepted (Amendment 1 2026-05-18: aws-lc-rs → ring, portability) | PR #30 / PR #49 / Amendment 1 | PR #30 / PR #49 |
| 0021 | Canonical-tuple identity for fetched papers (spec-only) | Superseded by 0024 (impl); §1–§4 NORMATIVE remains binding | Slice 4 (via 0024) | #12 |
| 0022 | Dry-run mode for fetch operations | Accepted | Slice 2 | #12 |
| 0023 | Structured `denial_context` on error envelopes | Accepted | Slices 1/2 | #12 |
| 0024 | CanonicalRef implementation + provenance log v1 → v2 migration | Accepted | Slice 4 | Slice 4 |
| 0025 | Tag-driven release with version gate + beta/stable lanes | Accepted (Amend. 5 2026-05-19: D6 `next`-primary; Amend. 6 2026-05-19: advisory `version-check` job, D1 unchanged) | PR #166 / Amend. 5 / Amend. 6 | maintainer review 2026-05-17 |
| 0026 | DOI suffix charset extension: permit `:` (SECURITY.md §1.1) | Accepted | #194 | #194 dogfood |
| 0027 | redirect-allowlist: add physics-society / diamond-OA hosts to `oa-publisher` | Accepted | #193 | #193 dogfood |

## Conventions

- Filename: `NNNN-<short-slug>.md` (e.g. `0001-stdio-only.md`).
- All ADRs reference relevant NORMATIVE docs (`../LEGAL.md`, etc.).
- An ADR is created **before** any code change that locks the decision.
- **An ADR flips from `Proposed` to `Accepted` when its decision is merged**
  (the implementing slice/PR is noted in the ADR's `Status:` line and in the
  "Implemented by" column above). An ADR whose decision is not yet realized
  stays `Proposed`.
- Once accepted, an ADR is never edited in place. To change a decision, write a new
  ADR with `Supersedes: NNNN` and update the old ADR's `Status:` to
  `Superseded by NNNN`.

## Why ADRs

If [GitHub Discussions](https://github.com/sotashimozono/doiget/discussions) is ever
deleted (the maintainer's stated worst-case for this repo), the ADRs preserve the
binding decisions in the source tree itself. The Discussions link is for historical
context only.
