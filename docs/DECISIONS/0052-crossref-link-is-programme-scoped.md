# 0052 - Crossref `link[]` is programme-scoped, so the fetch path does not carry it

- **Date:** 2026-08-26
- **Status:** Accepted
- **Supersedes:** -
- **Complements:** [0048](0048-access-ceiling.md) — this is the first widening proposal tested against the ceiling that ADR wrote down, and the ceiling held
- **Source:** #517 (the fetch path never contacts the publisher for a closed DOI, and CONFIG.md §6.1 names two blockers when there are three)

## Context

For a DOI with no Open Access copy, `doiget fetch` issues no request to the
publisher. Not because an attempt is refused — because there is nothing to
attempt:

```rust
let chain = extract_oa_url_chain(r.metadata_json.as_ref());   // Unpaywall ONLY
let (pdf_leg, pdf_bytes) = if oa_chain.is_empty() {
    (PdfLegStatus::NoOaUrl, None)
```

Unpaywall reports OA locations. A closed work has none, so the chain is empty and
the leg ends before any host is chosen. Entitlement is never consulted because no
request is ever formed.

`docs/CONFIG.md` §6.1, "Institutional networks: what works and what does not",
named two blockers — the `oa-publisher` allowlist and the publisher bot wall —
and **neither was reached**. A third sat in front of both. A reader entitled to a
paywalled paper concluded that widening the allowlist or obtaining TDM
credentials was the remedy, and for a closed DOI neither could help. The run
exited **0** with `metadata-only: no OA PDF available`.

#517 offered two positions: keep the invariant and write it down, or widen the
fetch path to carry a Crossref-derived candidate so that §6.1's blockers become
real. The maintainer chose to widen.

## The measurement that settled it

The candidate would have come from Crossref `message.link[]`. Every entry there
carries `intended-application`:

| value | what it is |
|---|---|
| `unspecified` | a general full-text link |
| `text-mining` | scoped to the publisher's TDM programme |
| `similarity-checking` | scoped to Similarity Check / iThenticate |
| `syndication` | scoped to syndication partners |

Live Crossref, 2026-08-26, six DOIs chosen to span the publishers this question
is actually about:

| DOI | publisher | `intended-application` values |
|---|---|---|
| `10.1137/0117004` | SIAM | `similarity-checking` |
| `10.1109/TSP.2018.2812747` | IEEE | `similarity-checking` |
| `10.1090/s0025-5718-04-01692-8` | AMS | `similarity-checking` ×2 |
| `10.1145/3292500.3330701` | ACM | `text-mining`, `similarity-checking` |
| `10.1038/nature12373` | Springer Nature | `text-mining` ×2, `similarity-checking` |
| `10.1098/rspa.2014.0585` | Royal Society | `text-mining` ×2, `similarity-checking` |

Plus the eight captured responses in `tests/fixtures/real_world/`:
`text-mining`, `syndication` ×2, `similarity-checking` ×4, and two with no
`link[]` at all.

**Twelve live entries and eight fixtures. Not one `unspecified`.**

Crossref `link[]` is therefore a **programme-scoped channel**, not a general
full-text channel. There is no general-purpose candidate in it to carry.

## Decision

**D1 — The fetch path does not carry a Crossref-derived candidate.** The ceiling
in ADR-0048 §2a holds unchanged. Wiring it would have meant one of two things,
and both are wrong:

- **With the `unspecified` filter:** a branch that never executes, while
  `LEGAL.md` and `CONFIG.md` claim a capability. That is the defect class this
  repository keeps closing — #413, #442, #454, #458, #476, #504, #509 — shipped
  deliberately this time.
- **Without the filter:** following Similarity Check and TDM links **without
  holding those licences**. doiget already has the licensed route for exactly
  this: its Tier-3 TDM sources, with the user's own credential and a recorded
  per-publisher agreement (`LEGAL.md` §6a.2). Taking the same URLs without that
  is the thing the whole Tier-3 apparatus exists to avoid.

**D2 — `extract_crossref_oa_url` is fixed anyway, and renamed.** It returned the
**first** `link[]` entry with no filtering, and its result is
`MetadataOnlyOutcome.oa_url`, whose doc comment invites callers to act on it
"for separate action". So doiget was already handing out Similarity Check and
TDM URLs under the name `oa_url` — to the MCP surface, and to anyone reading the
`metadata_only` output. `extract_crossref_publisher_url` accepts only
`unspecified`, and refuses an unlabelled entry too, because ADR-0048 D2 draws the
line at documented-by-the-vendor versus guessed-by-us.

In practice this means `oa_url` is now `None` where it was previously a
programme-scoped URL. **Seven real-world fixtures asserted the old value**, which
is the strongest available evidence that it was behaviour rather than an
accident; their expectations are corrected with the reason written in beside
them.

**D3 — `CONFIG.md` §6.1 gains the blocker that was in front of the two it
named**, and states plainly that it is not a bug awaiting a fix — the supported
route for a closed work is a TDM credential, which is the licensed version of
the thing #517 wanted.

## Consequences

**#517's first position wins, but on evidence rather than preference.** The issue
was right that the decision had to be recorded; it framed the choice as a
judgement call, and it turned out to be a factual question with a checkable
answer.

**`MetadataOnlyOutcome.oa_url` loses most of its values.** It never held a
usable OA URL for these works — it held a link the publisher had scoped
elsewhere. A field that is honestly empty is better than one that is
confidently wrong, and it is the same trade #472 was about.

**The user-visible gap #517 opened with remains open**, and this ADR does not
close it: on a subscribing network, a closed DOI still exits 0 saying there is
no OA copy. What can be improved without touching the ceiling is the *reporting*
— saying which sources were consulted, which were off, and that a TDM credential
is the route. That is #505, and it is now the whole remaining answer to #517.

**If a publisher ever does register an `unspecified` link**, the filter admits it
and the ceiling already covers it under §2a(a). Nothing further is needed; this
ADR is a statement about what Crossref returns, not a prohibition on the value.
