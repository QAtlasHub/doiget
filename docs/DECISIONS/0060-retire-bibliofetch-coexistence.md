# 0060 - Retire BiblioFetch.jl coexistence: the store and safekey specs are doiget's own

- **Date:** 2026-09-29
- **Status:** Accepted
- **Supersedes:** [0004](0004-bibliofetch-coexistence.md) (BiblioFetch.jl coexistence -- shared store contract)
- **Amends:** [`docs/STORE.md`](../STORE.md) and [`docs/SAFEKEY.md`](../SAFEKEY.md) (no longer shared specs), per [0014](0014-docs-class-system.md)
- **Source:** the maintainer's decision for the 0.9.0 cycle (2026-09-29): BiblioFetch.jl is no longer developed alongside doiget ("JIT too slow, binary heavy")

## Context

ADR-0004 made the store layout and the safekey algorithm a contract binding
two implementations: doiget and the Julia package BiblioFetch.jl. Any change
to either needed "an ADR coordinated across both projects", a cross-tool
round-trip CI job was reserved (`cross-tool-compat.yml`), and the safekey
vectors were to be diffed against a Julia implementation. None of the
cross-tool checks was ever built: the workflow is still the Phase 0
placeholder that prints a message and exits 0, and the Julia parity job in
`safekey-vectors.yml` is "deferred to Phase 2". Meanwhile every doiget change
to the store -- `[doiget]` fields added in 0.9.0 alone: `repaired_fields`,
`short_venue`, `origin` -- was nominally subject to coordination with a
project that is no longer moving.

The maintainer has stopped developing BiblioFetch.jl alongside doiget.

## Decision

**D1: ADR-0004 is superseded.** doiget no longer promises a shared store
contract with BiblioFetch.jl.

**D2: STORE.md and SAFEKEY.md are doiget's own NORMATIVE specs.** They bind
doiget only, and doiget may change them by its own ADR. Their content does not
change here: the layout, `schema_version`, locking and atomic-write protocol,
and the safekey algorithm with its 100 reference vectors all stand, because
they are right for doiget on their own terms and existing stores depend on
them.

**D3: Nothing on disk changes.** A store BiblioFetch.jl wrote stays readable,
and doiget keeps preserving tool tables it does not own (`[bibliofetch]` and
any other) across a rewrite, as STORE.md §4 already requires: a store may be
written by other tools, and dropping their data is wrong regardless of who
they are. Reserved top-level fields stay reserved.

**D4: The cross-tool checks that were never built are dropped.** The
`cross-tool-compat.yml` placeholder is removed; `safekey-vectors.yml` keeps
validating the Rust implementation against the vectors, without a deferred
Julia job. STORE.md §9's planned round-trip is removed.

**D5: doiget is described as itself.** The README, the crate READMEs and the
`.mcpb` description stop calling doiget "the agent-facing companion to
BiblioFetch.jl". MIGRATION.md keeps its BiblioFetch.jl scenarios as guidance
for people who have such a store.

## Consequences

- A future change to the store or the safekey algorithm needs a doiget ADR
  and, for safekey, updated vectors -- not coordination with another project.
- ADR-0036's note that the two tools default to different roots is history.
- BiblioFetch.jl users keep working stores; the format is not being changed
  to spite them, only no longer promised to them.
