# Golden fixtures

> **Status: INFORMATIVE.** This directory holds golden test fixtures for
> deterministic byte-level regression testing against canonical outputs of
> real upstream responses. It is referenced from
> [`docs/PHASES.md`](../../../docs/PHASES.md) §2 (Test fixtures) as a Phase 0
> deliverable placeholder.

## Purpose

Use this directory to pin down the exact bytes that doiget produces for a
representative set of DOIs, so that any change to the resolver, source
adapters, or [Store](../../../docs/STORE.md) normalizer is caught as a
diff against the committed canonical output. Each fixture pairs the raw
HTTP response body from a real upstream source (the input) with the
normalized Store output that doiget MUST produce from it (the expected
result). Tests load both halves and assert byte-equality.

## Layout (Phase 1+)

You will see one subdirectory per Tier 1 source from
[ADR-0009](../../../docs/DECISIONS/0009-mvp-tier-1-only.md):

```
tests/fixtures/golden/
├── crossref/
│   └── <slug>/
│       ├── input.json        # raw Crossref REST response body
│       └── expected.toml     # canonical Store TOML per docs/STORE.md
├── unpaywall/
│   └── <slug>/
│       ├── input.json
│       └── expected.toml
└── arxiv/
    └── <slug>/
        ├── input.xml         # raw arXiv API Atom response
        └── expected.toml
```

Each `<slug>/` directory holds one fixture per representative DOI: the
verbatim upstream response body (the input) and the canonical normalized
output that doiget produces for it. Where a fixture exercises CSL JSON
export ([`docs/MCP_TOOLS.md`](../../../docs/MCP_TOOLS.md) — `doiget_csl_export`),
add an `expected.csl.json` next to `expected.toml`.

## Naming convention

Use a DOI-derived slug for `<slug>` so paths stay filesystem-safe on every
target OS. Replace every character outside `[A-Za-z0-9._-]` with `_`, the
same rule [`docs/SAFEKEY.md`](../../../docs/SAFEKEY.md) §3 applies to
storage paths:

- `10.1038/s41586-021-03819-1`  →  `10_1038_s41586-021-03819-1/`
- `10.1103/PhysRevLett.130.200601`  →  `10_1103_PhysRevLett.130.200601/`
- arXiv `cond-mat/9501001`  →  `arxiv_cond-mat_9501001/`

This intentionally mirrors safekey output (without the `doi_` / `arxiv_`
prefix, since the source subdirectory already disambiguates). Preserve
case exactly as the safekey rule preserves it — do not re-case otherwise.

## How fixtures are generated

You do not hand-write golden files. Phase 1 will add a
`cargo xtask record-golden <DOI>` workflow that fetches the DOI once
through the production code path, tees the raw response body to
`input.<ext>`, and serializes the resulting `Metadata` through the same
`doiget-core::store::normalize_toml` reference normalizer
([`docs/STORE.md`](../../../docs/STORE.md) §7) into `expected.toml`.
Re-run the same xtask with `--save-golden` to refresh a fixture when the
upstream schema legitimately changes; record that upstream change in the
commit message.

Until Phase 1 lands, this directory ships only `.gitkeep` and this
README — no fixtures yet. Do not commit hand-edited `expected.*` files.

## What gets compared

Tests assert **byte-exact** equality on `expected.*` against the freshly
normalized output. Whitespace, key ordering, and trailing newlines all
count — that is the whole point of a golden test, and it is what
[`docs/STORE.md`](../../../docs/STORE.md) §7 (TOML normalization) makes
deterministic.

The raw `input.*` body is stored unmodified — no redaction, no
re-formatting, no header rewriting. Phase 1 fixtures use only public
Open Access metadata responses, so there is nothing to redact; if a
later Phase needs fixtures from authenticated sources, address redaction
in the ADR that introduces them.

## CI

The future `cargo test --features golden` job will load every
`<source>/<slug>/` pair under this directory, replay the input through
the production normalizer, and diff the result against `expected.*`.
A failing diff blocks the PR. This is the regression gate referenced
from [`docs/PHASES.md`](../../../docs/PHASES.md) §4 Phase 1 readiness
("`cargo test --workspace` is green").

No CI job runs against this directory in Phase 0 — the fixture set is
empty, so there is nothing to diff yet.

## References

- [`docs/PHASES.md`](../../../docs/PHASES.md) §2 — this deliverable
  (Test fixtures: `tests/fixtures/golden/` placeholder) and §4 Phase 1
  readiness criteria.
- [`docs/DECISIONS/0009-mvp-tier-1-only.md`](../../../docs/DECISIONS/0009-mvp-tier-1-only.md)
  — Tier 1 source list (Crossref / Unpaywall / arXiv) that drives the
  subdirectory layout above.
- [`docs/STORE.md`](../../../docs/STORE.md) — Store TOML schema and §7
  TOML normalization (the contract `expected.toml` is byte-compared
  against).
- [`docs/PUBLIC_API.md`](../../../docs/PUBLIC_API.md) — public types
  (`Metadata`, `Source`, `Store`) that golden fixtures exercise.
