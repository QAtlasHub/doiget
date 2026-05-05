# 0002 - TDM sources are compile-time feature-gated

- **Date:** 2026-05-05
- **Status:** Accepted
- **Supersedes:** -
- **Source:** Discussion #5

## Context

This is a stub ADR. The full Context, Decision, and Consequences sections will be
expanded during Phase 0 from the corresponding GitHub Discussion review threads.
The summary below is sufficient for ADR INDEX.md and for cross-references in the
NORMATIVE specs to resolve.

## Decision

Each TDM source (Elsevier / APS / Springer) is behind its own Cargo feature (tdm-elsevier / tdm-aps / tdm-springer). The default published binary contains no TDM source code at all. Users wishing to enable TDM access must rebuild from source with --features tdm-<publisher>. There is no umbrella tdm-all feature.

## Consequences

See the source Discussion for the analyzed positives and negatives. The binding
result is captured in the Decision above and is referenced from the relevant
NORMATIVE doc(s) under docs/.

To revise this decision, write a new ADR with Status: Accepted and Supersedes: 0002,
and update this file's Status to Superseded by NNNN per CONTRIBUTING.md.
