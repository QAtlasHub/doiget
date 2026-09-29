# 0057 - Repeat suppression: a session is not re-asked what a retry cannot change

- **Date:** 2026-09-29
- **Status:** Accepted
- **Supersedes:** -
- **Amends:** [`docs/LEGAL.md`](../LEGAL.md) §2 and §6a.5 (the rate-limit safeguard), per [0014](0014-docs-class-system.md)
- **Builds on:** [0006](0006-provenance-log-fail-closed.md) (the log is fail-closed, so its rows exist before the network is touched), [0055](0055-error-disposition.md) (every code has a disposition)
- **Source:** #507 (step 1: the `session_end` bookend records the code the caller was given; step 2: this)

## Context

LEGAL.md §6a.5 claimed the hard-coded rate limit "prevents bulk-scraping
patterns and cannot be overridden by configuration". It bounds requests per
second. It does not bound how many times the same request is made. An agent
that retries 500 refused DOIs ten times each emits 5,000 requests at 5/s and
never trips the limit. That is a bulk pattern, produced inside the safeguard,
by a caller behaving the way LLM callers behave when a failure looks
retryable.

The record needed to stop it already exists. Every call ends with a
`session_end` row naming the ref and the code the caller was told (#507 step
1), and ADR-0055 gives every code a disposition: `terminal`, `retry_after` or
`needs_config`.

## Decision

**D1: What is suppressed.** Before any network, `fetch_paper_with` asks the
session's `RepeatIndex` (`doiget_core::repeat`) about the ref:

| earlier answer's disposition | within | result |
|---|---|---|
| `terminal` / `needs_config` | 10 min | `FetchError::Replayed` carrying that code; no request |
| `retry_after` | 30 s | `FetchError::Replayed` carrying `RATE_LIMITED` and the seconds left |
| anything | after that | asked as usual |

**D2: Scope is one session.** That means one `doiget serve` process, or one
CLI run such as `batch`, where duplicates are caught too. The capability
profile is fixed for a session, so "nothing that decides the answer has
changed" holds by construction. The one exception is `config.toml`, which the
HTTP client reads per call. Each entry carries a fingerprint of that file, and
a changed file lifts the replay, so a user who follows a `needs_config`
remediation is asked again at once. Scoping to the session rather than keying
on a profile fingerprint was the finding of the #507 spike.

**D3: The index is fed from the log.** `ProvenanceLog::append` updates it only
after a `session_end` row is durably written. The log remains the record of
what callers were told. A clean success clears the ref. An answer with the same disposition
as the recorded one keeps the first time, so a loop cannot slide the window
forward. The same rule makes a replay's own bookend harmless: a replay
reports the disposition it replayed (a `Wait` reports `RATE_LIMITED`, which is
`retry_after` like the answer it waits on), so polling during a `Wait` cannot
restart it. No request is marked as a replay, so a genuine answer that lands
concurrently is never mistaken for one; a different disposition, or a
success, replaces the entry.

**D4: It is never silent.** A replay is `ok:false` with `replayed: true`, the
original code and disposition, the time of the original answer, and how to
ask anyway. This includes the case where the first call was `ok:true` with a
blocked PDF leg. Turning that into `ok:false` on the repeat is a deliberate,
wire-visible change: the repeat genuinely did nothing, and saying `ok:true`
again would present a replay as a fresh result.

**D5: Overridable by the caller, never by configuration.** `force: true` on
`doiget_fetch_paper`, `doiget_batch_fetch` and `doiget_batch_from_bibliography`
(`--refetch` on `fetch` / `batch`) asks anyway. A `repeat_forced` row records
the override with the code it overrode. The window and the gap are library
constants. No setting turns suppression off, which is the property §6a.5
requires of a safeguard.

## Consequences

- §6a.5 now claims what the mechanism does: it bounds rate **and**
  persistence within a session.
- Not covered: persistence *across* sessions. A new `doiget fetch` process
  starts with an empty index, deliberately. A negative answer is not
  permanent (a paper can become OA, an embargo can lift), and a cache of
  "no" that outlives the session would be the silent staleness #507's own
  constraints ruled out.
- Not covered: `doiget_resolve_paper`, `doiget_metadata_only` and the CLI's
  metadata-only paths. They make one or two metadata requests per call, not a
  content fetch, and stay bounded by the rate cap alone. Extending the index
  to them is a separate decision.
- An index whose lock was poisoned by a panic elsewhere is recovered, not
  treated as empty: a poisoned lock must not become an off switch.
- `FetchError` gains `Replayed`. It maps to the replayed code, so every
  surface's code, exit code and disposition stay those of the original
  answer.
- A second `doiget_fetch_paper` for a blocked ref in the same session is now
  `ok:false`. Callers that retried on `ok:true` + blocked leg must pass
  `force` to get a fresh attempt.
