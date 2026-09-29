# 0063 - Follow a closed DOI to its non-arXiv preprint through the preprint's own DOI

- **Date:** 2026-09-29
- **Status:** Accepted
- **Supersedes:** -
- **Amends:** [`docs/LEGAL.md`](../LEGAL.md) §2a (a-ii), per [0048](0048-access-ceiling.md); [0062](0062-find-a-dois-preprint.md) (whose Consequences left non-arXiv preprints for a further decision)
- **Builds on:** [0062](0062-find-a-dois-preprint.md), [0046](0046-vendor-claims-are-normative-links-are-pointers.md)
- **Source:** #640 (part 1 of #474, Gap 1)

## Context

ADR-0062 finds a closed DOI's **arXiv** preprint. Life-sciences papers post to
bioRxiv / medRxiv, Research Square, OSF and others instead, and a preprint's own
DOI already fetches today: `10.1101/2021.04.29.21256344` (medRxiv) -> Unpaywall
`green` -> fetched by `oa-publisher` (measured 2026-09-29). What was missing is
the step from the journal DOI to the preprint DOI.

Primary sources, checked 2026-09-29:
- bioRxiv's API (<https://api.biorxiv.org/>): `pubs/[server]/[DOI]/na/[format]`
  finds the preprint of a *published* DOI (`preprint_doi`, `preprint_platform`),
  for `biorxiv` and `medrxiv`. No rate limit is published.
- bioRxiv's About page (<https://www.biorxiv.org/about-biorxiv>): "bioRxiv
  provides free and unrestricted access to all articles posted on the server.
  We believe this should apply not only to human readers but also to machine
  analysis of the content."
- A constructed medRxiv PDF URL answers 403 to a scripted client, so the
  preprint is reached through its DOI's reported location, not a built URL.

## Decision

**D1: After ADR-0062's arXiv search comes a non-arXiv preprint DOI.** When the
content leg still holds nothing, `preprint::find_preprint_doi` tries:

1. **Crossref `relation.has-preprint` with a non-arXiv DOI** -- no request, from
   the record the fetch holds. In a live sample of 40 records with the relation,
   `10.1101` (bioRxiv/medRxiv) and `10.21203` (Research Square) were 10 each.
2. **bioRxiv / medRxiv `pubs`** -- `api.biorxiv.org`, `biorxiv` then `medrxiv`.
   Opt-in, `DOIGET_ENABLE_BIORXIV` (under `--features metadata`), because it is a
   new host asked on every closed DOI; one request a second at most, one at a
   time, since no limit is published.

**D2: The preprint is fetched through its own DOI.** Unpaywall is asked about the
preprint DOI and its reported OA locations are tried on the ordinary
`oa-publisher` allowlist -- LEGAL §2a (a), never a constructed URL.

**D3: The answer says what it is.** `PdfLegStatus::PreprintDoiFallback` (wire
`preprint_doi_fallback` -- its own status, so an agent tells it from an arXiv
`preprint_fallback` -- with `preprint_doi`, `platform`, `found_by`
`crossref_relation` / `biorxiv_pubs`, `original_block`); the stored entry carries
`preprint_doi`, and the licence is the preprint's.

## Consequences

- Measured live: `10.1111/1556-4029.14027` (Wiley, closed) now yields its bioRxiv
  preprint `10.1101/482166` in a default build.
- A closed DOI whose Crossref record names no preprint gains nothing unless
  bioRxiv `pubs` is enabled.
- OSF's and ChemRxiv's own APIs remain further sub-issues of #474.
