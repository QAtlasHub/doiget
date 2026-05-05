# 0005 - CapabilityProfile gates source invocation at the type level

- **Date:** 2026-05-05
- **Status:** Proposed
- **Supersedes:** -
- **Source:** Discussion #16 / #17

## Context

This is a stub ADR. The full Context, Decision, and Consequences sections will be
expanded during Phase 0 from the corresponding GitHub Discussion review threads.
The summary below is sufficient for ADR INDEX.md and for cross-references in the
NORMATIVE specs to resolve.

## Decision

Every Source::fetch implementation must accept a &CapabilityProfile parameter. Sources whose capability is not granted at startup cannot be invoked at the type level. Resolution from environment variables (CapabilityProfile::from_env) hard-fails on (agreed=1, no key) and (key set, no agreement) per docs/CAPABILITY.md §2.

## Consequences

See the source Discussion for the analyzed positives and negatives. The binding
result is captured in the Decision above and is referenced from the relevant
NORMATIVE doc(s) under docs/.

To revise this decision, write a new ADR with Status: Accepted and Supersedes: 0005,
and update this file's Status to Superseded by NNNN per CONTRIBUTING.md.
