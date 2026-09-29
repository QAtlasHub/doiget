# 0059 - Entitled-network publisher sources: the shape, decided before any source

- **Date:** 2026-09-29
- **Status:** Accepted (design only; no source implemented)
- **Supersedes:** -
- **Builds on:** [0002](0002-tdm-feature-gated.md) (Tier-3 opt-in), [0027](0027-redirect-allowlist-oa-publisher-physics.md) and [0039](0039-publisher-hosts-stay-off-allowlist.md) (`oa-publisher` is for Open Access), [0041](0041-tdm-sources-scoped-to-publisher-prefixes.md) (a publisher source is scoped to its prefixes), [0052](0052-crossref-link-is-programme-scoped.md) (Crossref's `similarity-checking` link is not a route), [0055](0055-error-disposition.md), [0057](0057-repeat-suppression.md)
- **Source:** #517 ("change it": retrieve through routes the user is lawfully entitled to use, including the network's institutional entitlement), #593 (Cambridge Core book chapters, measured), #603 (APS, measured)

## Context

#517 decided that doiget may retrieve through the entitlement of the network it
runs on, and ordered the work: (1) a trace for the closed-DOI path (#505,
shipped), (2) record the network in provenance, (3) then a publisher attempt.
#593 and #603 each measured a first target from a subscribing university
network:

| | Cambridge Core book chapter (#593) | APS journal article (#603) |
|---|---|---|
| authentication | IP only; no cookie, no SSO | IP only; no cookie, no SSO |
| bot wall | none for a browser UA | none, even for `doiget/…` and `curl/…` |
| route | the chapter-PDF href on the landing page | the PDF href on the landing page (`/prb/pdf/<doi>`) |
| entitled response | 200 `application/pdf` | 200 `application/pdf` |
| unentitled response | 200 **HTML** (measured) | not measured |
| Crossref `link[]` | one `similarity-checking` URL, byte-identical PDF | `syndication` + `similarity-checking` |

The byte-identical `similarity-checking` link is the shortcut these
measurements invite. ADR-0052 already excludes it, and it stays excluded: the
landing-page href is the same bytes reached the way an entitled reader reaches
them, which is the route with a provenance story.

This cycle does not build a source. What it fixes is the shape every such
source must have, so the first one is not the one that decides it by accident.

## Decision

**D1: A capability of its own, with no key.** The credential is the network,
so the Tier-3 `key + agreement` pair does not transfer. An entitled-network
source is compiled in by its own Cargo feature and enabled by an explicit
opt-in, `DOIGET_ENTITLED_<VENDOR>=1`, plus a recorded entitlement note (for
example the institution's name) that goes into the provenance log. No setting
enables every vendor at once.

**D2: A separate allowlist key, never a widening of `oa-publisher`.** Even where
the host is already on `oa-publisher` (APS: `*.aps.org`, there for PRX and PR
Research), the subscription route registers its own key, for example
`entitled-aps`. A closed host in the OA list is how a subscription PDF ends up
stored as if it were free, and the provenance row must say *subscription via
the network*, not *OA publisher*.

**D3: The route is the landing page's href, read, not constructed.** Even when
the PDF URL is a function of the DOI (APS), the source fetches the landing
page and takes the href it serves, so a layout change fails loudly instead of
fetching the wrong thing. ADR-0052 still excludes Crossref's
`similarity-checking` link.

**D4: Entitlement is proven positively.** A response counts only with
`Content-Type: application/pdf` and the `%PDF-` magic. An unentitled network
gets 200 + HTML (measured for Cambridge Core), so "no error" is not proof.
APS's off-network shape must be measured before its source ships.

**D5: The store says what the file is.** A PDF present means "free to use"
everywhere today. An entitled-network PDF is stored with
`[doiget] access = "subscription"`, the publisher's terms URL, and the
entitlement note, so `bib`, `csl`, `text` and an agent can tell it from an OA
copy. This extends the `[doiget]` table the way `origin = "user-supplied"`
did (#606).

**D6: The terms are enforced by the source, not left to the user.** Reading
under a subscription is permitted; systematic download is not. The source
refuses what the terms forbid and says why on refusal:

- whole books: refuse a `monograph` DOI, serve chapters only (Cambridge Core);
- volume sweeps: cap how many items one `container-title` (a book, or a
  journal volume) yields per session. `batch` over a list of `10.1103` DOIs is
  the case this must answer.

The cap is a library constant, like the rate limit, and is not configurable
(LEGAL.md §6a). Repeat suppression (ADR-0057) applies unchanged.

**D7: Scope of each source.** Prefix-scoped as ADR-0041 requires, and
type-scoped: Cambridge Core `10.1017` + `book-chapter`; APS `10.1103` +
`journal-article`. Anything else under the prefix is `not_covered`.

**D8: Order.** Step (2) of #517, the network in provenance, lands before or
with the first source. Cambridge Core is the first target (its unentitled
shape is measured); APS follows once its off-network response is.

## Consequences

- #593 and #603 stay open as the implementation issues; this ADR is their
  design. Neither is implemented in 0.9.0.
- LEGAL.md needs a section on entitled-network retrieval when the first source
  lands, quoting each publisher's terms; the APS terms text still has to be
  found (#603).
- #603's note stands as an open check: whether `tdm-aps` can fire for a closed
  DOI at all, given that Crossref always answers for one (#458). If it cannot,
  the entitled-network route is the only one for `10.1103`.
