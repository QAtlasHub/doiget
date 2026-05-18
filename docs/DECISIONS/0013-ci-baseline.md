# 0013 - CI baseline (workflows, OIDC publish, sigstore signing)

- **Date:** 2026-05-05
- **Status:** Accepted — implemented; Phase-0 9-workflow CI baseline + Slice 9 real MCP smoke gate + OIDC publish (Slice 22) + release-sign/SBOM workflow (commit `5a108dd`, v0.1.3); release portion Superseded by 0025 (PR #166)
- **Supersedes:** -
- **Superseded by:** 0025 (release portion) — the tag-driven release pipeline replaces the release-plz release-PR model + per-crate tag scheme. The CI-baseline / posture-lint / SHA-pin / Dependabot decisions of 0013 remain binding.
- **Source:** Discussion #10

## Context

This is a stub ADR. The full Context, Decision, and Consequences sections will be
expanded during Phase 0 from the corresponding GitHub Discussion review threads.
The summary below is sufficient for ADR INDEX.md and for cross-references in the
NORMATIVE specs to resolve.

## Decision

Phase 0 ships ci.yml + audit.yml + posture-lint.yml + codeql.yml. Release adds OIDC trusted publishing to crates.io, sigstore keyless binary signing, environment-protected release workflow, and SBOM generation. All third-party Actions are pinned by SHA, not floating tag. Dependabot creates PRs but does not auto-merge.

## Consequences

See the source Discussion for the analyzed positives and negatives. The binding
result is captured in the Decision above and is referenced from the relevant
NORMATIVE doc(s) under docs/.

To revise this decision, write a new ADR with Status: Accepted and Supersedes: 0013,
and update this file's Status to Superseded by NNNN per CONTRIBUTING.md.
