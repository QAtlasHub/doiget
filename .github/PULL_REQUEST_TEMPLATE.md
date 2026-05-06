<!--
Thanks for opening a PR against doiget. Please fill out each section
below — delete a section only if it genuinely does not apply.

Background reading:
  * CONTRIBUTING.md           — local dev loop, commit style, sign-off
  * docs/PHASES.md            — current phase, what's in/out of scope
  * docs/DECISIONS/INDEX.md   — accepted ADRs (cite them when relevant)

NORMATIVE doc edits (those marked `Status: NORMATIVE` per ADR-0014)
require a matching ADR. If your PR touches one, link it below.
-->

## Summary

<!--
1–3 bullets focused on WHY this change exists, not what the diff is.
The diff already shows what; the description should answer "why now,
why this shape, what does it unlock or fix?"
-->

-

## Phase / scope

<!--
Reference the relevant entry from docs/PHASES.md (e.g.
"Phase 0 deliverable: codeql.yml"), or "n/a (hygiene)", or "Bug fix".
-->

## Changes

<!--
High-level list of files added / modified / removed. Group by area
(crate, doc, workflow). Don't paste the diff — just the shape.
-->

## ADR references

<!--
Link any ADR(s) this PR locks-in or implements
(e.g. ADR-0007 safekey algorithm, ADR-0013 CI baseline).
If this PR introduces a new architectural decision, open the ADR
in the same PR or in a paired PR and link it here.
-->

## Test plan

- [ ] CI green on this branch
- [ ] No accidental NORMATIVE doc edits (per ADR-0014)
- [ ] If a workflow file is added/modified, all third-party Actions are SHA-pinned (ADR-0013)
- [ ] If Cargo.toml/Cargo.lock changed, dep churn reviewed
- [ ] Reviewer checklist: maintainer auto-assigned via CODEOWNERS

## Posture checks

<!--
doiget has a small, audited posture surface. Confirm both items below
or call out the exception explicitly.
-->

- [ ] No telemetry / phone-home added (ADR-0015)
- [ ] No marketing copy in code, docs, or commit messages (per `.github/workflows/posture-lint.yml`)

## Notes for the reviewer

<!--
Free-form: anything reviewers should look at first, known follow-ups,
flaky-CI caveats, or context that didn't fit above.
-->

<!--
If this PR was authored with AI assistance (Claude Code, Copilot, etc.),
please add a trailer to your commit(s) so attribution lands in `git log`:

    Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>

(Substitute the appropriate co-author identity for the assistant you used.)
-->
