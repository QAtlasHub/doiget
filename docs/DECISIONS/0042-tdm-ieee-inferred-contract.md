# 0042 - `tdm-ieee` ships against an inferred contract, marked unverified

- **Date:** 2026-08-24
- **Status:** Accepted
- **Supersedes:** -
- **Source:** #430 (deferred from #407 by [0039](0039-publisher-hosts-stay-off-allowlist.md))

## Context

[ADR-0039](0039-publisher-hosts-stay-off-allowlist.md) settled that per-publisher
TDM credentials are the supported route for IEEE / ACM / SIAM / AMS: their web
front ends answer a scripted client with `202` and an empty body regardless of
entitlement, so widening `oa-publisher` would move the failure one step later
rather than fix it. #430 is the IEEE half of that route.

#430 states its own blocker plainly: IEEE's programme contract — endpoint, auth
shape, response shape, rate limits, terms — could not be obtained from outside
the programme, which requires registration and a project summary before a key is
issued.

What *is* public, from IEEE's developer portal and its published SDKs:

- base `https://ieeexploreapi.ieee.org`, path `/api/v1/search/articles`
- the key as an `apikey` **query parameter**; no header-auth path is documented
- a JSON envelope `{ total_records, total_searched, articles: [...] }`

Not public: whether the TDM programme uses this same API surface, the response
shape under a TDM entitlement, and the rate limits.

`SOURCES.md` §5 requires an ADR locking the Tier and the Cargo feature name for
any new Tier-3 source. This is it, and it has one more thing to lock: what it
means to ship a source whose upstream contract is a reading of documentation
rather than an observation.

Three options:

- **(a)** wait for a key — leave #430 open until someone joins the programme
- **(b)** implement against the public shape, marked unverified, failing loudly
- **(c)** implement against the public shape and present it as verified

## Decision

**(b) — implement, mark unverified, and make every uncertain assumption fail
loudly rather than quietly.** Tier 3, Cargo feature `tdm-ieee`, prefixes
`10.1109` and `10.23919` per [0041](0041-tdm-sources-scoped-to-publisher-prefixes.md).

Concretely:

- The `SOURCES.md` §1 row is marked **`(unverified)`**, and §4 carries a
  subsection naming exactly which three facts are inferred. The marking is not
  decoration: it may not be removed until a fetch with a real key has been
  observed.
- A response that is not the expected envelope is a `SourceSchema` error that
  **names the missing field and quotes the body**. The first run against a real
  key therefore reports the actual contract instead of returning "no records"
  for a response that may have been a perfectly good record in another shape.
- `DOIGET_IEEE_BASE` exists from the start — unlike the first three Tier-3
  sources, which shipped without a base override and so could not be pointed
  anywhere, which is why nothing could prove they were reachable (#442). Here it
  also serves the specific purpose of replaying a recorded real response against
  a fixture.
- `format=json` is sent explicitly rather than relying on the documented default,
  so an upstream default flip surfaces as a request we made wrong rather than as
  a parse error.
- The full-text endpoint is **not** implemented. Its contract cannot be inferred
  at all from outside the programme, and it would additionally need the eight
  ADR-0019 safeguards wired through the orchestrator. `tdm-ieee` is
  metadata-only, like the other three.

**(a) was rejected** because the cost of waiting is asymmetric. The gates are
opt-in three times over (feature, key, agreement), so an unverified source is
inert for every user who has not joined the programme — which is everyone who
cannot verify it either. Waiting keeps the module unwritten and the *next*
person still cannot start, because the missing piece is the one thing a key
holder can supply in five minutes and a non-holder can never supply at all.

**(c) was rejected** outright. An unverified contract presented as verified is
the failure mode this project has spent three issues on: #442, #438 and #454 are
all "documented as working, never actually reached". Shipping a fourth on a
guess and not saying so would be the same defect with a new name.

## Consequences

**Positive.**

- A key holder can exercise the whole path today, and the first failure they hit
  reports the real contract rather than a shrug.
- IEEE was the corpus #407 measured as metadata-resolvable but unfetchable. That
  gap now has a route, gated behind the user's own agreement with IEEE.
- ACM / SIAM / AMS can follow the same shape, and this ADR is the precedent for
  how to do so before a key exists.

**Negative / accepted.**

- The source may not work on first contact. That is stated in `SOURCES.md` §1,
  §4 and in the module docs, and the failure is diagnostic rather than silent.
- Rate limits are unknown; the source runs under the same hard-coded limiter as
  every other, which may be more or less polite than IEEE requires.
  `SOURCES.md` §6 commits to adopting a stricter published limit once known.
- The key travels in the URL query, as Springer's does. `redact_api_key_query`
  now knows both spellings (`api_key`, `apikey`); a third source with a third
  spelling that does not add it there leaks its key into `HttpError::HttpStatus`.
  This is a registry that must be maintained by hand, and the module docs say so.

## Addendum, 2026-08-24 (#460) — first contact, without a key

One unauthenticated request settled part of the above:

```text
GET https://ieeexploreapi.ieee.org/api/v1/search/articles?doi=...&format=json
HTTP=403  content-type=text/xml
<h1>Developer Inactive</h1>
```

**Confirmed:** the base and path. The host resolves and the endpoint is served, so
the first of the three inferred facts is now an observed one.

**Corrected:** the error body is not JSON, and `format=json` does not make it so.
The decision above assumed the loud-failure path would be `SourceSchema`; for an
unauthorised caller it is `HttpError::HttpStatus`, which never reaches the JSON
parsing at all. The fixture that encoded the wrong assumption
(`{"error": "Developer Inactive"}`) was a guess and has been replaced by the
observed body, plus a test on the HTTP branch asserting the status is legible and
the key is redacted out of it.

**Still unverified:** the 200-response envelope and the rate limits. The
`(unverified)` marking in `SOURCES.md` §1 stands and now means specifically those
two, not the endpoint.

Also found while doing this: `tdm-ieee` **did not compile on its own**.
`TdmGrant::api_key` is gated on an `any(...)` that named the first three publishers
and not the fourth, and the four-way CI job hid it because the others were always
present. Fixed, with an `oa-only,tdm-ieee` singleton added to the clippy matrix —
one singleton job is not enough; each publisher needs its own.

**Revisit** when a run with a real programme key is observed: correct whatever it
disagrees with, drop the `(unverified)` marking, and record the observed rate
limits.
