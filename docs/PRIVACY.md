# Privacy Policy

doiget is a local, single-binary CLI + stdio MCP server. It runs entirely on
your machine.

## What doiget collects

**Nothing.** doiget has no telemetry, no analytics, no crash reporting, and no
"phone home" of any kind ([ADR-0015](DECISIONS/0015-no-telemetry.md) /
[`SCOPE.md`](SCOPE.md) non-goal #10). It makes **no network connection that is
not the direct result of a paper you (or your agent) ask it to fetch**.

## What leaves your machine

Only the API requests required to fulfil a fetch you initiate:

- DOI / arXiv id lookups to **Crossref, Unpaywall, arXiv, and OpenAlex**
  (Open-Access metadata and PDF sources).
- An optional contact email in the `User-Agent` header **only if you set**
  `DOIGET_UNPAYWALL_EMAIL` / `DOIGET_CONTACT_EMAIL` (used for the providers'
  polite API pool; never sent otherwise).

doiget transmits no document content anywhere. It downloads PDFs and metadata
**to your local store** and never redistributes them.

## What is stored locally

Fetched PDFs and metadata under your store root (default `./papers`, or
`DOIGET_STORE_ROOT`), plus an append-only local provenance log. All of it stays
on your machine.

## Credentials

Any publisher API keys you configure are read from your local environment or
`credentials.toml` and are used only to authenticate **your own** access to the
sources you explicitly enabled. doiget bundles no keys and shares none.

## Contact

Questions or concerns: <https://github.com/sotashimozono/doiget/issues>
