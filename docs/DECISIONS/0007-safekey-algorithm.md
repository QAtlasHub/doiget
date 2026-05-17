# 0007 - safekey algorithm with reference test vectors

- **Date:** 2026-05-05
- **Status:** Accepted — implemented; `Ref::safekey()` (PR #39) + 100-vector NORMATIVE parity set (Slice 3) gated by `.github/workflows/safekey-vectors.yml`
- **Supersedes:** -
- **Source:** Discussion #1 §Contract 4 / #17

## Context

This is a stub ADR. The full Context, Decision, and Consequences sections will be
expanded during Phase 0 from the corresponding GitHub Discussion review threads.
The summary below is sufficient for ADR INDEX.md and for cross-references in the
NORMATIVE specs to resolve.

## Decision

docs/SAFEKEY.md §3 specifies the deterministic algorithm. Both Rust and Julia implementations must produce bit-identical output for every entry in tests/fixtures/safekey/vectors.json. The full set of 100 vectors is generated jointly with BiblioFetch.jl.

## Consequences

See the source Discussion for the analyzed positives and negatives. The binding
result is captured in the Decision above and is referenced from the relevant
NORMATIVE doc(s) under docs/.

To revise this decision, write a new ADR with Status: Accepted and Supersedes: 0007,
and update this file's Status to Superseded by NNNN per CONTRIBUTING.md.
