+++
title = "Sources matrix"
description = "| Source | Tier | Phase | Auth | ToS link | doiget feature |"
weight = 200
+++

# Sources

> **Status: NORMATIVE (user responsibility advisory).** This document lists every source
> doiget integrates with, the access prerequisites, and a pointer to each source's
> official Terms of Service. **Users are responsible for ensuring they have the right
> to access content via these sources and for compliance with each source's ToS.**

---

## 1. Source matrix

| Source | Tier | Phase | Auth | ToS link | doiget feature |
|---|---|---|---|---|---|
| Crossref | 1 (OA) | 1 | none | <https://www.crossref.org/documentation/retrieve-metadata/rest-api/> | always-on |
| Unpaywall | 1 (OA) | 1 | email (polite pool) | <https://unpaywall.org/products/api> | always-on |
| arXiv | 1 (OA) | 1 | none | <https://info.arxiv.org/help/api/index.html> | always-on |
| ar5iv (full text) | 1 (OA) | 4 (PR4) | none | <https://ar5iv.labs.arxiv.org/> | always-on |
| OpenAlex | 2 (metadata) | 4 | none | <https://help.openalex.org/how-to/> | `--features metadata` + `DOIGET_ENABLE_OPENALEX` |
| Semantic Scholar | 2 (metadata) | 4 | API key (optional) | <https://www.semanticscholar.org/product/api> | `--features metadata` + `DOIGET_ENABLE_S2` |
| DOAJ | 2 (metadata) | 4 | none | <https://doaj.org/terms/> | `--features metadata` + `DOIGET_ENABLE_DOAJ` |
| DataCite | 2 (resolution) | 4 | none | <https://datacite.org/terms-and-conditions/> | `--features metadata` + `DOIGET_ENABLE_DATACITE` |
| HAL | 2 (metadata) | 4 | none | <https://api.archives-ouvertes.fr/docs> | `--features metadata` + `DOIGET_ENABLE_HAL` |
| OpenAIRE | 2 (metadata) | 4 | none | <https://graph.openaire.eu/docs/> | `--features metadata` + `DOIGET_ENABLE_OPENAIRE` |
| CORE | 2 (metadata) | 4 | free key (`DOIGET_CORE_API_KEY`); unregistered use is token-tiered, see §6.1 | <https://core.ac.uk/terms> | `--features metadata` + `DOIGET_ENABLE_CORE` |
| Europe PMC | 2 (metadata) | 4 | none | <https://europepmc.org/About> | `--features metadata` + `DOIGET_ENABLE_EUROPE_PMC` |
| Springer Nature OA | 3 (institutional) | 5a | API key | <https://dev.springernature.com/terms-conditions/> | `--features tdm-springer` + key + agree |
| APS Harvest TDM (**serves PDFs**) | 3 (institutional) | 5b | API key | <https://harvest.aps.org/> | `--features tdm-aps` + key + agree |
| Elsevier ScienceDirect TDM | 3 (institutional) | 5c | API key | <https://www.elsevier.com/about/policies-and-standards/text-and-data-mining> | `--features tdm-elsevier` + key + agree |
| IEEE Xplore TDM (**unverified**) | 3 (institutional) | 5d | API key | <https://developer.ieee.org/> | `--features tdm-ieee` + key + agree |

The ceiling on all of this — what doiget may attempt for a given ref, and what bounds
it — is [`LEGAL.md`](LEGAL.md) §2a. A change that lets doiget try a location it could
not try before amends that section in the same PR (#497, ADR-0048).

### 1.1 When Tier 3 is consulted (ADR-0044)

Tier-3 sources are asked two different questions at two different points, and the
distinction is what #458 was about:

| point | question | trigger |
|---|---|---|
| metadata stage | "who can tell me about this DOI?" | Crossref found nothing |
| **content stage** | "who will give me the bytes?" | the OA content leg was **blocked** |

The second is the one a TDM agreement is obtained for. Until ADR-0044 it did not
exist, so for a publisher-registered DOI — which Crossref resolves readily — an
enabled TDM source was never consulted at all and enabling it changed nothing
observable.

**`tdm-aps` returns PDF bytes** at the content stage; APS documents single-request
retrieval with `Accept: application/pdf`. The stored file reports
`license = "unknown"`: it came from the publisher under your agreement, not from the
OA location whose licence Unpaywall reported, and doiget does not guess licences.

`tdm-elsevier`, `tdm-springer` and `tdm-ieee` remain metadata-only — but for three
different reasons. This paragraph named them in one sentence with only Elsevier's
reason attached, so the weaker cases inherited the stronger one's justification
(#496):

| source | why it is metadata-only |
|---|---|
| `tdm-elsevier` | **The vendor's policy.** Retrieval of non-open-access article PDFs through the ScienceDirect APIs is not permitted; a non-OA article yields a first-page preview rather than the article. Full-text **XML** is what an entitlement grants, which is the ADR-0032 `paper_text` route rather than the content leg. |
| `tdm-springer` | **doiget's choice.** Springer publishes a Full Text (TDM) API and an Open Access API alongside the Meta API, so full text is contractually available. Staying metadata-only is a conservative decision here, not a restriction. |
| `tdm-ieee` | **The contract is not public.** Shipped against an inferred one (ADR-0042), so the narrower surface is the honest one until it can be checked. |

Disclosure is bounded exactly as before: a source is only ever told about DOIs its
own publisher registered (ADR-0041). What rises with ADR-0044 is how often it is
asked, not what it is told.

## 2. User responsibility

For each source the user invokes, the user is the contracting party. doiget does not
hold any credential for any user. Before enabling a source, ensure that you:

- Have read and accepted the source's Terms of Service.
- Hold the institutional or personal access rights the source requires.
- Comply with the source's politeness policy (rate limit, attribution).
- Are operating from a network and device authorized to use those rights.

doiget enforces a hard rate cap of **5 fetches per second** per process to make polite
behavior the default ([`LEGAL.md`](LEGAL.md) §6a safeguard 5), tightened per source
where a vendor publishes something stricter (§6.1 below, ADR-0045).

This cited "§6 safeguard 8" until #496. Safeguard 8 is marketing-language
self-policing, and it lives in §6b — *policy commitments*, the ones a contributor
could violate without CI catching it. The rate cap is §6a.5, an *enforced control*.
The citation sent a reader looking for the enforcement basis to the section that has
none.

## 3. Default release binaries

`cargo install doiget` (default) compiles Tier 1 only. The optional source surface
requires an opt-in build:

```sh
cargo install doiget --features metadata
```

**What `metadata` means (ADR-0040, NORMATIVE).** The feature name predates the sources
it now carries. As of 0.8.8 it gates **the optional non-Tier-1 source surface as a
whole** — enrichment, resolution *and* retrieval — not enrichment alone. Compiling it
in makes that code present; it does **not** turn any source on. Every source under it
is additionally gated at runtime by its own `DOIGET_ENABLE_<NAME>`, and with every such
variable unset the observable behaviour of the binary is identical to a Tier-1-only
build. The runtime flag is the boundary that matters; the Cargo feature only decides
what is compiled.

Published release binaries build `--no-default-features --features oa-only,citation`,
and `citation = ["metadata"]`, so this code ships — inert — in the binaries you
download.

Tier 3 TDM sources are individually feature-flagged and require user-driven build:

```sh
cargo install doiget --features metadata,tdm-springer
cargo install doiget --features metadata,tdm-aps
cargo install doiget --features metadata,tdm-elsevier
cargo install doiget --features metadata,tdm-ieee
```

There is no `tdm-all` umbrella feature ([`SCOPE.md`](SCOPE.md) §non-goal 12).

## 4. Source-specific notes

### Crossref

- Public, no-auth API. Polite pool requires `User-Agent` with contact email
  (`[network] user_agent` in `config.toml`).
- doiget uses Crossref for: DOI → metadata; OA URL where Crossref's `link` array
  contains a free-to-read entry.

### Unpaywall

- Free, but the polite pool requires `email=alice@example.org` in the URL. Set
  `[network] unpaywall_email` in `config.toml`.
- doiget uses Unpaywall for: OA URL discovery for a given DOI, with license metadata.

### arXiv

- Public, no-auth API. Its [Terms of Use](https://info.arxiv.org/help/api/tou.html)
  cap requests at **one every three seconds, over a single connection**, collectively
  across every machine under the caller's control.
- doiget applies that per-source, via `SOURCE_RATE_OVERRIDES` (ADR-0045). One arXiv
  *attempt* issues two *requests* — the Atom feed, then the PDF — and both are paced,
  because the guideline counts requests.
- This page previously claimed the global 5/sec cap "respects this". It did not: the
  effective rate was 15x the guideline and the concurrency 5x (#493).
- doiget uses arXiv for: arXiv id → PDF + metadata. The parsed metadata
  also carries the **published DOI** and **journal reference** when the
  submitter supplied them (`<arxiv:doi>` / `<arxiv:journal_ref>`) — the
  arXiv → published-DOI link (#281 item 5). These ride the **raw metadata
  payload**, so they surface via the MCP tools **`doiget_metadata_only`** /
  **`doiget_resolve_paper`** (which return that payload verbatim). They are
  NOT written to the shared store, so
  `doiget info` (which reads the stored `Metadata`) does not show them: the
  store write forces the arXiv entry's own `doi` to `None` and has no
  `journal_ref` field. Omitted when absent.

### ar5iv (full-text extraction)

- ar5iv (`ar5iv.labs.arxiv.org`) renders arXiv papers as LaTeXML XHTML.
  doiget's `paper_text` / `doiget text` extracts sectioned plain text from
  it (the #281 "read" step; ADR-0032). **Tier 1 OA, always-on** — ships in
  the default `oa-only` binary, no env gate.
- It is registered under a **distinct `"ar5iv"` source key** (not
  `"arxiv"`) so provenance distinguishes full-text HTML from the arXiv
  PDF/Atom API. The host is a `*.arxiv.org` subdomain, so it adds no new
  registrable domain to the network surface
  (`http::fulltext_allowlist()`).
- **Never opens the PDF blob** (ADR-0032 D1): this is a *separate* fetch of
  the publisher's HTML rendering, not PDF content processing (permanent
  non-goal #1 stays intact). Extracted text is cached at
  `<cache_root>/text/<safekey>.json` (`docs/CACHE.md`), not the shared
  store.

### OpenAlex / Semantic Scholar / DOAJ

- Metadata enrichment only. doiget does not fetch PDFs from these unless the response
  includes an OA URL whose host is on the per-source allowlist.
- **OpenAlex has two distinct call paths with two tiers** (ADR-0031 D4):
  the matrix row above (Tier 2) covers the *enrichment* / citation-`graph`
  source (`/works/doi:…`, `referenced_works[]`), gated by `--features
  citation` + `DOIGET_ENABLE_OPENALEX`. The separate **discovery search**
  path (`doiget search`, `/works?search=`, `doiget_core::discovery`) is
  **Tier 1 OA metadata, always-on**: it ships in the default `oa-only`
  binary and needs **no** env-var gate. It is read-only, metadata-only,
  never paywalled, and never fetches a PDF.

### TDM sources

Each requires:

1. A Cargo feature compiled in (`tdm-elsevier`, `tdm-aps`, `tdm-springer`, `tdm-ieee`).
2. The user's API key in `DOIGET_KEY_<PUBLISHER>` env or `[tdm.<publisher>] api_key` in
   credentials.toml.
3. The agreement env `DOIGET_AGREE_TDM_<PUBLISHER>=1`.

If any of the three is missing, the source is unavailable at runtime
([`CAPABILITY.md`](CAPABILITY.md) §2).

**Scoped to the publisher's own DOIs.** A TDM source is consulted only for DOI
prefixes its publisher registered (ADR-0041). Enabling `tdm-aps` does not send
your Elsevier lookups to APS.

| feature | DOI prefixes consulted |
|---|---|
| `tdm-aps` | `10.1103` |
| `tdm-elsevier` | `10.1016`, `10.1006`, `10.1053` |
| `tdm-springer` | `10.1007`, `10.1038`, `10.1057`, `10.1140` |
| `tdm-ieee` | `10.1109`, `10.23919` |

The lists are deliberately conservative, so a publisher may own a prefix not
listed. That is visible rather than silent: the fetch error names it, as
`not consulted (DOI prefix 10.xxxx is not <publisher>)`.

#### IEEE Xplore — an inferred contract (#430)

`tdm-ieee` is the one Tier-3 source whose upstream contract has **not** been
confirmed against a live programme key. IEEE's programme requires registration
and a project summary before a key is issued, so the endpoint
(`https://ieeexploreapi.ieee.org/api/v1/search/articles`), the auth shape (the
key as an `apikey` **query parameter**, as with Springer rather than the
`X-API-Key` header APS and Elsevier use) and the response envelope
(`{ total_records, total_searched, articles: [...] }`) are taken from IEEE's
public developer portal and SDKs.

One unauthenticated request on 2026-08-24 (#460) **confirmed the endpoint** — the
host resolves and `/api/v1/search/articles` is served — and corrected the assumed
failure shape: an unauthorised caller gets `403` with `content-type: text/xml` and
a body of `<h1>Developer Inactive</h1>`, not JSON. So `(unverified)` in §1 now means
specifically **the 200-response envelope and the rate limits**.

The failure mode is loud, not silent: a 403 surfaces with its status and the key
redacted out of the URL, and a 200 in any other shape is a schema error naming the
missing field and quoting the body — so the first run against a real key reports the
actual contract. `DOIGET_IEEE_BASE` replays that response against a fixture. **Do not
drop the "unverified" marking in §1 until a 200 with a real key has been observed.**

Rate limits are likewise unknown; the source is subject to the same hard-coded
limiter as every other, which may be more or less polite than IEEE requires.

**When they run.** Strictly after Crossref, and only when Crossref produced
nothing — the same rule the Tier-2 chain follows, so enabling a TDM source can
never change a resolution that already works. Within that step, TDM runs before
the Tier-2 OA aggregators: for a DOI its publisher registered, the publisher's
own API is the authoritative record.

Every outcome, consulted or not, is recorded in the attempt trace attached to a
failed fetch, so "asked and had nothing" is always distinguishable from "never
asked" and from "wrong publisher".

**Pointing them elsewhere.** `DOIGET_APS_BASE`, `DOIGET_ELSEVIER_BASE`,
`DOIGET_SPRINGER_BASE` and `DOIGET_IEEE_BASE` override the API base, mirroring `DOIGET_CROSSREF_BASE`.
Intended for tests and for institutional proxies.

### When the publisher refuses the content

The OA chain already tries every location Unpaywall returned, advancing past
each failure — a 429 on one host does not stop it. What it could not do was
look beyond that list: when Crossref resolved the DOI, the optional sources
were skipped entirely, so a rate limit on the single publisher URL ended a run
with other indexes switched on (#445).

If the content leg is still blocked after the arXiv preprint fallback
(#325), doiget now asks the **enabled** optional sources whether anyone else
holds a copy, and tries the document URL they report. Three of them publish
one: CORE (`downloadUrl`), HAL (`fileMain_s`, gated on `openAccess_bool`) and
Europe PMC (`fullTextUrlList`). OpenAIRE and DataCite report a DOI resolver or
a landing page rather than a file, so they contribute no URL — their outcome
still appears in the attempt trace.

The fetch itself stays on the `oa-publisher` leg, with its allowlist and its
ADR-0023 denial context, exactly as each source's own docs describe.

This costs a request only when the content leg has **already** failed and the
user has switched a source on. With no flags set, behaviour is unchanged.

## 5. Adding a new source

A new source addition requires:

1. A new GitHub Discussion describing the source, its access pattern, and Tier
   classification.
2. An ADR locking the Tier and (if Tier 3) the Cargo feature name.
3. An entry in this document with the official ToS link and prerequisites.
4. A doc in `INTEGRATION/<source>.md` if user-side configuration is non-trivial.
5. Update of this matrix.

## 6. Politeness defaults

doiget's defaults are designed to be on the polite side of every source we know of:

- 5 fetches per second, regardless of source.
- Per-source backoff of 200 ms between consecutive requests.
- `User-Agent: doiget/<version> (+https://github.com/QAtlasHub/doiget)`.
- Honors `Retry-After` headers (treats 429 as `RATE_LIMITED` with the indicated wait).

If a source publishes a stricter rate guideline, doiget adopts the stricter value at
the per-source level rather than relaxing the global cap. This is now mechanical rather
than a promise: `RateLimits::backoff_ms_for` and `max_concurrent_for` take the stricter
of the global setting and the source's entry in `SOURCE_RATE_OVERRIDES`, so an entry can
only ever tighten (ADR-0045).

### 6.1 What each vendor publishes, and what doiget does about it

Recorded so the paragraph above is auditable rather than aspirational (#496). A row
with no `SOURCE_RATE_OVERRIDES` entry runs on the global cap, and the "doiget" column
says so rather than leaving it to be inferred.

| source | vendor's published limit | doiget | source of the figure |
|---|---|---|---|
| arXiv | 1 request / 3 s, single connection | **enforced per-source** (`SOURCE_RATE_OVERRIDES`) | <https://info.arxiv.org/help/api/tou.html> |
| CORE | token-cost model: 100 tokens/day and 10/min unregistered, 1 000/day and 25/min registered; a complex query costs 3–5 tokens | global cap; the key is what raises the tier | <https://api.core.ac.uk/docs/v3> |
| OpenAlex | daily and per-second limits, plus a polite pool keyed on `mailto` | global cap; doiget sends `mailto` when a contact email is set | <https://help.openalex.org/how-to/> |
| Springer | a rate-limits page, tiered Basic / Premium | global cap | <https://dev.springernature.com/docs/rate-limit-details/rate-limits/> — **figures not recorded**, see below |
| APS | none found | global cap | <https://harvest.aps.org/docs/harvest-api> |
| Unpaywall, Crossref, DataCite, DOAJ, HAL, OpenAIRE, Europe PMC, Semantic Scholar, IEEE | none found, or not yet checked | global cap | — |

The Springer figures are **absent rather than guessed**: that page renders its body
through JavaScript and the audit could not extract the numbers. A plausible-looking
number here would be worse than the gap, because the point of the table is that it can
be checked against the vendor. Someone should open it in a browser and fill the cell in.

CORE's row is why the §1 matrix no longer calls its key "optional". Requests do resolve
without one, at roughly a hundred simple queries a day — so a run that worked yesterday
can stop working today, and "optional" gives the reader nothing to diagnose that with.
