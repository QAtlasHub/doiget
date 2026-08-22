# 0040 - Source expansion is gated by the existing `metadata` feature, runtime-gated off

- **Date:** 2026-08-22
- **Status:** Accepted
- **Supersedes:** - (settles the "open decision" in #413 that blocks its child PRs)
- **Source:** #413 (epic), #414–#418 (children)

## Context

#413 adds five optional OA sources — DataCite, Europe PMC, OpenAIRE, CORE, HAL —
under a constraint it states plainly:

> **With every flag unset, observable behaviour must be byte-identical to today.**

and asks which Cargo feature should gate them, because `metadata` currently means
*Tier 2 metadata enrichment*, while the new set includes **retrieval** sources and
DataCite is a **resolution** source closer in role to Crossref. Three options were
put:

- **(a)** reuse `metadata` — zero release-workflow change, least honest naming
- **(b)** a sibling feature, e.g. `oa-extra` — clearer, but the release build flags
  and the CI feature matrix both grow
- **(c)** DataCite as Tier 1 (always on) and the retrieval sources behind (b)

## Decision

**(a) — gate all five behind the existing `metadata` feature, each additionally
runtime-gated by its own `DOIGET_ENABLE_<NAME>`, all off by default.**

And redefine what `metadata` means, in `SOURCES.md` and `CAPABILITY.md`: *the
optional non-Tier-1 source surface — enrichment, resolution and retrieval —
compiled into release binaries and inert until a runtime flag turns a source on.*
The naming objection in (a) is answered by fixing the definition rather than by
adding a flag that carries the same code.

Three reasons.

**The Cargo feature is not the security boundary; the runtime flag is.** Both
`metadata` and a hypothetical `oa-extra` ship in the release binary — `citation`
already pulls `metadata` in, and release builds are
`--no-default-features --features oa-only,citation`. What keeps a source inert is
`CapabilityProfile::from_env` reading `DOIGET_ENABLE_<NAME>`, and that is
unchanged either way. Choosing (b) would move code between two compiled-in
buckets and change nothing observable.

**(c) breaks the epic's own load-bearing constraint.** Making DataCite Tier 1
means a DataCite-registered DOI that returns `NotFound` today would resolve — a
behaviour change for every user with no opt-in, in direct conflict with
"byte-identical to today". The motivation is real (a false `NotFound` in
`doiget-citation-check`, the most visible downstream consumer), but the fix
belongs there: that tool sets `DOIGET_ENABLE_DATACITE`. One line, in the place
that wants the behaviour.

**(b)'s cost lands where it cannot be verified before it bites.** The feature
string appears in 13 places across the workflows; two of them —
`release-plz.yml` and `release-sign.yml` — run **only on a tag push**, so a
mistake there is invisible to every PR and surfaces as a broken release. Adding
release-workflow surface is not worth a rename when the runtime gate already does
the enforcing.

## Consequences

**Positive.**

- Zero release-workflow change; the five child PRs are pure additive code + docs.
- Default behaviour is byte-identical, enforced by a default-off regression test
  per source (#413's checklist).
- The tier vocabulary in `SOURCES.md` / `CAPABILITY.md` — which is the
  user-facing one — keeps doing the explaining, instead of a Cargo feature name
  that users never type.

**Negative / accepted.**

- `metadata` becomes a broader bucket than its name suggests, mitigated by an
  explicit definition in both NORMATIVE docs rather than left implicit.
- DataCite resolution stays opt-in, so the `doiget-citation-check` false positive
  persists until that repo sets the flag.

**Revisit** if the optional surface grows past this epic, or if a source ever
needs to be excluded from release binaries — at which point the split is
motivated by something other than naming, and (b) becomes the right answer.
