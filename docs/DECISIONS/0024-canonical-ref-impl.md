# 0024 - CanonicalRef implementation + provenance log v1 → v2 migration

- **Date:** 2026-05-12
- **Status:** Accepted
- **Supersedes:** [0021](0021-canonical-tuple-identity.md) (for implementation; the §1–§4 NORMATIVE shape remains binding)
- **Source:** Slice 4 of the doiget Phase-2 roadmap

## Context

[ADR-0021](0021-canonical-tuple-identity.md) defined the
`CanonicalRef = (source_type, source_id, resolver_profile, version)`
audit identity and the `canonical_digest` provenance-log column, but
shipped as **spec-only**: the type was reserved for Phase 2 and Phase 1
provenance rows had no `canonical_digest` column.

This ADR records the Slice-4 implementation: the type ships, every
provenance row now carries the digest (or `null` for session bookends),
and a one-shot migration brings pre-existing v1 logs onto the v2 row
shape.

## Decision

### 1. Type surface (additive)

`doiget_core::CanonicalRef` and `doiget_core::SourceType` are
re-exported from the crate root (`docs/PUBLIC_API.md` §1). The struct
is `#[non_exhaustive]`; external code constructs values via
`CanonicalRef::new` or `Ref::promote` (the latter is the ergonomic
path for orchestrators that already hold a `Ref`).

The digest algorithm is the literal `SHA256(source_type | 0x00 |
source_id | 0x00 | resolver_profile | 0x00 | version_or_empty)` shape
ADR-0021 §1 named — `version_or_empty` is the empty byte sequence when
`version` is `None`, NOT a sentinel.

### 2. Provenance log schema bump (v1 → v2)

The on-disk `LogRow` (`docs/PROVENANCE_LOG.md` §3) gains two new
required fields:

- `schema_version: String` — always the literal `"v2"`
  (`doiget_core::provenance::LOG_SCHEMA_VERSION`) for rows written by
  this build.
- `canonical_digest: Option<String>` — 64 lowercase hex chars when the
  row has a meaningful audit identity (`Fetch` / `Resolve` /
  `StoreWrite` with a `ref`); `None` (wire form `null`) on session
  bookend rows.

Both fields participate in the SHA-256 hash chain. In v2 the
lex-first top-level key of the canonical JSON is `canonical_digest`
(< `capability`, both share the `"ca"` prefix and `n` < `p`).

`#[serde(deny_unknown_fields)]` plus a non-defaulted `schema_version`
mean v1 rows fail to parse loudly — the operator sees a clear
`corrupted log at line N` message rather than silent hash-chain
mismatches.

### 3. One-shot migration

`doiget_core::provenance::migrate_v1_to_v2` is the binding
implementation. The CLI exposes it via
`doiget provenance migrate [--dry-run]`. The migration is:

- **One-shot**: v1 logs are rewritten as v2 in place; the original is
  preserved at `<log_path>.v1-backup`.
- **Idempotent**: re-running on a v2 log re-parses every row via the
  v2 fallback path and produces byte-equivalent output (asserted by
  `migrate_is_idempotent_on_v2_log` in
  `crates/doiget-core/tests/provenance_migration_e2e.rs`).
- **Dry-runnable**: `--dry-run` returns a `MigrationReport` without
  touching disk.

For each v1 row, the migrator derives the `canonical_digest` by
treating the v1 `source` field as `resolver_profile`, the v1 `ref`
field as `source_id`, and `version = None` — matching the migration
recipe ADR-0021 §3 (clause 2) named. The `source_type` is recovered
from a `Ref::parse`-style heuristic (`ref` starts with `10.` ⇒ Doi,
else Arxiv).

The migration recomputes the SHA-256 hash chain from scratch under the
v2 canonicalization — the v1 chain is invalidated by the schema change.
The first-row v1 anchor and the recomputed first-row v2 anchor are
surfaced in `MigrationReport` for operator traceability.

Before the live rename, the staged v2 file MUST pass
`verify`; if not, the migration aborts without touching the live log.

### 4. MCP surface

`doiget_fetch_paper`, `doiget_metadata_only`, and each per-row entry of
`doiget_batch_fetch` results now include a `resolver_profile` string
field per ADR-0021 §4 ("at fetch time, which canonical identity was
just minted"). In Slice 4 the field equals `source` verbatim; the
field is kept distinct so future slices can decouple
"which resolver wrote to disk" from "which resolver is the audit
identity" when overlapping resolvers ship.

`doiget_info` and `doiget_audit_log` will surface the four tuple
fields plus the hex digest when they are wired in later slices
(ADR-0021 §4 — out of scope here).

### 5. Test vectors

- 16 `CanonicalRef::digest` golden vectors in
  `crates/doiget-core/src/canonical.rs::tests` cross-check every
  combination of source_type × resolver_profile × version=None|Some
  against an in-test reference SHA-256 reimplementation.
- 4 end-to-end migration tests in
  `crates/doiget-core/tests/provenance_migration_e2e.rs` drive the
  migrator against the synthetic v1 fixture at
  `tests/fixtures/provenance/migration_v1_to_v2.json` and assert
  byte-equality of each row's `canonical_digest` against an
  independent reference path, plus dry-run / idempotency invariants.

## Consequences

**Positive.**

- An LLM agent can ask "did doiget previously fetch `10.1234/foo` via
  Unpaywall?" and get a deterministic answer (ADR-0021 Consequences
  §+1 now actually ships).
- The audit log distinguishes the Crossref metadata leg, the Unpaywall
  enrichment leg, and the oa-publisher PDF leg of a single DOI fetch
  via three distinct `canonical_digest` values — exactly the
  resolver-distinction the external review on Discussion #12 named as
  a security concern.

**Negative.**

- **BREAKING**: any pre-existing v1 access log MUST be migrated via
  `doiget provenance migrate` before this binary will read it. The
  migration tool itself reads v1 rows via a separate shadow struct, so
  the migration path is unaffected — but the live writer and verifier
  reject v1 rows with a `corrupted log at line N` error.
- ~80 bytes per row of additional on-disk size (the hex digest plus
  the `schema_version` literal). ADR-0021 Consequences §-1 already
  named this cost.

**Out of scope.**

- Surfacing `canonical_digest` via `doiget_info` and `doiget_audit_log`
  MCP tools — those tools are not wired yet; the spec contract is
  preserved by ADR-0021 §4 and will be honored when they land.
- Threading an arXiv version token (`v2`, etc.) through the digest —
  Slice 4 sets `version = None` everywhere. A follow-up slice can
  decode the Atom-feed `id` element and pass the discriminator.
- Splitting on-disk storage by `resolver_profile` (rejected — see
  ADR-0021 §2).

To revise this decision, write a new ADR with `Supersedes: 0024` and
update this file's `Status:` per `CONTRIBUTING.md`.
