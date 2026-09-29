# 0065 - NASA ADS as a preprint finder, on the user's own token

- **Date:** 2026-09-29
- **Status:** Accepted
- **Supersedes:** -
- **Amends:** [`docs/LEGAL.md`](../LEGAL.md) §2a (a-ii), per [0048](0048-access-ceiling.md); [0062](0062-find-a-dois-preprint.md) D1 (a fifth finder)
- **Builds on:** [0062](0062-find-a-dois-preprint.md), [0064](0064-inspire-hep-as-a-preprint-finder.md), [0046](0046-vendor-claims-are-normative-links-are-pointers.md)
- **Source:** #644 (#474, Gap 2)

## Context

NASA ADS is the canonical bibliographic index for astronomy and much of physics,
and it holds a published DOI's arXiv id among the record's identifiers. Its web
pages answer a scripted client `202` with an empty body, so its terms were read from
ADS's own published sources (the `adsabs-dev-api` README and the `adsabs.github.io`
help pages), on 2026-09-29:

- "All API requests must pass your token in an `Authorization: Bearer <token>`
  HTTP header"; base `https://api.adsabs.harvard.edu/v1/`, `/search/query`;
  `X-RateLimit-Limit: 5000`, "the rate resetting happens at midnight UTC".
- Search syntax: "doi:DOI ... finds a specific record using its digital object id";
  "identifier ... finds a paper using any of its identifiers, arXiv, bibcode, doi".
- API terms: do not "circumvent the restrictions placed on the API client tokens";
  "you also shouldn't redistribute any data you obtain from our services to
  third-parties"; no "partial or full copy of the data ... by systematically
  downloading or crawling".

## Decision

**D1: A fifth finder, after INSPIRE and before the arXiv title search.** One
`/v1/search/query?q=doi:"<doi>"&fl=identifier&rows=1`, reading the `arXiv:<id>`
entry of the first record's `identifier`. `found_by: ads`.

**D2: The user's own token is the opt-in.** `DOIGET_ADS_TOKEN`, sent only as the
`Authorization: Bearer` header -- never in a URL, never in the provenance log,
never shipped. No token, no request. (`--features metadata`.)

**D3: Within the API terms.** One lookup per closed DOI the user asked to fetch; only
the arXiv id is used, for the user's own fetch, and nothing from ADS is stored or
redistributed; no ADS full-text link is followed (the PDF comes from arXiv, as #325
fetches it); one request a second, one at a time. A 429 is `retry_after`.

## Consequences

- With a token set, an astronomy / physics DOI whose preprint Unpaywall, Crossref and
  INSPIRE do not name is found without a title search.
- Not in the live suite: it needs a personal token, and the suite asserts a default
  build.
