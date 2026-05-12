# Real-world DOI / arXiv fixture set (Slice 6)

> **Status: INFORMATIVE.** Curated set of frozen response snapshots used by
> the wiremock-driven `real_world_fixtures_e2e` reference test in
> `crates/doiget-core/tests/real_world_fixtures_e2e.rs`. The companion
> master index lives at [`index.toml`](./index.toml).

## What these are

This directory holds **immutable, hand-crafted snapshots** that mirror the
real wire shapes of three public APIs:

- **Crossref REST** (`/works/<doi>`) — the canonical bibliographic
  metadata source for a DOI.
- **Unpaywall v2** (`/v2/<doi>?email=<contact>`) — open-access discovery
  layer that surfaces `best_oa_location.url_for_pdf`, `license`, etc.
- **arXiv export API** (`/api/query?id_list=<id>`) — Atom-feed response
  with `<entry><title>`, `<author>`, `<category>` children.

Each fixture pairs the **frozen request response** with an
[`expected.toml`](#expectedtoml-per-entry) describing what doiget's
metadata-only orchestrator MUST produce when fed that response.

**The reference test does NOT call any live API.** Snapshots are mounted
on a local `wiremock::MockServer` and the orchestrator is pointed at the
`http://127.0.0.1:N` origin via the `DOIGET_CROSSREF_BASE` /
`DOIGET_UNPAYWALL_BASE` / `DOIGET_ARXIV_BASE` env vars. The fixture set
is **closed** — adding or refreshing entries is a deliberate curation
step, never an automated CI side effect.

## Provenance — snapshot vs hand-crafted

Per the Slice 6 plan, fixtures may be either:

- **Snapshot from real API**: captured once with `curl` against the
  public endpoint, then trimmed and committed.
- **Hand-crafted realistic**: written by hand to match the documented
  API shape (Crossref's `message`, Unpaywall's `best_oa_location`,
  arXiv's Atom schema). The DOI / arXiv id chosen for the entry may be a
  real paper, but the response body is synthesized — we are testing the
  **response shape**, not the paper itself.

The current slice-6 set is **entirely hand-crafted** — this avoids the
licensing ambiguity of redistributing third-party API responses, keeps
each file ≤ 5 KB, and pins exactly the fields the test asserts on. Each
entry in `index.toml` carries `provenance = "hand-crafted"` so a future
curator can swap in a real snapshot per-entry without touching the test
driver.

## License — what we are NOT redistributing

- **Crossref REST data**: Crossref publishes their metadata under a
  Public Data Commons / CC0 framing — see their
  <https://www.crossref.org/operations-and-sustainability/terms/>. We
  reproduce only the field shape, not bulk metadata.
- **Unpaywall data**: CC0 per <https://unpaywall.org/products/api>. Same
  caveat — synthesized shapes, not redistributed records.
- **arXiv Atom feeds**: per arXiv's terms of use
  (<https://info.arxiv.org/help/api/tou.html>), the metadata may be
  freely used; we reproduce only the schema.
- **No PDFs.** This fixture set deliberately excludes the underlying
  paper artifacts. PDF licensing varies wildly per publisher; the
  `fetch_paper` orchestrator's PDF leg is exercised by the existing
  `crates/doiget-cli/tests/fetch_doi_oa_pdf_e2e.rs` and
  `crates/doiget-mcp/tests/fetch_paper_e2e.rs` tests with synthetic
  `%PDF-fake-bytes\n` payloads. The real-world set focuses on the
  **metadata response shape**.

## Layout

```
tests/fixtures/real_world/
├── README.md                       # this file
├── index.toml                      # master list with per-entry metadata
├── doi/
│   ├── <slug>/
│   │   ├── crossref.json           # frozen /works/<doi> response (or marker JSON when crossref is meant to fail)
│   │   ├── unpaywall.json          # frozen /v2/<doi> response (or absent)
│   │   └── expected.toml           # asserted shape: safekey, source, oa_url, license, title
│   └── …
└── arxiv/
    ├── <id-slug>/
    │   ├── atom.xml                # frozen /api/query Atom feed body
    │   └── expected.toml           # asserted shape: safekey, source, title, license=arxiv-default
    └── …
```

## `expected.toml` per entry

```toml
# Mandatory fields
safekey   = "doi_10.1234_example"
source    = "crossref"               # or "unpaywall" / "arxiv"
title     = "Example Paper"          # asserted against metadata payload

# Optional — when the entry exercises an OA path
oa_url    = "https://example.org/foo.pdf"
license   = "cc-by"                  # only when source = "unpaywall" or "arxiv"

# OR — when the entry asserts a failure code
# expected_error_code = "NO_OA_AVAILABLE"
```

The reference test reads each `expected.toml` and asserts the orchestrator
output equals these fields. Missing optional fields are not asserted on
(present-but-omitted is silent-pass; present-and-different is failure).

## `index.toml` master file

The driver test walks `index.toml`'s `[[entry]]` array. Each entry
declares its kind (`doi-crossref`, `doi-crossref-fail-unpaywall`,
`doi-no-oa`, `arxiv-new`, `arxiv-old`, `arxiv-versioned`,
`doi-long-suffix`, `doi-special-chars`, `doi-zenodo-denied`), the slug
under `doi/<slug>/` or `arxiv/<slug>/`, the original ref string, and
file paths for the mocked responses + expected output.

Set `disabled = true` on an entry to skip it (escape hatch when a real
API shape shifts and we need to keep CI green while we update the
snapshot). Dates use ISO-8601 `YYYY-MM-DD` in the per-entry
`last_refreshed_iso` field.

## How to add a new entry

1. Pick a stable, well-known DOI or arXiv id. Avoid embargoed material
   or publishers who explicitly forbid response redistribution.
2. If you want a real snapshot:
   ```sh
   curl -s 'https://api.crossref.org/works/10.1371/journal.pone.0001428' \
     | jq '{status, message: (.message | { DOI, title, author, link, "container-title", "issued", type })}' \
     > tests/fixtures/real_world/doi/<slug>/crossref.json
   ```
   Trim the response to ≤ 5 KB. We only need the fields doiget's
   orchestrator actually inspects (`title`, `author`, `issued`,
   `container-title`, `type`, `link[].URL`).
3. If you want a hand-crafted entry: just write the JSON / Atom by hand.
4. Write `expected.toml` with the fields the test should assert on.
5. Append an `[[entry]]` block to `index.toml`. Set `provenance` to
   `"hand-crafted"` or `"snapshot-from-real-api"`. Set
   `last_refreshed_iso` to today's ISO-8601 date.
6. Run `cargo test -p doiget-core --test real_world_fixtures_e2e` to
   sanity-check.

## What NOT to do

- **Do not include PDFs.** PDF licensing is publisher-specific and
  uncontrolled redistribution risks DMCA exposure. The PDF leg is
  exercised with synthetic bytes in other test files.
- **Do not refresh fixtures routinely.** Refresh **only** when (a) a
  test exposes a real upstream shape change (e.g. Crossref adds a new
  mandatory field), or (b) the entry's expected output is provably
  wrong. Document the rationale in the entry's `notes` field in
  `index.toml`.
- **Do not pull DOIs under embargo** or from publishers whose API
  ToS forbid response redistribution. When in doubt, hand-craft.
- **Do not let a single fixture exceed ~50 KB.** Crossref responses
  for highly-collaborative papers can be megabytes — trim ruthlessly.
- **Do not assume `unpaywall.json` is mounted for every DOI.** The
  metadata-only orchestrator returns immediately on Crossref success;
  Unpaywall is the fallback for Crossref failures (404 / 500). Entries
  intended to exercise the Unpaywall fallback set `crossref.json` to a
  404-marker (see the `doi-crossref-fail-unpaywall` kind).
