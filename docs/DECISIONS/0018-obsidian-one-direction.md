# 0018 - Obsidian export is one-direction, optional Phase 7

- **Date:** 2026-05-05
- **Status:** Accepted
- **Supersedes:** -
- **Source:** Discussion #15

## Context

This is a stub ADR. The full Context, Decision, and Consequences sections will be
expanded during Phase 0 from the corresponding GitHub Discussion review threads.
The summary below is sufficient for ADR INDEX.md and for cross-references in the
NORMATIVE specs to resolve.

## Decision

doiget-obsidian is a separate, default-OFF crate. The store-to-vault sync is one-direction (store is the source of truth); vault edits are not propagated back. Vault path must be supplied explicitly (no auto-discovery). Conflict resolution touches only frontmatter; user body content is never overwritten.

## Consequences

See the source Discussion for the analyzed positives and negatives. The binding
result is captured in the Decision above and is referenced from the relevant
NORMATIVE doc(s) under docs/.

To revise this decision, write a new ADR with Status: Accepted and Supersedes: 0018,
and update this file's Status to Superseded by NNNN per CONTRIBUTING.md.
