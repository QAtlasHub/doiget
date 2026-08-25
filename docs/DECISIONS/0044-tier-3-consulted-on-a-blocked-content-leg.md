# 0044 - Tier-3 TDM sources are consulted on a blocked content leg, not on a Crossref miss

- **Date:** 2026-08-25
- **Status:** Accepted
- **Supersedes:** -
- **Amends:** [0041](0041-tdm-sources-scoped-to-publisher-prefixes.md) — its option-(c) rejection rests on a premise this decision removes
- **Source:** #458 (the chain never ran), #445 (the trigger it should have had), #484 (the endpoint it should have asked)

## Context

Tier 3 was consulted from one place: `resolve_tdm_chain`, which records `NotNeeded`
for every entry when Crossref answered.

That is the correct trigger for Tier 2. Those sources are *resolution* fallbacks —
they exist because Crossref sometimes has nothing, and once it has something they
are genuinely not needed.

Tier 3 was added for a different reason. #407 measured the corpus that motivated it
as **metadata-resolvable but not fetchable**. The metadata was never the problem.
So the two conditions were backwards:

| | what it needs | what it got |
|---|---|---|
| Tier 2 | run when Crossref found nothing | run when Crossref found nothing |
| Tier 3 | run when the **content leg** failed | run when Crossref found nothing |

Crossref resolves publisher-registered DOIs readily, so for exactly the DOIs these
sources exist to serve, the chain was skipped. A user could sign a publisher's TDM
agreement, obtain a key, build with the feature, and get output byte-identical to
having done none of it. That is #458, and it is the #442 defect class one level up
from wiring: every documented gate satisfied, nothing changed.

A second problem sat behind it. Even reached, all four Tier-3 sources were
metadata-only by construction (`FetchResult.pdf_bytes` is always `None`), and a
source that cannot return bytes cannot close a gap that is about bytes. #458 was
explicit that the two halves land together or not at all: half 1 alone buys a
duplicate metadata record, half 2 alone is unreachable code.

## Decision

**D1 — Tier 3 gains a second consultation point, triggered by a blocked content
leg.** It is not moved. `resolve_tdm_chain` still answers "who can tell me about
this DOI?" when Crossref could not; the new `try_tdm_content_fallback` answers "who
will give me the bytes?" after the open routes are exhausted:

```
OA PDF chain
  Blocked -> try_arxiv_preprint_fallback          (#325)
    Blocked -> try_optional_source_oa_fallback    (#445)
      Blocked -> try_tdm_content_fallback         (this ADR)
```

It is additive on the same terms as its two siblings: on any failure the **original**
block survives. The publisher's refusal on the open route is what the user has to
act on, and burying it under a second failure from the TDM endpoint would answer a
question they did not ask.

**D2 — `Source::fetch_content`, defaulted to `Ok(None)`.** The metadata-only
contract used to be expressed by each impl setting `pdf_bytes: None` and saying so
in a doc-comment. The orchestrator cannot read a doc-comment, so it could not
distinguish a source with nothing to offer from one it had never asked. The default
makes "metadata-only" a stated fact rather than an emergent one, and leaves the
other sources untouched.

Overriding implementations MUST use a PDF-validating fetch. `HttpClient` gains
`fetch_pdf_with_headers` for this: publisher error pages and WAF holding responses
are 200s with a body, and `fetch_bytes_with_headers` would write one to
`<safekey>.pdf`.

**D3 — APS first.** It is the only Tier-3 publisher whose full-text contract is
both public and PDF-shaped:

| publisher | what its API grants |
|---|---|
| **APS Harvest** | `GET /v2/journals/articles/{doi}` with `Accept: application/pdf`, one request, `X-API-Key`. Published example in the vendor's own documentation. |
| Elsevier | **Non-OA PDF retrieval via the APIs is not permitted**; a non-OA article yields a first-page *preview*. What an entitlement grants for subscription content is full-text XML. |
| IEEE | Contract not public — shipped against an inferred one ([ADR-0042](0042-tdm-ieee-inferred-contract.md)). |
| Springer | Unverified. |

Elsevier was the first choice and was rejected on that evidence. Storing a
first-page preview as `<safekey>.pdf` and reporting it as fetched would be a worse
version of the defect this ADR exists to close: not an absent feature, but wrong
data presented as right. Elsevier remains a good candidate for a later XML-shaped
instalment under [ADR-0032](0032-fulltext-html-extraction.md); it is the wrong
publisher for closing a gap about bytes.

**D4 — `PdfLegStatus::TdmFetched { source, original_block }`, distinct from
`Fetched`.** The bytes did not come from an OA host, are not necessarily openly
licensed, and were obtained under an agreement the user signed. Provenance reading
`oa-publisher` would be wrong in all three respects. The stored source label is
derived from the leg rather than from a two-valued "was this a preprint fallback"
flag, which would have labelled the publisher's copy `oa-publisher` by default.

**D5 — a TDM-retrieved artifact reports `license = "unknown"`.** The licence tracks
the artifact that landed, not the work — that is already how the arXiv fallback
behaves, overwriting the licence with the preprint's. Unpaywall's `cc-by` describes
an OA location that was never reached; carrying it forward would put an
open-licence claim on a file obtained under a signed agreement, by a route that
licence does not describe. doiget does not guess licences, so the honest answer is
`unknown`.

**D6 — prefix scoping is kept, and is now a choice rather than a forced move.**

ADR-0041 rejected option (c), routing by the publisher named in Crossref metadata,
because *"it only works when Crossref answered — and the TDM chain runs precisely
when Crossref did not, so the routing information would be missing exactly when it
is needed."*

**D1 removes that premise.** The new consultation point runs *after* Crossref
answered, so (c) is available here in a way it was not in ADR-0041.

It is still not taken. A constant is testable and does not make the publisher
contacted depend on a prior network response — the second half of ADR-0041's
reasoning, which survives intact. This is recorded explicitly so the next reader
does not re-derive the rejection from an argument that no longer holds.

The prefix is checked in the orchestrator **before** credentials, exactly as in
ADR-0041, so `WrongPublisher` and `Disabled` stay distinguishable in the trace.
`Source::can_serve` checks it too; the redundancy is the deliberate double gate, and
only the orchestrator check produces the distinction.

## Consequences

**Positive.**

- A TDM agreement now changes what the user gets. The three gates lead somewhere.
- The trace distinguishes "consulted for metadata and not needed" from "consulted
  for content and refused", which are different problems with different fixes.
- `fetch_content` gives future publishers a defined place to land, and its default
  states the metadata-only contract for the three that have not.

**Negative, and the one that needs stating plainly.**

**Disclosure scope is unchanged; frequency rises.** ADR-0041's argument was that a
publisher is only ever told about DOIs it registered, so the disclosure is nil —
resolving such a DOI goes through them anyway. That still holds exactly: prefix
scoping is enforced on the new path too.

What changes is how often. The old trigger was "Crossref missed", which for a
publisher-registered DOI is rare. The new trigger is "the content leg was blocked
for a DOI this publisher registered", which is common — it is the case the feature
exists for. A user with `tdm-aps` enabled will contact APS about a meaningful
fraction of the APS DOIs they fetch, rather than almost never.

That is the cost of the feature working at all, and it is bounded by the same
prefix rule. It is recorded here rather than left for someone to discover in a
packet capture.

**Also negative.** The one-shot content attempt has no retry and no caching, so a
transient failure at the TDM endpoint costs the fetch. Consistent with the OA chain,
which does the same, and preferable to retry loops against a credentialed endpoint.

## Alternatives rejected

- **Move the trigger instead of adding one.** Tier 3 would stop answering the
  metadata question it currently answers when Crossref is down, for no gain.
- **Half 1 alone** (reach Tier 3 on a blocked content leg, stay metadata-only). #458
  rejected it in advance: it buys a duplicate metadata record for a DOI Crossref
  already resolved.
- **Elsevier first.** See D3. Rejected on the vendor's own documented policy, not on
  preference.
- **Infer a licence for the TDM copy** from the publisher record. That is a guess
  about *this retrieval*, and the repo's standing rule is not to guess licences.
