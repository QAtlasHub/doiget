# 0021 - Canonical-tuple identity for fetched papers

- **Date:** 2026-05-12
- **Status:** Proposed (spec-only; implementation deferred to Phase 2+)
- **Supersedes:** -
- **Source:** Discussion #12 (musaabhasan, 2026-05-08)

## Context

A raw DOI string is treated today as the sole identity of a paper inside doiget:
[`Ref::Doi(Doi(String))`](../PUBLIC_API.md) is the only identifier carried into
`safekey`, the metadata store, and the provenance log. The same is true for
`Ref::Arxiv`.

The external review on Discussion #12 (musaabhasan, 2026-05-08, comment 1)
points out a gap in this model:

> Normalize the user-supplied identifier into a canonical tuple
> `(source_type, source_id, resolver_profile, optional version)` — i.e. don't
> trust raw DOI strings as their own identity. […] That separates identifier
> trust from network authority. A syntactically valid DOI should not
> automatically grant permission to follow arbitrary redirects, write arbitrary
> paths, or log sensitive resolver credentials.

Concretely, two distinct fetch attempts can resolve the same DOI through
different resolver paths (e.g. Crossref → publisher OA URL vs. Unpaywall →
publisher OA URL vs. arXiv), and today the audit trail records the same
identity for both. That collapses two distinct provenance-relevant facts into
one, and it prevents an LLM agent from asking "did you previously fetch
`10.1234/foo` *via Unpaywall*?" without retrieving and inspecting every
provenance row.

The Phase 1 implementation works around this by writing the resolver path into
the `[doiget].source` field of the metadata TOML and into the `source` column
of every provenance row. That is sufficient for the Phase 1 surface (Tier 1
sources are disjoint by ref-class — Crossref/Unpaywall serve DOIs, arXiv serves
arXiv IDs), but the gap re-emerges as soon as Phase 2 introduces overlapping
resolvers (OpenAlex / Semantic Scholar / DOAJ all resolve DOIs) and Phase 5
introduces TDM resolvers that compete with OA resolvers for the same DOI.

## Decision

### 1. Canonical-tuple shape (NORMATIVE)

The audit identity of a fetched paper is the tuple

```rust
struct CanonicalRef {
    source_type:      SourceType,        // Doi | Arxiv | (future: Pmid, Handle, ...)
    source_id:        String,            // the validated identifier ("10.1234/foo")
    resolver_profile: String,            // e.g. "crossref", "unpaywall", "arxiv",
                                         //      "oa-publisher", "openalex", ...
    version:          Option<String>,    // e.g. arXiv "v2", Crossref-snapshot date
}
```

Every provenance row written by doiget MUST carry the canonical tuple's
SHA-256 digest in a new `canonical_digest: [u8; 32]` field. Two fetches of the
same DOI through Crossref vs. Unpaywall therefore produce two distinct
`canonical_digest` values, while two fetches of the same DOI through Crossref
on different days produce the same digest (idempotent under retries).

The digest is computed as

```text
canonical_digest := SHA256( source_type | 0x00 | source_id | 0x00
                          | resolver_profile | 0x00 | version_or_empty )
```

with `|` denoting byte concatenation, `0x00` as an unambiguous field separator,
and `version_or_empty` being either the version string or an empty string.

> **NORMATIVE.** When `version` is `None`, the `version_or_empty` byte sequence
> MUST be the empty string (zero bytes). doiget MUST NOT use any sentinel value
> (`"null"`, `"none"`, `"-"`, etc.) for an absent version — only the literal
> empty byte sequence between the preceding `0x00` separator and the SHA-256
> finalize call. This guarantees that two implementations of the canonical
> digest cannot disagree about the missing-version case.

### 2. `safekey` continues to depend on `Ref` only

The on-disk filename derivation algorithm in [`SAFEKEY.md`](../SAFEKEY.md) is
NOT changed: a `safekey` is a function of `Ref` only, never of
`CanonicalRef`. Two fetches of the same DOI through different resolvers share
one filesystem entry (one PDF, one metadata TOML), but produce two distinct
provenance rows with two distinct `canonical_digest` values.

Rationale: cross-tool round-trip with BiblioFetch.jl is keyed on `safekey`
(see [STORE.md](../STORE.md), [SAFEKEY.md](../SAFEKEY.md), ADR-0007). Splitting
the on-disk identity by `resolver_profile` would break that contract and would
duplicate PDFs on disk for no gain — the resolver-distinction the external
review asks for is an *audit-trail* concern, not a *file-storage* concern.

### 3. Implementation timing

This ADR is **spec-only** as of 2026-05-12. The `CanonicalRef` type, the
`canonical_digest` provenance column, and the `Ref → CanonicalRef` promotion
points are in scope for Phase 2 (the storage / audit-rich phase that ships
`info` / `search`), and are out of scope for the
`feat/musaabhasan-feedback-incorporation` PR that introduces this ADR.

The PR that ships `CanonicalRef` MUST:

1. Bump `provenance_log` schema_version (the row shape changes — see
   [PROVENANCE_LOG.md](../PROVENANCE_LOG.md)).
2. Provide a one-shot migration that re-derives `canonical_digest` for
   pre-existing rows by treating `resolver_profile = source` and
   `version = null`, and that recomputes the SHA-256 hash chain from the new
   row payloads.
3. Extend [PROVENANCE_LOG.md](../PROVENANCE_LOG.md) §3 with the new column,
   the new migration row type, and the new replay-verification rule.
4. Extend [PUBLIC_API.md](../PUBLIC_API.md) §2 with the `CanonicalRef` type
   and `Ref::promote(profile: &str, version: Option<&str>) -> CanonicalRef`.
5. Land its own ADR (`0024-canonical-ref-impl.md` or similar) that supersedes
   the implementation-deferred posture of this one and links to the
   migration's golden vectors.

### 4. MCP / CLI surface

Phase 2+ MCP tools that expose audit data (`doiget_info`, future
`doiget_audit_log`) MUST surface `canonical_digest` and the four tuple fields
verbatim — they are part of the public, machine-parseable contract per
ADR-0012. The `doiget_fetch_paper` tool result (and its CLI equivalent) MUST
include the chosen `resolver_profile` so an agent can see, *at fetch time*,
which canonical identity was just minted.

The `doiget bib` / `doiget csl` exports do NOT carry `canonical_digest` — those
formats target external citation managers and have no field for it.

## Consequences

**Positive.**
- An agent can ask "did doiget previously fetch `10.1234/foo` via Unpaywall?"
  and get a deterministic answer without inspecting every provenance row by
  hand.
- The provenance log gains the resolver-distinction the external review on
  Discussion #12 identified as a security concern (resolver path
  separated from identifier trust) without changing the on-disk filename
  derivation BiblioFetch.jl depends on.
- `version` becomes a first-class field, making arXiv `v2` retrievals and
  versioned Crossref snapshots distinguishable in audit.

**Negative.**
- Phase 2's provenance log carries one extra column (a 32-byte digest plus the
  four source fields). On-disk size impact: ~80 bytes/row.
- The Phase 2 PR ships a hash-chain re-computation migration. The migration is
  idempotent and `--dry-run`-able, but it touches every existing provenance
  log file and is therefore reviewer-load-bearing.
- The orchestrator must thread `resolver_profile` and `version` through every
  fetch path — a small refactor in `doiget-cli::commands::fetch` and the
  Phase 4 batch path.

**Out of scope.**
- Splitting on-disk storage by `resolver_profile` (rejected; see §2 above).
- Changing `safekey` (rejected; ADR-0007 unchanged).
- Implementing the migration in this PR (deferred to Phase 2 per §3).

To revise this decision, write a new ADR with Status: Accepted and Supersedes:
0021, and update this file's Status to Superseded by NNNN per CONTRIBUTING.md.
