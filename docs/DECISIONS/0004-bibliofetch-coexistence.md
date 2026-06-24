# 0004 - BiblioFetch.jl coexistence — shared store contract

- **Date:** 2026-05-05
- **Status:** Accepted — implemented; Store contract in `doiget-core/src/store/` (Phase 1) + BiblioFetch round-trip preservation test (CHANGELOG `0.1.2`, issue #121). **Amended by [0036](0036-default-store-cwd.md)** (2026-06-24): the default store root moved to `./papers` (under the cwd), so doiget and BiblioFetch.jl no longer co-locate *by default*; the shared on-disk *format* contract below is unchanged.
- **Supersedes:** -
- **Source:** Discussion #1 / #2

## Context

This is a stub ADR. The full Context, Decision, and Consequences sections will be
expanded during Phase 0 from the corresponding GitHub Discussion review threads.
The summary below is sufficient for ADR INDEX.md and for cross-references in the
NORMATIVE specs to resolve.

## Decision

doiget and BiblioFetch.jl share the on-disk store layout under ~/papers/. The contract is documented in docs/STORE.md and is binding for both implementations: TOML schema_version, advisory flock on a separate .toml.lock file, atomic write via tmp+fsync+rename+fsync-parent, and a shared safekey algorithm. doiget-side write discipline forbids modifying reserved top-level fields written by the other tool.

## Consequences

See the source Discussion for the analyzed positives and negatives. The binding
result is captured in the Decision above and is referenced from the relevant
NORMATIVE doc(s) under docs/.

To revise this decision, write a new ADR with Status: Accepted and Supersedes: 0004,
and update this file's Status to Superseded by NNNN per CONTRIBUTING.md.
