# 0012 - MCP tool naming + structured ok-false errors

- **Date:** 2026-05-05
- **Status:** Accepted — implemented; the `doiget_*` tool surface + structured `ok:false` envelopes are wired across Slices 1/2/7/8/15 (10-tool Phase-3 baseline + Phase-4 citation tool)
- **Supersedes:** -
- **Source:** Discussion #8

## Context

This is a stub ADR. The full Context, Decision, and Consequences sections will be
expanded during Phase 0 from the corresponding GitHub Discussion review threads.
The summary below is sufficient for ADR INDEX.md and for cross-references in the
NORMATIVE specs to resolve.

## Decision

All MCP tools use snake_case with the doiget_ prefix. Outputs are { ok: true, ... } or { ok: false, error: { code, message } }; tools never throw across the JSON-RPC boundary. error.code values are the closed ErrorCode enum from docs/ERRORS.md.

## Consequences

See the source Discussion for the analyzed positives and negatives. The binding
result is captured in the Decision above and is referenced from the relevant
NORMATIVE doc(s) under docs/.

To revise this decision, write a new ADR with Status: Accepted and Supersedes: 0012,
and update this file's Status to Superseded by NNNN per CONTRIBUTING.md.
