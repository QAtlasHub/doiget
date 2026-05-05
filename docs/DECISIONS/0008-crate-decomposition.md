# 0008 - Workspace is split into doiget-core / -cli / -mcp (+ -obsidian opt)

- **Date:** 2026-05-05
- **Status:** Proposed
- **Supersedes:** -
- **Source:** Discussion #14 / #18

## Context

This is a stub ADR. The full Context, Decision, and Consequences sections will be
expanded during Phase 0 from the corresponding GitHub Discussion review threads.
The summary below is sufficient for ADR INDEX.md and for cross-references in the
NORMATIVE specs to resolve.

## Decision

Three published crates: doiget-core (semver-strict library), doiget-cli (binary), doiget-mcp (MCP server library). doiget-obsidian is Phase 7 optional, default-OFF. Forbidden dependency directions (lib->bin, lib->server, server->bin) are CI-enforced. profile.release.panic = abort applies workspace-wide; if doiget-mcp is ever linked into a host process, an override is required.

## Consequences

See the source Discussion for the analyzed positives and negatives. The binding
result is captured in the Decision above and is referenced from the relevant
NORMATIVE doc(s) under docs/.

To revise this decision, write a new ADR with Status: Accepted and Supersedes: 0008,
and update this file's Status to Superseded by NNNN per CONTRIBUTING.md.
