# 0061 - A PubMed id resolves to its DOI; it is not a new store identity

- **Date:** 2026-09-29
- **Status:** Accepted
- **Supersedes:** -
- **Amends:** [`docs/LEGAL.md`](../LEGAL.md) §2 (the hosts a default build can contact), per [0014](0014-docs-class-system.md); [`docs/REDIRECT_ALLOWLIST.md`](../REDIRECT_ALLOWLIST.md) §3
- **Builds on:** [0030](0030-bibliography-input-adapters.md) D3 (identifier priority `doi` > `arxiv` > `pmid`), [0046](0046-vendor-claims-are-normative-links-are-pointers.md) (vendor claims verified against the vendor), [0060](0060-retire-bibliofetch-coexistence.md) (safekey is doiget's own)
- **Source:** #500 (and #576, its reporting half)

## Context

A bibliography exported from PubMed identifies entries by PMID, often with no
DOI field. doiget reported such an entry, since #576, as "identified only by
PMID, which doiget cannot resolve yet", and did nothing more. #500 proposed a
`Ref::Pmid` / `Ref::Pmcid` variant with its own safekey class, and was blocked
on the safekey contract shared with BiblioFetch.jl; ADR-0060 retired that
contract.

A new `Ref` variant is still the wrong shape. The store, safekey, canonical
digest and every fetch source are keyed on the DOI or arXiv id; 37 non-test
files match on `Ref`. And the papers PubMed indexes overwhelmingly have DOIs,
which PubMed lists in each record's `articleids`. The DOI is the identity; a
PubMed id is another name for it.

## Decision

**D1: A PMID or PMCID is an alias, resolved to the DOI PubMed lists for it.**
One NCBI E-utilities `esummary.fcgi` request (`db=pubmed` for a PMID, `db=pmc`
for a PMCID, `retmode=json`), taking the `articleids[]` entry with
`idtype == "doi"`. From there the existing chain runs unchanged, and the paper
is stored under its DOI. No `Ref` variant, safekey class or store change.
Checked against the live API on 2026-09-29: the DOI is present for both
databases, and an unknown id answers `result.<uid>.error`.

**D2: Where it applies.** `doiget fetch` and `doiget cite` accept `pmid:N`,
`pmcid:PMCN` / `PMCN`, and PubMed / PMC article URLs; bare digits are refused
(too easily something else). `batch`, `verify`, `missing` and
`doiget_batch_from_bibliography` resolve the PMID / PMCID entries a
bibliography carries. Other single-ref commands say which commands take a
PubMed id. `--dry-run` and `--offline` make no request, so they do not resolve
one, and say so.

**D3: Nothing is guessed when there is no DOI.** A record with no DOI is
reported as such (`NOT_IMPLEMENTED`: the record is real, doiget reaches it only
through a DOI); an id PubMed has no record of is `NOT_FOUND` (`verify` calls it
absent). A PubMed title search for a DOI is not attempted.

**D4: NCBI's terms are a rate override, not a politeness setting.** NCBI's
E-utilities guidance (NBK25497) allows 3 requests a second without an API key,
below doiget's global cap of 5, so `ncbi` gets a `SOURCE_RATE_OVERRIDES` entry
(334 ms apart, one at a time). Requests carry `tool=doiget` and, when one is
configured, the contact email NCBI asks for. doiget sends no API key and never
assumes the keyed rate of 10 a second. The documentation page now serves
scripted clients a CAPTCHA; the figures were verified for #500 against the page
itself and are cited from there.

**D5: A host of its own, outside the fetch plan.** `eutils.ncbi.nlm.nih.gov`
has its own allowlist (`pubmed_allowlist`, source key `ncbi`), registered by the
CLI and the MCP server and kept out of the Tier 1 list a fetch plan is read
from: it answers which DOI, never with content. Each lookup is a `resolve`
row in the provenance log with the PubMed id as its `ref`.

## Consequences

- A PubMed-exported bibliography now resolves wherever its records list a
  DOI; the remainder is named, entry by entry.
- A paper with no DOI at all cannot enter the store from a PubMed id. That is
  a real gap for older MEDLINE records; closing it would need a store identity
  other than the DOI, which is a further decision.
- PMC full text still arrives through Europe PMC (#415) once the DOI is known.
