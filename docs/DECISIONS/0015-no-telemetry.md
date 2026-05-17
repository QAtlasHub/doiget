# 0015 - No telemetry / phone-home / self-update

- **Date:** 2026-05-05
- **Status:** Accepted — standing policy, enforced; `docs/SCOPE.md` non-goals #10/#11 + `.github/workflows/posture-lint.yml` (telemetry/self-update crate + endpoint guard); no phone-home path in any shipped slice
- **Supersedes:** -
- **Source:** Discussion #12

## Context

This is a stub ADR. The full Context, Decision, and Consequences sections will be
expanded during Phase 0 from the corresponding GitHub Discussion review threads.
The summary below is sufficient for ADR INDEX.md and for cross-references in the
NORMATIVE specs to resolve.

## Decision

doiget makes no network connection that is not the result of a user-initiated fetch. There is no auto-update path, version check, crash report transmission, or usage analytics. This is enforced via cargo-deny denials (deny.toml) of relevant crates and by a posture-lint.yml grep over imports.

## Consequences

See the source Discussion for the analyzed positives and negatives. The binding
result is captured in the Decision above and is referenced from the relevant
NORMATIVE doc(s) under docs/.

To revise this decision, write a new ADR with Status: Accepted and Supersedes: 0015,
and update this file's Status to Superseded by NNNN per CONTRIBUTING.md.
