# 0019 - Eight-safeguard legal posture (5 social + 3 technical)

- **Date:** 2026-05-05
- **Status:** Accepted — standing posture, enforced; the 5 social + 3 technical safeguards are realized across `docs/LEGAL.md`/`SCOPE.md` + posture-lint + the capability-gate / redirect-allowlist / provenance-log technical controls (Phase 1 onward)
- **Supersedes:** -
- **Source:** Discussion #16

## Context

This is a stub ADR. The full Context, Decision, and Consequences sections will be
expanded during Phase 0 from the corresponding GitHub Discussion review threads.
The summary below is sufficient for ADR INDEX.md and for cross-references in the
NORMATIVE specs to resolve.

## Decision

doiget posture is documented in docs/LEGAL.md and protected by 8 safeguards: (1) no bundled credentials, (2) opt-in TDM agreement env var, (3) user responsibility documented, (4) takedown contact with SLA, (5) marketing language self-policing in CI, (6) compile-time TDM feature gating, (7) runtime CapabilityProfile gate, (8) hard-coded rate limit. Risk planning targets the worst plausible case rather than probability estimates.

## Consequences

See the source Discussion for the analyzed positives and negatives. The binding
result is captured in the Decision above and is referenced from the relevant
NORMATIVE doc(s) under docs/.

To revise this decision, write a new ADR with Status: Accepted and Supersedes: 0019,
and update this file's Status to Superseded by NNNN per CONTRIBUTING.md.
