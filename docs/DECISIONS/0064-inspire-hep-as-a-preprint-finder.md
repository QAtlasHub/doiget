# 0064 - INSPIRE-HEP as a preprint finder

- **Date:** 2026-09-29
- **Status:** Accepted
- **Supersedes:** -
- **Amends:** [`docs/LEGAL.md`](../LEGAL.md) §2a (a-ii), per [0048](0048-access-ceiling.md); [0062](0062-find-a-dois-preprint.md) D1 (a fourth finder)
- **Builds on:** [0062](0062-find-a-dois-preprint.md), [0046](0046-vendor-claims-are-normative-links-are-pointers.md)
- **Source:** #642 (#474, Gap 2)

## Context

ADR-0062 finds a closed DOI's arXiv preprint through Crossref's relation, OpenAlex
(opt-in) and an arXiv title search. INSPIRE-HEP curates the journal-DOI <-> arXiv
link for high-energy physics, gravitation and related fields, which is more
reliable than a title search and holds when the arXiv title differs.

Primary source, <https://github.com/inspirehep/rest-api-doc> (read 2026-09-29):
base "https://inspirehep.net/api/"; lookup by DOI
"https://inspirehep.net/api/doi/10.1103/PhysRevLett.19.1264"; "every IP address is
allowed 15 requests in a 5s window. If you exceed those limits, you will receive a
response with HTTP status code 429"; read-only, no authentication; "most of the
metadata is available under a CC0 license, but restrictions apply to some fields,
and bulk collection of email addresses is not allowed". Live:
`api/doi/10.1103/PhysRevLett.116.061102` -> `metadata.arxiv_eprints[0].value`
`1602.03837`; an unknown DOI answers 404.

## Decision

**D1: A fourth finder, after OpenAlex and before the arXiv title search.** One
request to `inspirehep.net/api/doi/<doi>`, reading `metadata.arxiv_eprints[].value`
only. `found_by: inspire`.

**D2: Opt-in.** `DOIGET_ENABLE_INSPIRE` (`--features metadata`), because it is a new
host asked on closed DOIs. 334 ms apart, one at a time (15 per 5 s); a 429 is
`retry_after`.

**D3: `metadata.documents` is not followed.** Those are files hosted on
inspirehep.net whose provenance and licence are not stated per file, so they are
outside LEGAL §2a. Only the arXiv id is taken, and the preprint is fetched from
arXiv as #325 does.

## Consequences

- With INSPIRE enabled, a HEP / gr-qc DOI whose preprint Unpaywall and Crossref do
  not name is found without a title search.
- Not in the live suite: the suite asserts a default build, and INSPIRE is opt-in.
