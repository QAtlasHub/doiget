# 0006 - Provenance log is JSON Lines + SHA-256 hash chain (fail-closed)

- **Date:** 2026-05-05
- **Status:** Accepted
- **Supersedes:** -
- **Source:** Discussion #12 / #17

## Context

This is a stub ADR. The full Context, Decision, and Consequences sections will be
expanded during Phase 0 from the corresponding GitHub Discussion review threads.
The summary below is sufficient for ADR INDEX.md and for cross-references in the
NORMATIVE specs to resolve.

## Decision

Every fetch is recorded in a local JSON Lines log with a SHA-256 hash chain (prev_hash + this_hash). Log writes are fail-closed: a fetch that cannot be logged must abort. The log is local-only; doiget transmits no log data anywhere. Spec is docs/PROVENANCE_LOG.md.

## Consequences

See the source Discussion for the analyzed positives and negatives. The binding
result is captured in the Decision above and is referenced from the relevant
NORMATIVE doc(s) under docs/.

To revise this decision, write a new ADR with Status: Accepted and Supersedes: 0006,
and update this file's Status to Superseded by NNNN per CONTRIBUTING.md.
