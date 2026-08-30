# 0054 - An access refusal is a type, and it collapses to `NO_OA_AVAILABLE`

- **Date:** 2026-08-30
- **Status:** Accepted
- **Supersedes:** -
- **Complements:** [0014](0014-docs-class-system.md) — this is the ADR that document requires for the `docs/ERRORS.md` §2 change below
- **Source:** #538 (an access refusal was classified by substring, so rewording one silently reclassified the row)

## Context

A source signalling *"I found it and cannot give it to you"* returned
`FetchError::SourceSchema { hint }` with an explanatory string, and
`classify_attempt` decided what the trace row said by **reading that string
back**:

```rust
fn is_access_refusal(hint: &str) -> bool {
    hint.contains("not open access")
        || hint.contains("openAccess")
        || hint.contains("no retrievable PDF")
}
```

Match → `AttemptOutcome::NotOpenAccess`, *"consulted: found, not open access"*.
Miss → `AttemptOutcome::Failed`, *"consulted: failed"*. Those are different
claims to an operator: **the source has it and will not give it to us** versus
**the source broke**, and only the second is a bug to chase.

It had already fired. #503 reworded Europe PMC's refusal from *"is indexed but
not open access"* to *"advertises no retrievable PDF"* — correctly, because the
gate moved from OA-subset membership to per-entry retrievability — the hint fell
out of the predicate, and every Europe PMC refusal became `Failed`. Nothing in
`europepmc.rs` said the wording was load-bearing. `hal` matched on the substring
`openAccess`, which comes from a JSON **field name**, not prose anyone chose.

It was also latent for every future source: an author had no way to learn that
the phrasing of an error message decides how the row renders.

There is a second defect underneath. `SourceSchema` collapses to
`ErrorCode::InternalError`, so whenever such a refusal reached the boundary it
was reported as *a bug in doiget* — for the ordinary situation of a paper not
being free at one repository.

## Decision

**1. The refusal is a variant.**

```rust
FetchError::NotRetrievable { source_key: String, detail: String }
```

`classify_attempt` matches the variant. `is_access_refusal` is deleted. `detail`
is carried verbatim into the row, because the reason is for a reader — it is
just no longer *parsed*.

**2. It collapses to the existing `NO_OA_AVAILABLE`, and the closed set does
not widen.**

`docs/ERRORS.md` §3 defines a closed `ErrorCode` set, and #538 asked explicitly
whether a new code was needed. It is not: *"found it, no free copy"* is what
`NO_OA_AVAILABLE` already means. Adding a code would have split one situation
across two wire values for an internal refactor, and every consumer switching on
the code would have needed to learn the new one to keep behaving the same.

`docs/ERRORS.md` §2's description of `NO_OA_AVAILABLE` is widened to match: it
said *"Tier 1 sources reported no OA URL"*, and it now also covers an optional
source that holds the record but no retrievable copy. That is the NORMATIVE
change ADR-0014 requires this ADR for. **The wire value and its recoverability
guidance are unchanged.**

**3. It is not a `DenialContext`.**

`From<&FetchError> for Option<DenialContext>` returns `None`. ADR-0023's denial
channel is for policy refusals — a capability to grant, an allowlist to widen.
An access refusal is a fact about the work: there is no configuration change
that makes a closed paper open, and offering a `denial_context` would send a
reader after one that does not exist.

## Consequences

- Rewording a refusal can no longer reclassify a row. The compiler is the guard,
  which is what #538 asked for.
- An access refusal that reaches the boundary stops being reported as
  `INTERNAL_ERROR`. This is a **wire-visible behaviour change for the same
  input**, and a strictly more accurate one; the code it now returns was already
  in the closed set and already documented as recoverable-by-enabling-a-source.
- Adding a source no longer requires knowing a phrasing convention. The variant
  is the contract.
- Three tests that asserted `matches!(err, FetchError::SourceSchema { .. })` now
  assert the category and the collapsed `ErrorCode`, which is a stronger claim.
  One test asserted the *phrase* `"no retrievable PDF"`; it now asserts the
  criterion the detail names (`documentStyle = pdf`, `availabilityCode`), since
  asserting the phrase would have rebuilt the prose-coupling inside the test.
- `error.disposition` (#506) is left for its own ADR. An access refusal is
  `needs_config` only when another source could be enabled, and that judgement
  belongs with the disposition design rather than smuggled in here.
