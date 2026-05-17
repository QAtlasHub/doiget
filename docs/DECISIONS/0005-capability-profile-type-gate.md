# 0005 - CapabilityProfile gates source invocation at the type level

- **Date:** 2026-05-05
- **Status:** Accepted — implemented; `Source` trait + `CapabilityProfile::from_env` (PR #64/#65, Phase 1); Tier 2 / TDM sources gate on `profile.*` flags (Slices 11–19)
- **Supersedes:** -
- **Source:** Discussion #16 / #17

## Context

This is a stub ADR. The full Context, Decision, and Consequences sections will be
expanded during Phase 0 from the corresponding GitHub Discussion review threads.
The summary below is sufficient for ADR INDEX.md and for cross-references in the
NORMATIVE specs to resolve.

## Decision

Every `Source::fetch` implementation must accept a `&CapabilityProfile` parameter.
Sources whose capability is not granted at startup cannot be invoked at the type
level. Resolution from environment variables (`CapabilityProfile::from_env`)
hard-fails on `(agreed=1, no key)` and `(key set, no agreement)` per
`docs/CAPABILITY.md` §2.

**Phase 0 status:** the type-level gate (`Source` trait signature, `CapabilityProfile`
shape with `#[non_exhaustive]`, `RateLimits::HARD_CODED` discipline) is already in
place in `lib.rs`. The env-resolution algorithm itself is a Phase 1 deliverable:
Phase 0 ships a stub `from_env()` that returns a Tier-1-only profile and emits a
`tracing::warn!` breadcrumb if any `DOIGET_AGREE_TDM_*` / `DOIGET_KEY_*` env var is
detected. Phase 1 must replace the stub with the full algorithm and exercise both
`CapabilityError` variants in tests.

## Consequences

See the source Discussion for the analyzed positives and negatives. The binding
result is captured in the Decision above and is referenced from the relevant
NORMATIVE doc(s) under docs/.

To revise this decision, write a new ADR with Status: Accepted and Supersedes: 0005,
and update this file's Status to Superseded by NNNN per CONTRIBUTING.md.
