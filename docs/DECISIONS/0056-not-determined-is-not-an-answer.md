# 0056 - A not-determined marker never overwrites a determination

- **Date:** 2026-08-31
- **Status:** Accepted
- **Supersedes:** -
- **Amends:** [`docs/STORE.md`](../STORE.md) §6 — the re-fetch downgrade note
- **Complements:** [0014](0014-docs-class-system.md) — the ADR a NORMATIVE doc change requires; [0055](0055-error-disposition.md), which drew the same distinction on the wire
- **Source:** #583 (a default `metadata_only` re-write silently downgraded `[doiget].oa_status` and `.license`)

## Context

`docs/STORE.md` §6 lets doiget rewrite the `[doiget]` table on a re-fetch, and
says why:

> A doiget re-fetch of an entry that previously had a PDF but is now
> metadata-only ... rewrites the `[doiget]` table (`source`, `size_bytes`, …)
> in place. **This is intentional, not silent:** as of issue #118 the
> blocked-PDF reason is surfaced to the caller ... so the operator always
> learns the entry was downgraded and why.

The permission is conditional on the report. Since #539 there is a path where
the report does not exist: `metadata_only` takes `include_oa_location`, and when
it is omitted — the ordinary call shape — the OA lookup never runs. The record
built from that outcome carries `oa_status: None` and `license: "unknown"`, the
merge let them win, and a caller got no `note:` line, no `pdf.status`, and no log
row saying a known `gold` / `CC-BY-4.0` had been replaced with *not determined*.

Measured before deciding, because the issue as filed claimed the wrong field:

| field | existing | after a default re-write |
|---|---|---|
| `url` (where `oa_url` lands) | `Some("https://…/paper.pdf")` | `Some(…)` — preserved by `merge_opt!` |
| `[doiget].oa_status` | `Some("gold")` | `None` |
| `[doiget].license` | `"CC-BY-4.0"` | `"unknown"` |

So the reserved top-level fields were never at risk. The defect is confined to
the two `[doiget]` fields whose absent value is a **marker** rather than a
reading — and both are documented as such: `oa_status` is "omitted when not
determined (#281)", `license` is "an OA license string, or the literal
`unknown`".

## Decision

**A marker for "no answer" does not overwrite an answer.**

In `merge_metadata`, the `[doiget]` arm keeps the existing `oa_status` when the
incoming one is `None`, and the existing `license` when the incoming one is
`LICENSE_UNDETERMINED`. Everything else in the table still follows §6: doiget
owns it and the re-write wins.

This cannot suppress real news, which is the only reason it is safe:

- a paper that stops being open access reports `oa_status: Some("closed")`
- a license that changes reports the new string

Neither reports the marker. `None` and `"unknown"` are only ever produced by a
call that did not look, so preferring the stored value is not a guess about
which is newer — it is the observation that only one of the two is a value.

`"unknown"` gained a name (`LICENSE_UNDETERMINED`) so the merge does not depend
on a string literal matching the others scattered through the orchestrator.

## Consequences

`docs/STORE.md` §6's note says a downgrade guard "is deferred (post-MVP) — it is
a policy choice, not a correctness bug." That remains true of the case it was
written about (#123: an entry loses its PDF because the OA host went
off-allowlist) — there the downgrade is real, and it is reported. It is not true
of a field the caller never asked about, so the note is amended to scope its
claim to determinations rather than markers.

Nothing BiblioFetch.jl reads changes shape. No field is added, removed or
renamed, `schema_version` does not move, and the reserved top-level fields keep
the `merge_opt!` behaviour they already had. This is a change to which of two
`[doiget]` values doiget itself keeps.

### Not done

**Warning on every preserve.** The reserved-field arms `warn!` when they keep an
existing value, because there it means two tools disagree and a human may need
to look. Here it is the ordinary outcome of the ordinary call shape; a log line
per default `metadata_only` would be noise, and the thing worth reporting —
losing the value — no longer happens.

**Extending this to `source` or `size_bytes`.** They have no marker value.
`size_bytes: 0` is a legitimate reading for a metadata-only entry, and `source`
always names the resolver that actually ran. Treating either as absence would
invent exactly the call-history dependence #583 warned against.
