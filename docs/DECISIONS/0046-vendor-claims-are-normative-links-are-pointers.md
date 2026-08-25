# 0046 - In SOURCES.md, a claim about a vendor is normative; the URL where the vendor says it is a pointer

- **Date:** 2026-08-26
- **Status:** Accepted
- **Supersedes:** -
- **Refines:** [0014](0014-docs-class-system.md) — which requires an ADR for changes to NORMATIVE content, without saying whether a URL is content
- **Source:** #495 (five of sixteen ToS links dead or wrong), #496 (CORE, Springer, rate limits, a cross-reference pointing at the wrong safeguard), #498 (the 2026-08-25 vendor-terms audit)

## Context

`docs/SOURCES.md` is `Status: NORMATIVE (user responsibility advisory)`. A
2026-08-25 audit requested all sixteen links in its §1 matrix and read the prose
against each vendor's current documentation. It found two different kinds of
defect, and they do not want the same treatment.

**Wrong claims.** The document asserted things about vendors that were false:

- CORE's key was called "optional". Requests do resolve without one, at roughly a
  hundred simple queries a day under a token-cost model — so a run stops working
  and "optional" gives the reader nothing to diagnose it with.
- `tdm-elsevier`, `tdm-springer` and `tdm-ieee` were named in one sentence as
  metadata-only, with only Elsevier's reason attached. Springer publishes a Full
  Text (TDM) API and an Open Access API; staying metadata-only there is doiget's
  conservative choice, not a vendor restriction. The weaker case inherited the
  stronger one's justification.
- §6 promised doiget adopts a stricter vendor guideline per source, while
  recording no vendor's published limit anywhere — so the promise could not be
  checked against anything. (arXiv's, the one measured violation, is ADR-0045.)
- The rate cap cited "LEGAL.md §6 safeguard 8". Safeguard 8 is marketing-language
  self-policing in §6b, *policy commitments*. The cap is §6a.5, an *enforced
  control*. The citation sent a reader looking for the enforcement basis to the
  section that has none — inverting the meaning of the reference.

**Rotted pointers.** Five links no longer led to terms:

| row | was | result | now |
|---|---|---|---|
| Crossref | `crossref.org/services/metadata-retrieval/rest-api/` | 404 | `crossref.org/documentation/retrieve-metadata/rest-api/` |
| Elsevier | `elsevier.com/legal/tdmrep` | 404 | `elsevier.com/about/policies-and-standards/text-and-data-mining` |
| OpenAlex | `docs.openalex.org/how-to-use-the-api/api-overview` | 200 → redirects to the `help.openalex.org` root; deep target gone | `help.openalex.org/how-to/` |
| DOAJ | `doaj.org/api` | 200 → redirects to `swagger.json`, a schema | `doaj.org/terms/` |
| Springer | `dev.springernature.com/` | 200, portal root | `dev.springernature.com/terms-conditions/` |

The Elsevier one was the worst even before it died: `tdmrep` is the **TDM
Reservation Protocol**, the W3C-track standard by which a rightsholder
machine-readably signals an opt-out *from* text and data mining. It is not
Elsevier's API terms. The sole ToS reference for a Tier-3 source pointed at the
wrong kind of document.

## Decision

**D1 — A claim about a vendor is NORMATIVE content. Changing one needs an ADR.**

What a source requires, permits and limits is exactly what a user relies on this
document for. This ADR is the record for the four corrections above.

**D2 — Vendor-published limits are recorded in `SOURCES.md` §6.1, or their absence is.**

§6's promise is only meaningful next to what the vendors actually publish. §6.1 now
carries a row per source: the vendor's limit, what doiget does about it, and where
the figure came from. A source running on the global cap says so, rather than leaving
it to be inferred from silence.

**A figure that could not be read is left blank and labelled.** Springer's
rate-limit page renders its body through JavaScript and the audit could not extract
the numbers, so the cell says so. A plausible-looking number would be worse than the
gap, because the entire value of the table is that a reader can check it against the
vendor.

**D3 — A URL is a pointer, not a claim. Correcting one does not need an ADR.**

Repointing a link at the same document under its new address changes nothing the
document asserts. Requiring an ADR per publisher site reorganisation would produce
ADRs that record no decision, and — worse — would make the correction expensive
enough to defer, which is how five accumulated.

The boundary: if the replacement is *the same document at a new address*, it is a
pointer fix. If it is a *different document* — as the Elsevier row was, moving from a
wrongly cited opt-out protocol to the actual API policy — then the claim about where the
vendor states its terms changed, and that is D1.

**D4 — Rot is made visible on a schedule, not per PR.**

`.github/workflows/tos-links.yml` requests every §1 link monthly and opens an issue
on any non-200. Deliberately **not** a PR check: a publisher reorganising their site
overnight would turn an unrelated PR red, which is the wrong failure mode and the one
that teaches people to ignore a check.

It follows redirects and reports the final URL, so a link that still returns 200 while
landing somewhere unrelated is visible. It fails loudly when it extracts **zero**
URLs, because a table reformat that emptied the list would otherwise be
indistinguishable from a clean sweep — the same "silently checking nothing" shape as
#442 and #454.

## Consequences

**Positive.** §6's promise is auditable. The Springer row no longer implies a vendor
restriction that does not exist. A reader chasing the rate cap's enforcement basis
lands on an enforced control. Link rot surfaces within a month instead of at the next
audit.

**Negative.** §6.1 is a hand-maintained table of facts held by other people, and it
will drift. Nothing checks the *numbers* — only that the URLs resolve. The scheduled
job is the floor, not a guarantee, and the honest position is that a vendor can change
a limit silently and this document will be wrong until someone looks.

**Also negative.** Monthly outbound requests to sixteen third parties who did not ask
for them. Monthly rather than weekly is the concession: publisher sites move on a
scale of months.

## Alternatives rejected

- **An ADR per link fix.** Records no decision, and the cost is what let five
  accumulate.
- **Per-PR link checking.** Wrong failure mode; see D4.
- **Guessing Springer's numbers** from the tier names. The table's only value is
  being checkable, and a plausible wrong number destroys that more thoroughly than a
  blank does.
- **Dropping the ToS column** as unmaintainable. It is the mechanism by which
  LEGAL.md's "the user holds the rights" posture is actionable at all — without a
  link, "comply with the source's terms" is advice the user cannot follow.
