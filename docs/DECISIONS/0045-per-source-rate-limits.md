# 0045 - Per-source rate limits, taken as the stricter of the vendor's terms and the global cap

- **Date:** 2026-08-25
- **Status:** Accepted
- **Supersedes:** -
- **Source:** #493 (arXiv fetched at 15x its published rate), #496 (no vendor limit recorded anywhere), #498 (the 2026-08-25 vendor-terms audit)

## Context

`RateLimits` carried three numbers for the whole process: 5 concurrent fetches,
5 requests per second, 200 ms between consecutive requests to the same source.
There was no per-source dimension at all.

arXiv's [API Terms of Use](https://info.arxiv.org/help/api/tou.html) say:

> make no more than one request every three seconds, and limit requests to a
> single connection at a time

That is **15x the permitted rate and 5x the permitted concurrency**, on a Tier-1
always-on source that ships in every published binary. The page adds that the
limit is collective across every machine under the caller's control and that
circumventing it may have access blocked.

Three places asserted the opposite — `sources/arxiv.rs`, `docs/SOURCES.md` §4,
and, worst, `docs/SOURCES.md` §6:

> If a source publishes a stricter rate guideline, doiget will adopt the stricter
> value at the per-source level rather than relax the global cap.

arXiv publishes exactly such a guideline. Nothing adopted it. The promise was
real and the mechanism to keep it did not exist.

A second, quieter problem: arXiv's terms cap **requests**, and one arXiv attempt
issues **two** — the Atom feed, then the PDF — under a single `acquire`. Even a
perfectly serialised caller sent those back to back.

## Decision

**D1 — A per-source table of vendor guidelines, as library constants.**

`SOURCE_RATE_OVERRIDES: &[(&str, SourceRate)]`, keyed by `Source::name`, with
`SourceRate { min_interval_ms, max_concurrent }`. arXiv is 3000 ms / 1.

Constants, not configuration, and not caller-supplied. `docs/LEGAL.md` §6a
safeguard 5 makes `RateLimits` unsynthesizable by downstream code on purpose;
a per-source table that accepted caller values would hand back precisely what
that safeguard withholds.

**D2 — An entry can only tighten.**

Callers go through `RateLimits::backoff_ms_for(source)` and
`max_concurrent_for(source)`, which return `max(global, entry)` and
`min(global, entry)` respectively. The global cap stays the ceiling.

This is the load-bearing part. §6's promise was kept by intention before, and
intention is what failed. Taking the stricter of the two makes it impossible for
a table entry to relax anything even if someone writes one that tries, and the
table itself is pinned by a test asserting every entry is at least as strict as
the global cap on both axes — so an entry that would be silently ignored fails
the build instead of sitting there looking effective.

**D3 — Concurrency is enforced with a per-source semaphore, after the global one.**

Ordering matters: global semaphore first (the ceiling still binds), then the
per-source one, then the interval wait. A source capped at one connection
therefore serialises rather than accumulating sleepers behind a shared interval.

**D4 — `RateLimiter::pace(source)` for additional requests inside one attempt.**

It waits out the source's interval and records the start, without touching the
concurrency slots — the caller already holds them, and re-taking them would
deadlock at `max_concurrent == 1`, which is exactly the arXiv case. The arXiv
PDF leg calls it.

The alternative — one `acquire` per HTTP request — would have been cleaner in
the abstract and wrong here: the permit's lifetime is what bounds concurrency
for the whole attempt, and splitting it would let a second attempt interleave
between one attempt's two legs.

## Consequences

**arXiv fetches are now ~6 s per paper** (two paced requests), against well under
a second before. That is the rate the vendor publishes. A batch of arXiv refs is
correspondingly slower, and there is no opt-out: per D1 there is no knob, and per
LEGAL.md §6a there should not be one.

**Nothing else changes.** Every source without an entry keeps the 200 ms floor
and the global concurrency cap; there is a test for that specifically, so this
change cannot have slowed the rest of the tree by accident.

**The table is now the place vendor limits are recorded.** #496 tracks the ones
still unwritten (Springer's tier system, CORE's 10/min token bucket, OpenAlex).
Adding them is an entry each, not a redesign — which is the point of doing the
mechanism and arXiv together rather than per source.

**`docs/SOURCES.md` is `Status: NORMATIVE`**, so per ADR-0014 this ADR is the
record for the change to §4 and §6.

## Alternatives rejected

- **Lower the global cap to 1 request / 3 s.** Correct for arXiv, absurd for
  Crossref, and it would make every other source pay arXiv's price.
- **A config knob.** Directly against LEGAL.md §6a safeguard 5 and safeguard 8:
  the rate limit is hard-coded so that it cannot be argued with.
- **Pace only between attempts, not within one.** Leaves the measured violation
  in place — arXiv counts requests, and an attempt is two of them.
- **Per-source `Retry-After` only, reacting to 429s.** Reactive, and the terms
  are a *guideline to observe*, not a limit to discover by tripping it.
