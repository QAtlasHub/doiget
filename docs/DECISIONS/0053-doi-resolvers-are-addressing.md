# 0053 - A DOI resolver is addressing, not hosting

- **Date:** 2026-08-30
- **Status:** Accepted
- **Supersedes:** -
- **Complements:** [0027](0027-redirect-allowlist-society-hosts.md) — answers a layer question 0027 never had to ask, and [0039](0039-publisher-hosts-stay-off-allowlist.md), which decided the adjacent case the other way for the same reason
- **Source:** #533 (a gold-OA cc-by paper refused at `doi.org`, one hop before an already-allowlisted publisher)

## Context

`doiget fetch 10.1002/pcn5.205` — Harada & Kato 2024, **gold OA, cc-by** — was
refused:

```
redirect target doi.org not in allowlist for source oa-publisher
```

with a remediation offering `doi.org` and `*.doi.org` as hosts to add.

This is not an incomplete list. It is a gate applied at the wrong layer.

Unpaywall reports `best_oa_location.url` for this work as literally
`https://doi.org/10.1002/pcn5.205`, with no `url_for_pdf` (verified against the
live API, 2026-08-30). That is not unusual — it is the normal shape for
publisher-hosted gold OA. So the very first host doiget touches on the fetch leg
is the DOI resolver, and it was being adjudicated as though it were the place
the bytes come from. The chain was refused before it ever reached
`onlinelibrary.wiley.com`, whose `*.wiley.com` was already on the list.

The offered remediation is worse than the refusal. ADR-0027 justifies the
built-in list as *bounded* registrable-domain wildcards for established
publishers and repositories, and the list contains exactly that — no resolvers.
Adding `doi.org` would not widen the trusted surface toward one publisher. It
would remove the bound entirely, because **every DOI in existence resolves
through it**. An agent following that advice gets its PDF and silently loses the
invariant the allowlist exists to hold.

ADR-0039 refused to add IEEE/ACM/SIAM/AMS to `oa-publisher` because those are
content hosts and the ADR would not widen the content surface. The same
principle decides this case the opposite way, and for the same reason: a
resolver is not a content host at all, so making it followable widens nothing.

Three recent bugs share this shape — #503 (europe-pmc refused on a flag before
consulting the URL list), #516 (a Tier-2 gate keyed on the wrong feature), and
this one. #462 names why they survive review: *"every 'unreachable source' bug
passed its unit tests."* The allowlist matcher here is correct in isolation; the
defect only appears once `unpaywall → doi.org → wiley` is actually walked.

## Decision

**A closed set of DOI resolver hosts is transparent to host adjudication.** They
are followed, but never allowlisted, never named as remediation, and never
counted as the source of the content.

```
doi.org          the canonical DOI resolver
dx.doi.org       its long-standing alias, still present in live metadata
hdl.handle.net   the Handle System resolver doi.org proxies
```

Each was measured issuing a single 302 straight to the publisher (2026-08-30,
`10.1002/pcn5.205`).

Three constraints on that set:

1. **Exact hosts, no wildcards.** `*.doi.org` would sweep in `www.doi.org` —
   the DOI Foundation's website, not a resolver — and anything else ever stood
   up there. `evil-doi.org` and `doi.org.evil.test` are what an attacker
   registers.
2. **The host that serves the bytes is adjudicated exactly as before.** This is
   transparent to the ADR-0027 invariant, not an exception to it.
3. **One predicate, not five gates.** `SourceAllowlist::permits` is what every
   adjudication site calls; `matches` remains the narrower "is this host on the
   list" used to build and assert the lists themselves. A resolver is therefore
   never reported inside any source's `expected_hosts`.

### Rejected: adjudicate only the terminal host

#533's first suggestion. It is a larger change than it appears: a chain could
traverse *any* host so long as it ended somewhere allowed, and every hop still
observes the request. A named, closed set keeps the bound while fixing the
reported class.

### Rejected: allowlist `doi.org`

The remediation the tool was emitting. Mechanically effective, semantically
wrong, and it deletes the invariant — see Context.

## Consequences

- Publisher-hosted gold OA whose Unpaywall location is a `doi.org` URL is
  reachable. This is a whole class, not one paper.
- No new content host is trusted. The set contains no host that serves papers.
- A denial now names the host the user would actually have to trust, because a
  resolver hop can no longer produce one.
- **This does not promise a PDF.** `10.1002/pcn5.205` reaches Wiley and then
  meets Wiley's own cookie wall (`/action/cookieAbsent`). That is a publisher
  posture question — ADR-0039's territory — and it fails honestly at the
  publisher rather than dishonestly at the addressing layer. The fix removes a
  wrong refusal; it does not manufacture access.
- A posture-lint step fails any adjudication site that calls `matches` on a host
  variable, so the fifth gate cannot be added without the fourth's lesson.
