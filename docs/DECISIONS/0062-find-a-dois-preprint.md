# 0062 - Find a DOI's arXiv preprint when Unpaywall does not name one

- **Date:** 2026-09-29
- **Status:** Accepted
- **Supersedes:** -
- **Amends:** [`docs/LEGAL.md`](../LEGAL.md) §2a (the access ceiling gains (a-ii)), per [0048](0048-access-ceiling.md)
- **Builds on:** [0048](0048-access-ceiling.md), [0052](0052-crossref-link-is-programme-scoped.md); #325 (the arXiv preprint fallback)
- **Source:** #636 (the gap the live suite, #462, measured on the maintainer's own paper)

## Context

The #325 fallback stores an arXiv preprint in place of a blocked publisher copy,
but only a preprint Unpaywall names. `10.1103/bbnt-brjz` (Shimozono & Hotta, PRB
2026) is closed at APS, Unpaywall knows no copy, and Crossref and OpenAlex list
only the journal location -- yet arXiv holds the preprint, 2512.07923. doiget
answered `no_oa_url` for a paper with a public preprint.

## Decision

**D1: When the content leg found nothing, look for the preprint.** On
`no_oa_url`, or a block with no Unpaywall arXiv hint, `preprint::find` tries, in
order, stopping at the first answer:

1. **Crossref `relation.has-preprint`** -- no request: it is in the record the
   fetch already holds. An `arxiv` id, or an arXiv DOI `10.48550/arXiv.<id>`
   (both forms measured in live records).
2. **OpenAlex `locations[]`** -- a location whose landing page is
   `arxiv.org/abs/<id>`. One request, only when `DOIGET_ENABLE_OPENALEX` is set:
   a default fetch contacts no host it did not before.
3. **arXiv's API search**, `ti:"<title>" AND au:<first-author surname>` -- one
   request, at arXiv's 3-second rate (#493). A hit counts only when its
   title equals the record's (letters and digits only, case-folded), the first
   author's surname is among its authors, and it names no different published
   DOI.

A found preprint is fetched exactly as #325 does -- arXiv's `/pdf/<id>`, stored
under the DOI's safekey, reported as `preprint_fallback`.

**D2: The answer says who found it.** `PdfLegStatus::PreprintFallback` gains
`found_by`: `unpaywall`, `crossref_relation`, `openalex_location` or
`arxiv_title_search`, on the MCP envelope and in the CLI line.

**D3: Strict matching over recall.** A wrong preprint stored under a DOI is worse
than none: every check in D1.3 must hold, and a short title (under 12 letters
and digits) or a record with no named first author is not searched at all.
Fuzzy matching, and preprint servers other than arXiv, are not attempted.

**D4: The access ceiling moves, and says so.** An arXiv id a source *reported* is
not the identifier the user typed, so LEGAL.md §2a gains (a-ii), naming exactly
these sources, and "How this changed" records it.

## Consequences

- A closed DOI with a public arXiv preprint now yields the preprint, and the
  envelope says it is a preprint and how it was found.
- A closed DOI with none costs one arXiv search more than before (plus an
  OpenAlex request if enabled), paced by arXiv's rate limit.
- Not covered: bioRxiv / medRxiv / Research Square preprints named by Crossref's
  relation. Their DOIs could be fetched through the ordinary OA route; that is a
  further decision.
