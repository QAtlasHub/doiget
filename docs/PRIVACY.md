# Privacy Policy

doiget is a local, single-binary CLI + stdio MCP server. It runs entirely on
your machine.

## What doiget collects

**Nothing.** doiget has no telemetry, no analytics, no crash reporting, and no
"phone home" of any kind ([ADR-0015](DECISIONS/0015-no-telemetry.md) /
[`SCOPE.md`](SCOPE.md) non-goal #10). It makes **no network connection that is
not the direct result of a paper you (or your agent) ask it to fetch**.

## What leaves your machine

doiget stores nothing about you, but **fulfilling a fetch sends the identifier
you request** — a DOI or arXiv id, and (only if you set it) your contact email —
to the upstream provider for that source. Nothing else leaves your machine:
doiget transmits no document content, saves PDFs and metadata **to your local
store**, and never redistributes them.

### Third-party services doiget contacts

Each request is governed by **that provider's own Terms of Service and privacy
policy**. doiget operates within those terms, and — because doiget holds no
credentials and runs locally — **you are the contracting party** for every
source you use (see [`LEGAL.md`](LEGAL.md) and [`SOURCES.md`](SOURCES.md) for the
full matrix and ToS links). doiget enforces a hard 5 requests/second cap to keep
its use of these APIs polite.

**Default build — Open Access, always on:**

- **Crossref** — DOI → metadata.
  <https://www.crossref.org/services/metadata-retrieval/rest-api/>
- **Unpaywall** — Open-Access PDF locations (the polite pool uses your email if
  you set it). <https://unpaywall.org/products/api>
- **arXiv** (export API + **ar5iv** for full text) — preprint metadata, PDF,
  text. <https://info.arxiv.org/help/api/index.html>
- **OpenAlex** — literature discovery, identity resolution, and citation graph
  (the search / frontier / link / citation tools).
  <https://docs.openalex.org/how-to-use-the-api/api-overview>

**Opt-in only — compile-time feature flag + your own configuration:**

- `--features metadata`: **Semantic Scholar**, **DOAJ** (extra metadata) and
  **DataCite** (<https://api.datacite.org>, DOI resolution for Zenodo / figshare /
  Dryad / OSF). Each is additionally inert until you set its own
  `DOIGET_ENABLE_<NAME>`, so compiling the feature in contacts nobody by itself.
  DataCite is queried by exact DOI only — never used as a search surface — and
  needs no key or account.
- `--features metadata`: **HAL** (<https://api.archives-ouvertes.fr>, the French
  national OA repository). Same shape: inert until `DOIGET_ENABLE_HAL` is set,
  queried by exact DOI through the `doiId_s` field only, no key or account.
- `--features metadata`: **OpenAIRE** (<https://api.openaire.eu>, European repository
  aggregation). Inert until `DOIGET_ENABLE_OPENAIRE` is set; queried by exact DOI
  through the Graph API v1 `pid` parameter only. No key or account.
- `--features metadata`: **CORE** (<https://api.core.ac.uk>, cross-repository OA
  aggregation). Inert until `DOIGET_ENABLE_CORE` is set. Works with no account; if
  you supply your own free key in `DOIGET_CORE_API_KEY` it is sent as a bearer
  header to CORE and to nowhere else, and is never written to the provenance log.
- `--features metadata`: **Europe PMC** (<https://www.ebi.ac.uk>, biomedical OA full
  text). Inert until `DOIGET_ENABLE_EUROPE_PMC` is set; queried by exact DOI only.
  No key or account.
- `--features tdm-springer | tdm-aps | tdm-elsevier | tdm-ieee`: the respective
  publisher text-and-data-mining APIs, used **only with your own API key and
  explicit agreement**. doiget bundles no keys and shares none. Each is consulted
  **only for DOI prefixes that publisher registered** (ADR-0041) — enabling
  `tdm-aps` does not disclose your Elsevier or IEEE lookups to APS — and only
  after Crossref failed to resolve the DOI. Since a publisher is asked only about
  DOIs it issued, and resolving such a DOI goes through it anyway, enabling one of
  these tells that publisher nothing it could not already observe.

  Two of them — `tdm-springer` and `tdm-ieee` (<https://ieeexploreapi.ieee.org>) —
  send the key as a **URL query parameter**, because neither upstream documents a
  header-auth path. It is therefore visible to that publisher's own server-side and
  proxy logs. doiget strips it from every URL it keeps: the metadata record, the
  provenance log and any error text (issue #146).

## What is stored locally

Fetched PDFs and metadata under your store root (default `./papers`, or
`DOIGET_STORE_ROOT`), plus an append-only local provenance log. All of it stays
on your machine.

## Credentials

Any publisher API keys you configure are read from your local environment or
`credentials.toml` and are used only to authenticate **your own** access to the
sources you explicitly enabled. doiget bundles no keys and shares none.

## Contact

Questions or concerns: <https://github.com/QAtlasHub/doiget/issues>
