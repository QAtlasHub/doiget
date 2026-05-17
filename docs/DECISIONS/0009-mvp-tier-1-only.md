# 0009 - MVP source list is Tier 1 only (Crossref / Unpaywall / arXiv)

- **Date:** 2026-05-05
- **Status:** Accepted — implemented; default `oa-only` build ships Tier 1 (Crossref / Unpaywall / arXiv) only (PR #67/#68/#69, Phase 1); Tier 2 is feature-gated and off by default (Slices 10–13)
- **Supersedes:** -
- **Source:** Discussion #3

## Context

This is a stub ADR. The full Context, Decision, and Consequences sections will be
expanded during Phase 0 from the corresponding GitHub Discussion review threads.
The summary below is sufficient for ADR INDEX.md and for cross-references in the
NORMATIVE specs to resolve.

## Decision

Phase 1 ships only Crossref + Unpaywall + arXiv. Tier 2 metadata sources (OpenAlex / S2 / DOAJ) land in Phase 4 driven by user feedback, not parity with BiblioFetch.jl. Tier 3 TDM sources are Phase 5 and gated as in ADR-0002.

## Consequences

See the source Discussion for the analyzed positives and negatives. The binding
result is captured in the Decision above and is referenced from the relevant
NORMATIVE doc(s) under docs/.

To revise this decision, write a new ADR with Status: Accepted and Supersedes: 0009,
and update this file's Status to Superseded by NNNN per CONTRIBUTING.md.
