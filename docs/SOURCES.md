# Sources

> **Status: NORMATIVE (user responsibility advisory).** This document lists every source
> doiget integrates with, the access prerequisites, and a pointer to each source's
> official Terms of Service. **Users are responsible for ensuring they have the right
> to access content via these sources and for compliance with each source's ToS.**

---

## 1. Source matrix

| Source | Tier | Phase | Auth | ToS link | doiget feature |
|---|---|---|---|---|---|
| Crossref | 1 (OA) | 1 | none | <https://www.crossref.org/services/metadata-retrieval/rest-api/> | always-on |
| Unpaywall | 1 (OA) | 1 | email (polite pool) | <https://unpaywall.org/products/api> | always-on |
| arXiv | 1 (OA) | 1 | none | <https://info.arxiv.org/help/api/index.html> | always-on |
| ar5iv (full text) | 1 (OA) | 4 (PR4) | none | <https://ar5iv.labs.arxiv.org/> | always-on |
| OpenAlex | 2 (metadata) | 4 | none | <https://docs.openalex.org/how-to-use-the-api/api-overview> | `--features metadata` + `DOIGET_ENABLE_OPENALEX` |
| Semantic Scholar | 2 (metadata) | 4 | API key (optional) | <https://www.semanticscholar.org/product/api> | `--features metadata` + `DOIGET_ENABLE_S2` |
| DOAJ | 2 (metadata) | 4 | none | <https://www.doaj.org/api> | `--features metadata` + `DOIGET_ENABLE_DOAJ` |
| DataCite | 2 (resolution) | 4 | none | <https://datacite.org/terms-and-conditions/> | `--features metadata` + `DOIGET_ENABLE_DATACITE` |
| HAL | 2 (metadata) | 4 | none | <https://api.archives-ouvertes.fr/docs> | `--features metadata` + `DOIGET_ENABLE_HAL` |
| OpenAIRE | 2 (metadata) | 4 | none | <https://graph.openaire.eu/docs/> | `--features metadata` + `DOIGET_ENABLE_OPENAIRE` |
| Springer Nature OA | 3 (institutional) | 5a | API key | <https://dev.springernature.com/> | `--features tdm-springer` + key + agree |
| APS Harvest TDM | 3 (institutional) | 5b | API key | <https://harvest.aps.org/> | `--features tdm-aps` + key + agree |
| Elsevier ScienceDirect TDM | 3 (institutional) | 5c | API key | <https://www.elsevier.com/legal/tdmrep> | `--features tdm-elsevier` + key + agree |

## 2. User responsibility

For each source the user invokes, the user is the contracting party. doiget does not
hold any credential for any user. Before enabling a source, ensure that you:

- Have read and accepted the source's Terms of Service.
- Hold the institutional or personal access rights the source requires.
- Comply with the source's politeness policy (rate limit, attribution).
- Are operating from a network and device authorized to use those rights.

doiget enforces a hard rate cap of **5 fetches per second** per process to make polite
behavior the default ([`LEGAL.md`](LEGAL.md) §6 safeguard 8).

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

- Public, no-auth API, but the API has a 3-second-per-request rate guideline. doiget's
  global 5/sec cap respects this.
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

1. A Cargo feature compiled in (`tdm-elsevier`, `tdm-aps`, `tdm-springer`).
2. The user's API key in `DOIGET_KEY_<PUBLISHER>` env or `[tdm.<publisher>] api_key` in
   credentials.toml.
3. The agreement env `DOIGET_AGREE_TDM_<PUBLISHER>=1`.

If any of the three is missing, the source is unavailable at runtime
([`CAPABILITY.md`](CAPABILITY.md) §2).

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

If a source publishes a stricter rate guideline, doiget will adopt the stricter value at
the per-source level rather than relax the global cap.
