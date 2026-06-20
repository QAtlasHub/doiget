# Changelog

All notable changes to doiget will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

`doiget-core` is the only crate with strict semver guarantees during the 0.x line; CLI
flag changes and `doiget-mcp` tool spec changes will be called out explicitly here.

## [Unreleased]

## [0.7.2-beta.1] - 2026-06-20

### Added
- **[distribution]** `flake.nix` — Nix Flakes integration (#247). Provides
  `packages.doiget` (built with `rustPlatform.buildRustPackage`, `oa-only`
  feature, Rust 1.86 toolchain), `apps.doiget` for `nix run`, and
  `devShells.default` with `cargo-deny`, `cargo-nextest`, `cargo-llvm-cov`,
  and `taplo`. Uses `rust-overlay` to pin the toolchain; `doCheck = false` in
  the Nix sandbox (network tests skipped).
- **[distribution]** macOS Intel (`doiget-macos-x86_64`) added to the release
  CI matrix (#247). The `macos-13` GitHub Actions runner builds natively for
  `x86_64-apple-darwin`; the signed binary and `.sha256` sidecar are uploaded
  to every GitHub Release.
- **[distribution]** `scripts/install.sh` now supports macOS Intel (#247).
  `uname -m == x86_64` on Darwin downloads `doiget-macos-x86_64` instead of
  erroring.
- **[batch]** `--delay <SECS>` (#326): sleep between individual fetches to avoid
  tripping per-host rate limits below the HTTP 429 threshold (APS, Springer, etc.).
  No delay before the first fetch; `<SECS>` is a float (e.g. `--delay 1.5`).
- **[batch]** `--user-agent <STRING>` (#326): override the default
  `doiget/<version>` User-Agent for every HTTP request in the batch. Useful when
  a publisher's WAF classifies the default string as a bot.
- **[core]** `HttpClient::new_with_user_agent(allowlists, ua)` — new public
  constructor that takes an explicit User-Agent string; `HttpClient::new` delegates
  to it using the default `doiget/<version>` UA.

## [0.7.2-beta.0] - 2026-06-20

### Added
- **[fetch]** Auto preprint fallback (issue #325): when a DOI OA PDF fetch is
  blocked (403, allowlist denial, magic-byte mismatch) and Unpaywall's response
  includes an arXiv preprint URL, `doiget fetch` now automatically retrieves
  the arXiv PDF and stores it under the DOI entry instead of returning an error.
  The stored metadata includes both `doi` and `arxiv_id`; `[doiget].source` is
  set to `"arxiv"`. Observable: `tracing::info!` at fallback attempt and
  success; the CLI success line names the arXiv ID used; the MCP
  `pdf_leg.status` wire value is `"preprint_fallback"`.
- **[core]** `PdfLegStatus::PreprintFallback { arxiv_id, original_block }` — new
  variant signalling that the stored PDF came from arXiv, not the publisher.
  `outcome_is_clean_success` treats it as success; `Blocked` semantics
  (exit ≠ 0) are preserved when arXiv also fails.
- **[batch]** `--only-failed` flag (issue #324): re-run a batch file and skip
  refs whose PDF already exists in the store (`<store>/<safekey>.pdf`).
  Metadata-only entries (`.metadata/<safekey>.toml`) are NOT skipped — they
  represent prior `NoOaUrl` or `Blocked` outcomes that may now succeed.
  In `--mode json`, skipped refs emit `{"ok": true, "ref": "...", "already_fetched": true}`.
  Summary line gains a `skipped-already-fetched` count when non-zero.
- **[core]** `trust_academic_repos` config flag (`[network] trust_academic_repos = true`
  in `config.toml`) that activates a curated set of 15 single-suffix academic host
  wildcards (`.ac.uk`, `.ac.jp`, `.edu.au`, `.edu.cn`, `.edu.br`, etc.) without
  requiring manual `[[network.additional_hosts]]` entries (#323).
- **[core]** `academic_repo_hosts()` public function in `doiget_core::user_extension`
  returning the built-in academic patterns; callers can compose or extend the set.
- **[core]** `UserExtensionConfig` struct (replaces the bare `Vec<UserExtensionHost>`
  previously returned by `load()`) exposing `additional_hosts` and
  `trust_academic_repos` fields.
- **[cli]** `config doctor` check now reports `trust_academic_repos` status alongside
  the user-extension host count.

### Changed
- **[core]** `user_extension::load()` return type changed from
  `Result<Vec<UserExtensionHost>, _>` to `Result<UserExtensionConfig, _>` (semver
  minor bump). Callers access hosts via `cfg.additional_hosts`.

### Changed
- **[cli]** `doiget config doctor` now prints a `tip:` remediation line on
  stderr for each failed check, naming the exact env var to set or the
  config file path to fix (#322). Stdout remains clean (no change to the
  empty-stdout-on-pass contract). Tips cover: missing `store_root` / `log_dir`
  parents (`DOIGET_STORE_ROOT`, `DOIGET_LOG_PATH`), unset contact email
  (`DOIGET_CONTACT_EMAIL`), and malformed `config.toml` (resolved path shown).

## [0.7.1-beta.0] - 2026-06-20

### Added
- **[tex-source]** `doiget tex-source <arxiv-id>` — Tier-1 OA command that
  fetches the raw LaTeX source for an arXiv preprint from the arXiv source API
  (`export.arxiv.org/src/<id>`) (#327). Supports gzip+tar archives (picks the
  longest `.tex` file by content length) and single-file gzip; detects
  PDF-only responses and emits an actionable `doiget fetch` note. The source
  text is the artifact (ADR-0017 Amendment 2): implicit Quiet does not suppress
  it, enabling `doiget tex-source arxiv:2401.12345 > paper.tex`. Results are
  cached under `<cache_root>/tex-src/` with a 7-day TTL. The MCP
  `get_paper_tex_source` tool follows the same fetch/cache path and emits
  provenance log `SessionStart`/`SessionEnd` bookends.
- **[core]** `paper_tex_source` module with `paper_tex_source()`, `PaperTexSource`,
  and `resolve_arxiv_src_base()` — the shared fetch/cache/extract logic used by
  both CLI and MCP.
- **[error]** HTTP 401/403 from the arXiv source endpoint maps to
  `ErrorCode::CapabilityDenied` in the `source` module.

## [0.7.0] - 2026-06-16

Promotes the cumulative `0.7.0-beta.0`–`0.7.0-beta.6` line to a stable release
(next → main, #318). Highlights since 0.6.0: discovery-search relevance-only
ranking with impact/recency filters (#290), arXiv published-version `cite`
merge (#303), bulk offline `bib` / `csl` export (#305), batch failure digest
and auto-chunking (#222 / #304), `TEXT_UNAVAILABLE` signalling (#302), and
real arXiv fetch metadata (#303). See the `0.7.0-beta.*` sections below for the
per-change detail.

### Fixed
- **[review #318]** second-pass promotion-review fixes:
  - **[text]** `doiget text <arxiv-id>` now emits its extracted prose under a
    non-TTY *implicit* Quiet — piping `doiget text … > paper.txt` no longer
    writes an empty file. `text` joins the artifact-command set (ADR-0017
    Amendment 2); only an **explicit** `--quiet` / `DOIGET_MODE=quiet`
    suppresses it.
  - **[search]** `--min-fwci` / `--min-percentile` are now validated: a
    negative or non-finite FWCI floor, or a percentile above 100, is rejected
    up front instead of composing a malformed OpenAlex `filter` clause (#290).
  - **[docs]** corrected the `output` module's Amendment 2 artifact-command
    list and the `csl --from-file` partial-array contract wording.
  - **Tests**: `text` piped / explicit-Quiet / unavailable-note behaviors,
    `cite` arXiv-shaped cross-ref guard (no spurious second resolve),
    `--min-fwci` / `--min-percentile` filter-clause e2e, out-of-range
    validation, and tightened `bib` / `csl --from-file` digest assertions.

## [0.7.0-beta.6] - 2026-06-16

### Added
- **[cite]** published-version merge for arXiv preprints (#303): when `doiget cite <arxiv-id>`'s Atom feed cross-references a published journal DOI (`<arxiv:doi>`), the entry now cites as the rich `@article` (journal / volume / issue / pages / publisher / issn / doi from Crossref) with the arXiv preprint identity retained (`eprint` / `archivePrefix` / `primaryClass`). No extra OpenAlex call — the DOI comes from the already-fetched feed. Best-effort: an absent or unresolvable cross-ref keeps the `@misc` preprint entry.
- **[csl]** bulk, offline CSL-JSON export from the local store, at parity with `bib` (#305): `doiget csl --all` emits every store entry as one deduplicated CSL-JSON array, and `doiget csl --from-file <FILE>` emits the refs listed in a file (plain refs / CSL-JSON / BibTeX), each rendered from the store. Missing entries are skipped; `--from-file` exits non-zero with the missing count. The positional ref is now optional and mutually exclusive with the two flags.
- **[batch]** end-of-batch **failure digest** on stderr: after the count summary, `doiget batch` lists each failed ref and its primary error code (`<ref> -> <ERROR_CODE>`), so a human / agent sees which refs failed and why without grepping the JSONL provenance log. stdout stays clean (`--mode json` JSON-Lines remains the machine channel) (#222).

### Changed
- **[search]** **(breaking)** discovery search ranking — relevance is now the **only** sort (#290). `--sort cited` / `--sort recent` (and the MCP `sort: cited|recent`) are **removed**: verified against live OpenAlex, every non-relevance sort over the loose free-text match floats high-scoring *off-topic* papers to the top. "Important / recent / high-quality" is now expressed as server-side **filters** that narrow the set without overriding topicality: new `--min-fwci <f>` (field-and-year-normalized impact floor), `--min-percentile <p>` (top-X% within the same-year cohort), and the existing `--from-year` used as a recency filter. The free-text query also now matches on `title_and_abstract.search` instead of the looser full-text `search=` (precision win). Still one OpenAlex request — `fwci` / `cited_by_percentile_year` are existing response fields.

### Fixed
- **[fetch]** `doiget fetch <arxiv-id>` now stores the **real** title / authors / year / subject categories from the Atom feed (which the fetch already retrieves) instead of an `arxiv:<id>` placeholder title. A later `doiget bib` / `info` on a fetched arXiv entry shows a usable reference rather than the bare id. The Atom leg stays best-effort — if it fails, the placeholder is kept so a successful PDF fetch always stores a valid entry (#303).
- **[review #318]** robustness fixes from the promotion review: `bib`/`csl` `--from-file` no longer aborts the whole export on one entry's read error (skip + count, matching `--all`); `csl` flattening surfaces (not silently drops) an entry that renders to an empty CSL item; `cite` prints a `note:` when a published-version DOI resolve fails (no longer a silent degradation) and only treats a bare-DOI cross-ref as a merge trigger; `csl` validates "no selector" before opening the store (parity with `bib`); `FetchError::TextUnavailable` now carries a validated `ArxivId` instead of a raw `String`.

## [0.7.0-beta.5] - 2026-06-16

### Changed
- **[deps]** dependency bumps merged from Dependabot: `biblatex` 0.11.0 → 0.12.0, `chrono` 0.4.44 → 0.4.45, `uuid` 1.23.2 → 1.23.3 (#298, #306), and the `codecov/codecov-action` CI action 6.0.1 → 7.0.0 (#300). `cargo-vet` `safe-to-deploy` exemptions updated to the new versions.

## [0.7.0-beta.4] - 2026-06-16

### Added
- **[bib]** bulk, offline BibTeX export from the local store (#305): `doiget bib --all` emits every store entry as one deduplicated `.bib`, and `doiget bib --from-file <FILE>` emits the refs listed in a file (plain refs / CSL-JSON / BibTeX), each rendered from the store. Missing entries are skipped with a stderr note; `--from-file` exits non-zero with the missing count (the `batch` failure-count convention) so a script can tell a complete export from a partial one. This turns "fetch a batch, then build a combined `.bib`" into a single offline command instead of a 100-call flaky loop.
- **[cite]** `doiget cite --offline <ref>` renders from the local store only (no network).

### Fixed
- **[cite]** `doiget cite <ref>` now falls back to the local store when the live resolve fails (network hiccup / OpenAlex flake) instead of returning nothing — an already-fetched ref always cites, with a `note:` on stderr marking the offline path. A ref in neither place is a non-zero error carrying the resolve failure, never a silent empty stdout (#305, cf. #302/#304).

### Notes
- CSL-JSON parity (`csl --all` / `--from-file`) is a deferred follow-up; this change covers the BibTeX surface the issue describes.

## [0.7.0-beta.3] - 2026-06-16

### Fixed
- **[cite]** `doiget cite <arxiv-id>` now emits a **complete** arXiv BibTeX entry instead of a title+author stub. The `@misc` entry gains `eprint`, `archivePrefix = {arXiv}`, `primaryClass`, and `year`: the year is parsed from the Atom `published` timestamp, and the primary class from the first `<category>` (or, for an old-style id like `cond-mat/0403602`, the archive prefix). Any entry carrying an arXiv id — including a stored one read by `bib` — now renders these fields. Previously an arXiv reference (a large fraction of a physics bibliography) needed manual `eprint`/`year` injection (#303).

### Added
- **[core]** `Metadata.arxiv_categories` — the arXiv subject categories (primary first), populated from the Atom feed and preserved across re-writes. Additive optional field; does not bump `schema_version` (`docs/STORE.md` §7).

### Notes
- Deferred to follow-ups (still part of #303): `bib` on a store entry written by the PDF-fetch path can still show a placeholder `arxiv:<id>` title until that path chains in Atom extraction; and merging a published version's metadata (`@article` via the DOI↔arXiv `link`) for arXiv ids with a journal version.

## [0.7.0-beta.2] - 2026-06-16

### Fixed
- **[batch]** `doiget batch <file>` no longer aborts the entire run when the input exceeds 100 refs. Previously it printed `Error: batch size N exceeds limit 100` and fetched **nothing** the moment a bibliography crossed 100 entries, forcing a manual `split`. The input is now processed in full, dispatched in bounded windows of `MCP_BATCH_MAX_SIZE` so the in-flight task count stays capped while the shared rate limiter keeps the 5-per-second politeness invariant across every window. The per-ref failure-count exit code is unchanged (non-zero when any ref fails). The `MCP_BATCH_MAX_SIZE` hard cap still applies to a single MCP `batch_fetch` request — that request-shape bound is unrelated to a local file handed to the CLI (#304).

## [0.7.0-beta.1] - 2026-06-16

### Fixed
- **[text]** `doiget text <arxiv-id>` no longer exits **0 with empty output** when ar5iv has no usable render. A 200 with no extractable prose (`char_count == 0` — the paper was never converted to HTML, including the case where the body parses to heading shells with empty bodies) now surfaces the new `TEXT_UNAVAILABLE` code on a non-zero exit, with an actionable `= note:` pointing at `doiget fetch arxiv:<id>`. Agents and the MCP `doiget_paper_text` tool can now tell **"text unavailable (fetch the PDF)"** apart from **"wrong identifier"** (`NOT_FOUND`) and **"no OA at all"** (`NO_OA_AVAILABLE`) instead of misreading the silent empty output as a bad DOI (#302).

### Added
- **[core]** `ErrorCode::TextUnavailable` (wire `"TEXT_UNAVAILABLE"`) and `FetchError::TextUnavailable { arxiv_id }` — the id is valid and resolvable but the requested representation is missing. Minor, additive (`ErrorCode` is `#[non_exhaustive]`). See `docs/ERRORS.md` §2.

## [0.7.0-beta.0] - 2026-06-16

### Added
- **[dist]** `scripts/install.sh` (POSIX `sh`, Linux/macOS) and `scripts/install.ps1` (Windows) — install the prebuilt, SHA-256-verified binary from the signed GitHub Release with no Rust toolchain required: `curl -fsSL https://raw.githubusercontent.com/sotashimozono/doiget/main/scripts/install.sh | sh`. `DOIGET_VERSION` pins a release (default: latest stable); `DOIGET_INSTALL_DIR` overrides the target (default `~/.local/bin` / `%LOCALAPPDATA%\Programs\doiget`). The binary's published `.sha256` sidecar is verified before install. First channel of the multi-platform distribution roadmap (#247); README gains an **Installation** section.

### Fixed
- **[cli]** `info` / `list-recent` / `search` / `link` no longer emit nothing under a non-TTY implicit Quiet (agent / pipe / ssh). They are now artifact-class and honor only **explicit** Quiet (`--quiet` / `-q` / `--mode quiet` / `DOIGET_MODE=quiet`), per [ADR-0017 Amendment 2](docs/DECISIONS/0017-output-mode-resolution.md): their stdout rendering IS the requested artifact and must reach a captured / piped caller. Previously a fetch-then-`info` confirmation read as "fetch failed" or "store empty" when it had in fact succeeded (#301).

## [0.6.0] - 2026-06-07

Discovery — the front half of the agent research loop (#281). `doiget search` becomes
a literature **discovery** tool, not just a local-store re-finder. See
[ADR-0031](docs/DECISIONS/0031-discovery-search-tier1.md).

### Added
- **[core]** `doiget_core::discovery` module — `paper_search(base, contact_email, query, ctx)` runs an external OpenAlex `/works?search=` query and returns ranked `PaperHit` candidates (DOI / OpenAlex id / arXiv id / title / authors / year / venue / **abstract** (reconstructed from `abstract_inverted_index`) / `cited_by_count` / `oa_status`). New public types `PaperSearchQuery`, `PaperHit`, `PaperSearchResults`, `SearchSort`, `DiscoverySource`. **Metadata-only** — never fetches a PDF (ADR-0031 D3).
- **[cli]** `doiget search <topic>` now performs external discovery by default, with `--limit`, `--from-year`, `--to-year`, `--oa-only`, `--min-citations`, `--sort relevance|cited|recent`, and the name-resolved entity filters `--author` / `--venue` / `--publisher` (each name is resolved to its OpenAlex ID via a `/authors` / `/sources` / `/publishers` `?search=` lookup, then applied as `authorships.author.id` / `primary_location.source.id` / `primary_location.source.publisher_lineage`; a name matching nothing is `NOT_FOUND`, one matching several entities with no clear winner is `AMBIGUOUS` and lists the candidates). `--limit` is validated to `1..=200` (rejected, not silently clamped). Tier-1 OA metadata, **always-on** — no `DOIGET_ENABLE_OPENALEX` gate and shipped in the default `oa-only` binary (ADR-0031 D1/D2).
- **[core]** `http::discovery_allowlist()` — always-compiled `api.openalex.org` allowlist entry so discovery search reaches OpenAlex in `oa-only` builds (the Tier-2 `tier_2_allowlist()` wiring stays `#[cfg(feature = "citation")]`).
- **[core]** `ErrorCode::Ambiguous` (wire `"AMBIGUOUS"`, CLI exit `2`) — a name filter matched several entities with no clear winner; distinct from `NOT_FOUND` so an agent narrows the query rather than concluding the entity does not exist. Additive on the `#[non_exhaustive]` enum.
- **[core]** `PaperSearchQuery::validate()` — shared limit/year-range boundary validation invoked by both the CLI and the MCP tool (single source of truth).
- **[mcp]** `doiget_paper_search` tool — external OpenAlex discovery over MCP (#281 item 2), mirroring `doiget search`. Validates inputs at the tool boundary; returns the `{ ok, scope, query, total_results, count, results }` envelope or a typed error (`AMBIGUOUS` / `NOT_FOUND` / …).
- **[core/cli/mcp]** **OA transparency** (#281 item 4) — `oa_status` (Unpaywall's `gold` / `green` / `hybrid` / `bronze` / `closed`, or `"green"` for arXiv; `null` when not determined) is now extracted from Unpaywall and surfaced on `MetadataOnlyOutcome` / `FetchPaperOutcome`, persisted to `[doiget].oa_status` in the store metadata (additive, no schema bump), and emitted in the `doiget_fetch_paper` / `doiget_metadata_only` MCP envelopes and the `doiget batch --mode json` `result`. Combined with the existing `pdf.status` (`no_oa_url` / `blocked`) and error codes (`NOT_FOUND` / `RATE_LIMITED`), an agent can now distinguish a paywalled work (`closed` + `no_oa_url`) from one it merely could not reach.
- **[core]** `doiget_core::paper_text` module — `paper_text(base, id, max_chars, ctx)` extracts the full text of an **arXiv** paper from **ar5iv**'s LaTeXML XHTML (`ar5iv.labs.arxiv.org/html/<id>`) and returns it as sectioned plain text (`{ heading, text }`), with inline math rendered from the source's `alttext` LaTeX. New public types `PaperText`, `TextSection`, `TextSource`. The #281 "read" step (item 3). **The PDF blob is never opened** — this is a separate fetch of the publisher's HTML rendering, distinct from PDF content processing ([ADR-0032](docs/DECISIONS/0032-fulltext-html-extraction.md) narrows [ADR-0003](docs/DECISIONS/0003-pdf-content-out-of-scope.md) / permanent non-goal #1 to PDF-*blob* processing). Tier-1 OA metadata, **always-on** (ships in the default `oa-only` binary). Extracted text is cached at `<cache_root>/text/<safekey>.json` (the doiget-private cache root, not the shared `~/papers/` store). `max_chars` truncation is flagged on `truncated`, never silent.
- **[cli]** `doiget text <arxiv-id>` — extract a paper's full text from ar5iv as a Markdown-ish title + section layout, or the `PaperText` JSON under `--mode json`. `--max-chars N` caps the output; `--no-cache` bypasses the text cache. A bare **DOI** reports `NO_OA_AVAILABLE` ("pass the arXiv id") — DOI→arXiv linking is #281 item 5 (ADR-0032 D5).
- **[core]** `http::fulltext_allowlist()` — always-compiled `ar5iv.labs.arxiv.org` allowlist entry under a distinct `"ar5iv"` source key so full-text extraction reaches ar5iv in `oa-only` builds and the provenance trail distinguishes it from the arXiv PDF/Atom API.
- **[mcp]** `doiget_paper_text` tool — full-text extraction over MCP (#281 item 2), mirroring `doiget text`. Takes `ref` (arXiv id) + optional `max_chars`; returns the `{ ok, arxiv_id, source, title, sections, char_count, truncated, retrieved_from }` envelope, or a typed error (a DOI → `NO_OA_AVAILABLE`; an unconverted paper → `NOT_FOUND`). Tier-1 OA, always-on; **never opens the PDF blob** (ADR-0032).
- **[core]** `doiget_core::discovery::resolve_links_for_doi(base, contact_email, doi, ctx)` + `PaperLinks` — given a published DOI, resolve its cross-identifier identity cluster (`{ doi, arxiv, openalex_id, title }`) via OpenAlex (`/works?filter=doi:`), in particular whether the work has a free **arXiv preprint** (#281 item 5: arXiv ↔ published-DOI linking & dedup). Tier-1 OA metadata, always-on (same OpenAlex path as discovery, ADR-0031); never fetches a PDF.
- **[cli]** `doiget link <doi>` — report whether a DOI's work has an arXiv preprint (+ OpenAlex id / title), as human lines or `PaperLinks` JSON under `--mode json`. Lets an agent read the free full text (`doiget text arxiv:<id>`) or dedup a preprint against its journal version. arXiv → DOI (reverse) is a planned follow-up; a non-DOI ref is rejected.
- **[mcp]** `doiget_link` tool — DOI → arXiv preprint linking over MCP (#281 item 2/5), mirroring `doiget link`. Takes `ref` (a DOI); returns `{ ok, doi, arxiv, openalex_id, title }` or a typed error (a non-DOI ref → `INVALID_REF`; a DOI with no OpenAlex work → `NOT_FOUND`). Tier-1 OA, always-on; never fetches a PDF.
- **[core]** The arXiv Atom-feed parser now captures the **published DOI** and **journal reference** (`<arxiv:doi>` / `<arxiv:journal_ref>`) into the metadata JSON (`doi` / `journal_ref` keys, omitted when absent) — the arXiv → published-DOI link (#281 item 5, the reverse of `doiget link`). These ride the raw payload, so they surface via the MCP tools `doiget_metadata_only` / `doiget_resolve_paper`; they are **not** written to the store, so `doiget info` does not show them (the store write forces an arXiv entry's own `doi` to `None`). Additive.

### Fixed
- **[mcp]** The MCP `FetchError` → error-code mapping now uses the canonical `From<&FetchError> for ErrorCode` instead of a hand-rolled match whose wildcard collapsed `NOT_FOUND` (and the new `AMBIGUOUS`) to `INTERNAL_ERROR`.

### Changed
- **[cli] [BREAKING]** `doiget search <query>` default scope changed from the **local-store substring scan** to **external OpenAlex discovery**. The local scan is now `doiget search --local <query>` (behaviour otherwise unchanged). Scripts relying on the old default must add `--local`.
- **[cli] [BREAKING]** `doiget search --mode json` now emits a `{ "scope": "external" | "local", "query": "...", "count": N, "results": [...] }` envelope (the external scope additionally carries `"total_results"`, the upstream OpenAlex match count which may exceed `count`; the `results[]` element schema is scope-dependent — the local element is the unchanged `EntryInfo` shape). Previously it emitted a bare `EntryInfo` array.

## [0.5.0] - 2026-06-02

Promotion of the `next` integration line to a stable release. A **minor**
bump (not patch) because it adds new public surface: the `doiget lint`
subcommand and the `ErrorCode::NotFound` / `source::FetchError::NotFound`
variants, on top of the `doiget verify` reference-classification work.

### Added
- **[core]** `ErrorCode::NotFound` (wire `"NOT_FOUND"`) — a metadata source authoritatively reported the identifier does not exist (HTTP 404 / 410 / 451, or a source-specific absence such as arXiv's empty `<feed>` for an unknown id), distinct from the transient `NETWORK_ERROR` / `RATE_LIMITED`. Additive variant on the `#[non_exhaustive]` enum. A matching `source::FetchError::NotFound` variant carries the non-HTTP absence signal.
- **[cli]** `doiget lint <path>` — structural validation of a BibTeX bibliography, independent of DOI resolution (`doiget verify`'s job) and the network. Flags missing expected fields per entry type, blank fields, and `$$` display-math titles that break some downstream renderers (e.g. DocumenterCitations). **Read-only and math-aware**: inline `$...$` titles are never touched or flagged. Emits one JSON-Lines finding per issue; structural rules are warnings (exit 0) while an unparsable file is an error, and `--strict` promotes warnings so any finding fails the run.

### Changed
- **[cli]** `doiget verify` now classifies a non-resolving id by *why* it failed instead of a single `unresolved` bucket. **`absent`** (HTTP 404/410/451 or an empty arXiv feed → `NotFound`, a definite dead reference) **always** counts toward the exit code, independent of `--strict`; **`unreachable`** (transient transport / 429 / 5xx / timeout) is tolerated by default and fails only under `--strict`. This lets the default lane catch genuinely dead references — including unknown arXiv ids — while staying green through network blips. An `InternalError` from resolution now aborts the run (a bug signal, not a tolerable blip), alongside the existing `LogError` abort. The JSON-Lines `status` field gains `absent` / `unreachable` and no longer emits `unresolved`.

### Fixed
- **[core]** `doiget verify` no longer aborts when its provenance log's parent directory does not yet exist; the parent is created on open (#274).

## [0.4.1] - 2026-06-02

Promotion of the `0.4.1-beta.0` integration line on `next` to a stable
release. Rolls up the never-tagged `0.4.1-beta.0` window plus the
subsequent reference-tooling work (`.bib` input, `doiget verify`,
`doiget cite`). SemVer **patch** bump within the additive 0.4.x line;
all new surfaces are optional/additive.

### Added
- **[core]** BibTeX / BibLaTeX (`.bib`) bibliography input via the `biblatex` crate, completing the ADR-0030 D2 parser slice. `doiget batch` and `doiget verify` now accept `.bib` files; identifier priority is `doi` > arXiv `eprint`.
- **[cli]** `doiget verify <path>` — check that every DOI / arXiv reference in a `.bib` / CSL-JSON / plain-refs file resolves to real metadata, **without** downloading PDFs or writing to the store. Emits one JSON-Lines record per entry; exit code is the failing-entry count (capped at 255).
- **[cli]** `[verify]` config section (`on_missing_id = "warn" | "error" | "skip"`, `strict`) controlling how id-less and unresolved entries affect the exit code. `--strict` overrides the config.
- **[ci]** `doiget-verify` composite GitHub Action (`.github/actions/verify`) so other repositories can gate their bibliography references in CI.
- **[cli]** `doiget cite <ref>` — resolve a DOI / arXiv reference live (cache-aware, **no** store write) and print a clean BibTeX entry on stdout, a `doi2bib`-style helper. The DOI path enriches the entry from the Crossref envelope (year / journal / volume / number / pages / publisher / ISSN), and HTML / MathML markup in titles (`<i>`, `<mml:math>`) is scrubbed via the shared `to_bibtex` renderer. `cite` is an independent, clean-room implementation — it uses no code from the AGPL-3.0 `doi2bib` project, so doiget stays MIT-licensed.
- **[core]** `Metadata` gains optional `volume` / `issue` / `pages` reserved fields (STORE.md §2); `to_bibtex` renders them as `volume` / `number` / `pages` and `to_csl_array` as `volume` / `issue` / `page`. Crossref single-hyphen page ranges (`477-528`) are normalized to the BibTeX en-dash form (`477--528`).
- **[core]** Support suggesting an arXiv version when a primary PDF fetch fails. The `PdfLegStatus::Blocked` variant now carries an optional `suggested_arxiv_id` field populated from Unpaywall metadata.
- **[core]** Support resolving free-form bibliographic citation strings to ranked DOI candidates via Crossref Works query API. Compute a token-based overlap similarity score to filter (score >= 0.5) and rank candidates.
- **[cli]** The `doiget fetch` command prints a suggested arXiv command when a primary PDF fetch is blocked but an arXiv alternative is available.
- **[cli]** Add `doiget resolve-citation "<query>"` command and `doiget batch-resolve-citations` command (which reads queries from stdin line-by-line) to return resolved DOI candidates in JSON.
- **[cli]** Add `doiget version [--check]` command to print the current version and optionally query GitHub Releases for the latest stable tag.
- **[mcp]** The `doiget_fetch` MCP tool output includes the `suggested_arxiv_id` in the `pdf_leg` object when blocked.
- **[mcp]** Add `doiget_resolve_citation` and `doiget_batch_resolve_citations` MCP tools to resolve bibliographic citation strings to ranked DOI candidates.

### Fixed
- **[core]** arXiv metadata (Atom) now queries `export.arxiv.org/api/query` instead of `arxiv.org/api/query`, which redirected and failed the resolve. PDFs still use `arxiv.org`; the two endpoints now use separate bases. Fixes arXiv `eprint` references resolving as `unresolved` in `doiget verify` / `doiget_metadata_only`.

## [0.4.0] - 2026-05-21

Promotion of the `0.4.0-beta.1..beta.13` integration line on `next` to
a stable release. SemVer **minor** bump (not patch) — this window
introduces multiple new public surfaces (`doiget_core::refs` module,
`doiget_batch_from_bibliography` MCP tool, `FetchPaperOutcome` field
additions, user-extensible capability gate, fetch chain) and one CLI
flag set expansion, alongside several behavioural changes for the
OA-PDF leg and batch JSONL wire shape. Beta-window detail is preserved
in the merge commits on `next` (see `git log --merges v0.3.0..HEAD`)
and the ADR set under `docs/DECISIONS/`.

### Added
- **[core] ADR-0028 user-extensible capability gate** — new
  `doiget_core::user_extension` module parses
  `<config_dir>/doiget/config.toml`'s `[[network.additional_hosts]]`
  entries (literal FQDN or single-suffix `*.<suffix>` wildcards) and
  merges them into the `oa-publisher` redirect allowlist. The `host`
  field is a `HostPattern` newtype whose `Deserialize` runs validation
  so the "valid pattern" invariant is type-level. Both `doiget fetch`
  and `doiget serve` honor the extension; `doiget config doctor`
  reports health; `doiget capabilities` JSON exposes
  `user_extension_count`. ToS+verified-curation framing rejects
  WAF-bypass / impersonation proposals permanently (ADR-0028 D3 and
  #223 close-rationale).

- **[core] ADR-0029 fetch chain (slice 1)** — the DOI OA-PDF leg now
  walks a multi-candidate chain from Unpaywall's `best_oa_location` +
  `oa_locations[]` instead of trying only the single "best" URL. On
  any non-PDF / 403 / network failure the chain advances; first
  successful candidate wins. Each attempt emits its own
  `oa-publisher` Fetch provenance row. Recovers the dogfood case
  where a DOI hits a WAF-blocked publisher but the same record's
  arXiv preprint is in `oa_locations[]`.

- **[core] ADR-0030 bibliography input adapters (slice 1)** — new
  `doiget_core::refs` module exposes `Format`, `ParsedEntry`,
  `ParseError`, and `parse_input(text, format, path)` that
  auto-detect plain-refs / CSL-JSON / BibTeX (BibTeX returns
  `UnsupportedFormat` until the biblatex follow-up slice).
  Identifier priority is DOI > arXiv > PMID-parking. 23 unit tests
  cover format detection, the priority rule, Zotero/Mendeley
  wire-shape quirks, and malformed-input tolerance.

- **[mcp] `doiget_batch_from_bibliography`** — new MCP tool per
  ADR-0030 D6 accepting a CSL-JSON file path and returning structured
  per-entry fetch outcomes with the source bibliography's `entry_key`
  threaded through. Unlocks the Zotero distribution path: a plugin
  author can hand a `.json` file to `doiget serve` and bridge
  fetched PDFs back to the originating reference without shelling
  out. `strict` input field controls per-entry parse-error abort;
  malformed-input always aborts.

- **[cli] `doiget batch library.json`** — `doiget batch` now
  auto-detects CSL-JSON inputs by file extension + content
  fingerprint and dispatches through `refs::parse_input`. Plain refs
  files (`.txt`) keep working unchanged. Malformed CSL-JSON aborts
  with a loud whole-input error rather than silently behaving as an
  empty batch.

- **[cli] `doiget batch --json` structured outcomes (#210 / S4)** —
  JSONL success records carry
  `result.{safekey, store_path, canonical_digest}`; failure records
  carry the typed `ErrorCode` wire string plus an ADR-0023
  `denial_context` when applicable. `PdfLegStatus::Blocked` outcomes
  surface as failures with the policy-class reclassification
  (`effective_blocked_code`).

- **[cli] global flags `--store-root` / `--log-path` / `--color` /
  `--progress` (#211 / S5)** — four new global CLI flags with
  env-var precedence, ValueEnum drift guards, and 17 unit tests
  covering precedence and validation.

- **[cli] `doiget config doctor` user-extension surface (ADR-0028
  D2-3)** — adds a checklist entry reporting
  `[ ok ] user-extension hosts loaded: N` or
  `[FAIL] user-extension config invalid: <error>`. Missing config is
  normal.

- **[cli] `doiget capabilities` JSON `user_extension_count` field
  (ADR-0028 D2-4)** — additive top-level field reporting how many
  user-extension hosts are loaded on the current host. Drift-guarded
  by parity test.

- **[core] `FetchPaperOutcome` field additions** —
  `safekey: String` and `canonical_digest: String` are always-
  populated additive fields on the `#[non_exhaustive]` struct
  (#210).

- **[core] `#[doc(hidden)] FetchPaperOutcome::for_test_synthetic`** —
  test-only constructor exposed for downstream test code to drive
  classification logic without running the full orchestrator.

### Changed

- **[core/http] `http://` → `https://` URL upgrade (#220 / S2)** —
  `fetch_inner` upgrades legacy `http://` URLs returned by metadata
  sources to `https://` before sending, with case-insensitive
  `localhost` literal + `.localhost` TLD detection, IPv6 loopback
  (`::1` + IPv4-mapped) detection, and a `tracing::warn!` on the
  `set_scheme` fallback. 12 unit tests pin the edge cases.

- **[cli] ADR-0017 Amendment 1: explicit-vs-implicit Quiet (#220 /
  S1)** — `OutputMode::Quiet` now distinguishes a user's explicit
  `--quiet` / `--mode quiet` from the implicit TTY-detection
  fallback. `doiget capabilities` (and `bib` / `csl`) — the artifact
  commands — respect only explicit Quiet and emit their JSON
  inventory on non-TTY pipes so an LLM cold-boot from a stripped
  environment is not silently empty. `ResolvedOutput { mode,
  quiet_was_explicit }` and `is_artifact_command()` helper added.
  `audit-log --verify` is NOT routed through
  `is_artifact_command()` — its `--mode json` body is produced by an
  internal branch in the subcommand handler rather than by the
  artifact-vs-informational discriminator; the wire shape is
  unchanged on this release.

- **[cli] `FetchHarness::fetch_one` returns
  `Result<FetchPaperOutcome, FetchError>` (#210)** — replaces the
  prior `anyhow::Result<()>` so callers can render machine-readable
  shapes from the same typed outcome. Rendering / `CliExit`
  synthesis moves to per-caller paths.

- **[cli] `batch` JSONL `ref` field** — now the bare identifier per
  `docs/PROVENANCE_LOG.md` §3 (`Ref::as_input_str()` canonical
  form) rather than the raw file line. For an input line
  `arxiv:2401.99999`, the JSONL `ref` is now `"2401.99999"`. DOI
  inputs were already in this form. Wire-format-visible.

### Documentation

- **ADR-0017 Amendment 1, ADR-0028, ADR-0029, ADR-0030** — four new
  design records covering quiet bifurcation, the user-extensible
  capability gate, the fetch chain primitive, and the bibliography
  input adapters.

### Notes — deferred to follow-up slices

- ADR-0028 D2-2 `verified_by = "user"` per-attempt provenance row
  field.
- ADR-0029 D4/D5: `chain: Vec<AttemptOutcome>` +
  `ALL_FALLBACKS_EXHAUSTED` + schema-additive `chain_*` provenance
  columns.
- ADR-0030 D2/D5/D6: BibTeX/.bib parsing (needs `biblatex` crate +
  cargo-vet exemptions), `--format` / `--strict` CLI flags,
  structured `entry_key` on JSONL `error` object.
- MCP `doiget_fetch_paper` envelope upgrade for `safekey` /
  `canonical_digest`.
- `canonical_digest` on `EntryInfo`.
- Single-fetch `--json` symmetric output.
- #212 MCP/CLI output shape alignment, #222 batch resumability.

## [0.3.0] - 2026-05-20

Promotion of the `0.2.1-beta.1..beta.12` integration line on `next` to a
stable release. Bumped to a SemVer **minor** (not patch) because this
window introduces multiple new public surfaces — the global output-mode
flags, the `--mode json` body contract for previously human-only
commands, the batch JSONL per-ref shape, and a new `doiget
capabilities` subcommand — alongside one practical default-behaviour
change for non-TTY callers. CLI flag surface and `doiget-mcp` tool
spec changes are called out below per the policy in the changelog
preamble.

### Added

- **[cli] `doiget capabilities` — single-shot inventory JSON for LLM
  cold-boot (#214 / #215).** A new subcommand that emits one parseable
  JSON value listing the binary's full surface in one round-trip:
  `version` + compile-time `features` (so an agent can tell whether
  `graph` / TDM is available in *this* build); the four `OutputMode`
  values; `global_flags` (`--mode` / `--json` / `--quiet`) with help
  text + accepted values; `subcommands[]` walked from the live
  `clap::Command` tree (cannot drift from the parser) with name,
  summary, positional `args`, named `flags`, hand-maintained
  `examples`, a `json_mode` discriminant (`artifact` / `supported` /
  `unsupported`) carrying its own status tag, and any `feature_gated`
  Cargo feature; `env_vars[]` (DOIGET_* table mirroring
  `docs/CONFIG.md` §5); `mcp_tools[]` (the `doiget_*` tool inventory
  from `docs/MCP_TOOLS.md` §1); `docs{}` map pointing at the canonical
  spec files. Output is always JSON (product-output convention);
  `--mode quiet` is the one mode that suppresses, per ADR-0017 / #203.
  Field names are part of the public wire format with the same
  stability discipline as `EntryInfo` / `MigrationReport`. Every public
  schema struct + `JsonMode` / `FlagKind` / `ArgKind` enum carries
  `#[non_exhaustive]` so additions are non-breaking and renames /
  removals are compile-time breaks for Rust consumers. Parity unit
  tests lock the canonical `env_vars` / `mcp_tools` sets and the per-
  subcommand metadata coverage so drift between code, docs, and the
  `Cli` enum becomes a CI failure.
- **[cli]** Global output-mode flags `--mode <human|json|quiet|mcp>`,
  `--json`, `-q`/`--quiet`, and the `DOIGET_MODE` environment variable
  are now parsed and resolved per the ADR-0017 precedence ladder
  (`--mode` > `--json`/`--quiet` > `DOIGET_MODE` > subcommand-implicit
  > TTY > quiet default). The three flag forms are mutually exclusive
  (clap-enforced). `doiget serve` is pinned to `mcp` regardless of
  flags / env, preserving the load-bearing stdout-purity invariant
  (CONFIG.md §5 / Slice 9). (#144)
- **[cli]** `--mode json` (and `--json` / `DOIGET_MODE=json`) now emits
  structured JSON for six commands that previously emitted human-only
  output: `info` (the `Metadata` struct), `list-recent` / `search`
  (a JSON array of `EntryInfo` objects with
  `{safekey, title, year, fetched_at}`), `config show` (the
  `ResolvedConfig` struct), `config path` (`{"config_path": "..."}`),
  `audit-log --verify` (a report object with `total_rows` /
  `total_ok` / `total_issues` / per-segment summaries / per-issue
  records), and `provenance migrate` (the `MigrationReport` wrapped
  with `log_path`). Single-value bodies (NOT JSON-Lines — the batch
  JSONL contract is the separate ERRORS.md §3 surface). Stderr (human
  errors) is unaffected. `Serialize` was added to two additive public
  types in `doiget-core`: `store::EntryInfo` and
  `provenance::MigrationReport`. (#204)
- **[cli]** `batch --mode json` (and `--json` / `DOIGET_MODE=json`) now
  emits the ERRORS.md §3 CI-persona JSON-Lines per-ref shape on stdout:
  one record per input line of the form `{"ok": true, "ref": "..."}`
  on success or
  `{"ok": false, "ref": "...", "error": {"code": "...", "message": "..."}}`
  on failure. Exit code is the failure count (unchanged, capped at
  255 per ERRORS.md §4). Human mode is unchanged (per-ref summary
  remains on stderr per ADR-0001). The structured outcome bodies
  (`safekey` / `store_path` / `canonical_digest` on success;
  `denial_context` on `CAPABILITY_DENIED`) require `fetch_one` to
  return `FetchPaperOutcome` instead of `Result<()>` and land in a
  follow-up; the contract surface ships now. (#205)
- **[provenance]** Log rotation + retention (`docs/PROVENANCE_LOG.md`
  §6), previously unimplemented (#140). When `access.log` exceeds
  100 MiB an `append` gzip-archives it to
  `access.log.<YYYY-MM-DD-HHMMSS>.gz` and starts a fresh GENESIS-rooted
  segment (the hash chain restarts per segment — segments are not
  linked). Rotation is **fail-closed**: any gzip/rename/unlink error
  aborts the append (and the surrounding fetch) so the chain never
  silently skips. At `open`, rotated segments older than
  `DOIGET_LOG_RETENTION_DAYS` (default 90; `0` disables) are pruned
  **best-effort** (a prune failure is logged, not fatal). `doiget
  audit-log --verify` now verifies the full history — every rotated
  `.gz` plus the current file — reporting per-segment when more than
  one exists; single-segment output is unchanged. Adds the pure-Rust
  `flate2` (`miniz_oxide` backend — no C toolchain, consistent with
  ADR-0020 portability). Internal `DOIGET_LOG_ROTATE_BYTES` ops/test
  knob (`0` disables rotation).

### Changed (potentially breaking)

- **[cli] Non-TTY default is now `Quiet`.** The `--mode quiet` honoring
  slice (#203) changed the default behaviour for non-TTY invocations:
  pipelines and shell scripts like `doiget audit-log --verify | tee
  audit.log` or `doiget list-recent | awk -F'\t' …` now emit empty
  stdout because the resolver lands on `Quiet` when stdout is not a
  terminal. To restore the previous output, either pass `--mode human`
  or set `DOIGET_MODE=human` in the calling environment. The semantic
  was always documented by ADR-0017 / CONFIG.md §5, but the practical
  break is new here.

### Changed

- **[cli]** `--mode quiet` (and `-q`/`--quiet`/`DOIGET_MODE=quiet`/the
  non-TTY default) now suppresses *informational* stdout in the six
  commands that previously emitted it unconditionally: `audit-log
  --verify` (header / per-segment summary / per-issue lines), `info`
  (TOML dump), `list-recent` (TSV table), `search` (TSV table),
  `config show` (TOML dump), `config path` (path), and
  `provenance migrate` (summary). Errors (stderr), exit codes, and
  on-disk side effects are unaffected — `audit-log` with chain issues
  still exits non-zero even with `stdout == ""`. `fetch` / `batch`
  were already quiet by design (success/summary on stderr per
  ADR-0001), and product-output commands (`bib` / `csl` / `graph` /
  `*-dry-run` plan) are deliberately not suppressed. (#203)
- **[core]** `oa-publisher` redirect allowlist now includes
  physics-society / diamond-OA hosts: `*.aps.org` (APS), `scipost.org`
  + `*.scipost.org` (SciPost diamond OA), `*.iop.org` (IOP) (#193, per
  ADR-0027 and `docs/REDIRECT_ALLOWLIST.md` §5). The list was
  bio/medical-leaning; a real `doiget batch` over 30 OpenAlex-OA
  finite-temperature-MPS DOIs had 7 denied purely because the
  discovered OA PDF host was off-list (24/30 → ~30/30). Unlike the
  surrounding `(unverified)` entries these are empirically verified.
  `hdl.handle.net` / `ruj.uj.edu.pl` (open handle/repo surfaces) are
  deliberately out of scope.
- **[core]** `store::EntryInfo` and `provenance::MigrationReport`
  carry an explicit doc-comment "wire-format stability" note: field
  names became part of the public API the moment `Serialize` shipped.
  Renaming a field is now a semver minor bump warranting a CHANGELOG
  `[BREAKING]` callout; new fields are safe (`#[non_exhaustive]`).
- **[cli/batch]** `batch --mode json` JoinSet-panic record now emits
  `"ref": null` instead of the sentinel string `"<task-panic>"`. A
  consumer doing `retry(rec["ref"])` would have mishandled the
  sentinel as a literal "DOI"; `null` is honest and parseable.

### Fixed

- **[portability]** `doiget` now installs **everywhere** — `cargo
  install doiget-cli` no longer requires cmake/nasm/go, and the
  published Linux binary runs on old glibc (Ubuntu 20.04 / RHEL 8 /
  HPC boxes). Root cause: reqwest's `rustls` feature pulled the
  aws-lc-rs crypto provider (heavy C toolchain) and the release
  binary was dynamically linked against the runner's glibc. Now:
  `reqwest` uses `rustls-no-provider` with the `ring` crypto provider
  (cc + perl only), installed as the process default in
  `doiget-core`'s `http` module; the release Linux artefact is a
  static `x86_64-unknown-linux-musl` build. TLS posture is unchanged
  (rustls-only, platform-verifier roots; `deny.toml` allowlist still
  satisfied). See ADR-0020 Amendment 1.
- **[core]** `Doi::parse` now accepts `:` in the DOI suffix (#194).
  Legacy Kluwer/Springer (`10.1023/A:NNNNNNNNNN`) and EDP Sciences /
  Journal de Physique (`10.1051/jphys:NNNN`) DOIs — both resolvable
  at doi.org and via Crossref — were previously rejected with
  `INVALID_REF`, silently losing real papers from physics corpora
  (3/38 niche refs lost in the Ising-RG dogfood). `:` grants no
  path-traversal capability beyond the already-permitted `/`, and
  `safekey` escapes it before any filesystem use. `docs/SECURITY.md`
  §1.1 charset widened to `[A-Za-z0-9._/():-]` per ADR-0026.
- **[mcp/core]** `doiget_metadata_only` now writes the metadata TOML
  to the store (`<root>/.metadata/<safekey>.toml`) — the documented
  `docs/MCP_TOOLS.md` §11 SIDE EFFECT that was previously a `TODO`
  (orchestrator returned provenance rows but zero disk artifacts, so
  `doiget_info` after `doiget_metadata_only` returned `metadata:
  null`). Implemented as a new `metadata_only_to_store` wrapper
  around the unchanged **pure** `metadata_only`; `doiget_resolve_paper`
  (`resolve_only`) keeps delegating to the pure resolver, so its
  `docs/MCP_TOOLS.md` §1 "NEVER writes a metadata TOML" contract now
  holds *structurally* (the store-write lives in a separate entry
  point `resolve_only` does not call) and cannot regress. (#139)
- **[repo]** `LICENSE` is now the verbatim 21-line SPDX MIT body. The
  trailing `---` separator + paper-licensing `Note:` paragraph (which
  pushed GitHub's `licensee` classifier below its match threshold, so
  the repo showed `licenseInfo: Other`) is removed; the identical
  posture statement already lives in `docs/LEGAL.md` and the site
  posture page. Restores MIT classification for crates.io / SPDX /
  shields. (#157)

## [0.2.0](https://github.com/sotashimozono/doiget/compare/doiget-core-v0.1.3...v0.2.0) - 2026-05-18

First release cut under the tag-driven pipeline (ADR-0025): a single signed
workspace tag `v0.2.0`, gated by the mandatory version gate. The **minor** bump
(0.1.x → 0.2.0) signals the called-out breaking CLI exit-code-contract and MCP
tool-spec changes below — per this project's 0.x semver policy (CHANGELOG
header), such breaks are permitted within 0.x when explicitly enumerated. This
section is hand-curated from the real non-merge history `doiget-core-v0.1.3..main`
(#159/#160/#161/#162/#163/#165) — it replaces the materially inaccurate
release-plz-generated `#164` section, which (traversing first-parent only)
captured a single `fix(core)` line plus a stray merge subject and dropped the
MCP spec-conformance, CLI exit-code-contract, credential-hygiene and docs work.

### Changed

- *(mcp)* **[breaking: MCP tool spec]** `doiget_capability_profile` response now
  conforms to `MCP_TOOLS.md` §7: corrected shape, non-goal/forbidden tools are
  guarded, and `denial_context` is routed through a logged helper so denials are
  observable. Added `serde(deny_unknown_fields)` on the profile type and a
  negative test asserting forbidden tools are rejected. ([#159](https://github.com/sotashimozono/doiget/pull/159), closes #141/#152/#154)
- *(cli)* **[breaking: CLI exit-code contract]** Exit codes and environment
  variables aligned with `ERRORS.md`/`CONFIG.md`: batch failure-count exit
  semantics (#143), `Blocked` → `CAPABILITY_DENIED` classification (#145),
  graph/audit-log exit codes (#149), and `DOIGET_LOG_PATH` log-path unification
  (#142). `--help` gains `long_about`; `ERRORS.md` §2/§6.1 updated. ([#162](https://github.com/sotashimozono/doiget/pull/162), closes #142/#143/#148/#149)

### Fixed

- *(core)* TDM `api_key` is now threaded through the capability grant (secrecy
  0.10) rather than read out of band; `tdm_springer` key URL-redaction added;
  the Semantic Scholar `x-api-key` header is wired; `dry_run` uses the fallible
  `try_*` API; rustdoc/Debug redaction hardened so the S2 key never appears in
  `Debug` output. ([#161](https://github.com/sotashimozono/doiget/pull/161), refs #153/#156)
- *(core)* The OA-publisher allowlist is now enforced on the
  Unpaywall-discovered OA URL **before** the pre-fetch, not only on redirect
  hops — closing the off-allowlist OA-fetch gap. ([#163](https://github.com/sotashimozono/doiget/pull/163), refs #145)

### Docs

- Planning artifacts reconciled with shipped v0.1.3 reality; date-provenance
  wording and the `SOURCES.md` non-goal cross-reference corrected. ([#160](https://github.com/sotashimozono/doiget/pull/160))
- *(site)* `docs/`→`site/` projection resynced so the Zola `build (zola)` job
  passes again (errors.md projection refreshed for the `ERRORS.md` §6.1 edits). ([#165](https://github.com/sotashimozono/doiget/pull/165))

## [0.1.3](https://github.com/sotashimozono/doiget/compare/doiget-core-v0.1.2...doiget-core-v0.1.3) - 2026-05-17

### Fixed

- MVP polish batch (closes #123)
- *(store)* write PDF before metadata for crash-consistency (closes #122)

### Other

- Merge branch 'main' into fix/122-torn-write-ordering-r2

## [0.1.2](https://github.com/sotashimozono/doiget/compare/doiget-core-v0.1.1...doiget-core-v0.1.2) - 2026-05-17

### Other

- Merge pull request #126 from sotashimozono/test/121-bibliofetch-roundtrip
- *(store)* BiblioFetch round-trip — typed table + unknown scalar (closes #121)

## [0.1.1](https://github.com/sotashimozono/doiget/compare/doiget-core-v0.1.0...doiget-core-v0.1.1) - 2026-05-17

### Added

- *(mcp)* Slice 15b — doiget_bibtex_export + doiget_csl_export tools

## [0.0.0](https://github.com/sotashimozono/doiget/releases/tag/doiget-core-v0.0.0) - 2026-05-15

### Added

- *(core)* Slice 20 — per-source HTTP header hook
- *(core)* Slice 18 — APS Harvest TDM source (Phase 5b)
- *(core)* Slice 17 — Springer Nature OA TDM source (Phase 5a)
- *(slice-13)* DOAJ source impl (Tier 2, Phase 4)
- *(slice-12)* Semantic Scholar source impl (Tier 2, Phase 4)
- *(slice-14)* citation_graph BFS expansion (ADR-0010, Phase 4)
- *(slice-11)* OpenAlex source impl (Tier 2, Phase 4)
- *(slice-10)* tier_2_allowlist() — Phase 4 redirect-allowlist scaffolding
- *(slice-7)* doiget_resolve_paper MCP tool + no-persistence orchestrator
- *(slice-4)* [**breaking**] CanonicalRef impl + provenance log v1->v2 migration
- *(slice-3)* safekey reference vectors 13 -> 100 + real CI parity
- *(slice-2)* MCP doiget_fetch_paper + doiget_batch_fetch wired
- *(slice-1)* metadata_only orchestrator + arxiv Atom feed parse
- incorporate musaabhasan feedback from Discussion #12
- *(cli)* OA PDF fetch from DOI via Unpaywall best_oa_location (Phase 1) ([#78](https://github.com/sotashimozono/doiget/pull/78))
- *(cli)* doiget audit-log --verify (Phase 1) ([#74](https://github.com/sotashimozono/doiget/pull/74))
- *(cli)* doiget fetch <ref> orchestrator (Phase 1) ([#72](https://github.com/sotashimozono/doiget/pull/72))
- *(sources)* Unpaywall source impl (Phase 1 Tier 1) ([#69](https://github.com/sotashimozono/doiget/pull/69))
- *(sources)* arXiv source impl (Phase 1 Tier 1) ([#68](https://github.com/sotashimozono/doiget/pull/68))
- *(sources)* Crossref source impl (Phase 1 Tier 1) ([#67](https://github.com/sotashimozono/doiget/pull/67))
- *(core)* Store trait + Metadata + FsStore impl (Phase 1) ([#66](https://github.com/sotashimozono/doiget/pull/66))
- *(core)* CapabilityProfile::from_env real impl (Phase 1) ([#65](https://github.com/sotashimozono/doiget/pull/65))
- *(core)* Source trait + FetchContext + FetchResult + FetchError (Phase 1) ([#64](https://github.com/sotashimozono/doiget/pull/64))
- *(core)* provenance log writer (JSON Lines + SHA-256 chain) ([#61](https://github.com/sotashimozono/doiget/pull/61))
- *(core)* rate limiter (5/sec global + 200ms per-source backoff) ([#63](https://github.com/sotashimozono/doiget/pull/63))
- *(core)* centralized HTTP client with security defaults ([#62](https://github.com/sotashimozono/doiget/pull/62))
- *(core)* Doi::parse + ArxivId::parse + Ref::parse with validation (Phase 1) ([#55](https://github.com/sotashimozono/doiget/pull/55))
- *(core)* Safekey derivation per docs/SAFEKEY.md (Phase 1) ([#39](https://github.com/sotashimozono/doiget/pull/39))

### Fixed

- *(ci)* green up posture-lint, rustdoc; let Windows clippy re-run
- address PR #84 multi-agent review findings (C1, C2, I1-I7)
- *(ci)* allow expect/unwrap in tests; allow CDLA-Permissive-2.0
- address re-review findings (serde transparent, ADR status, CI alignment)
- address PR-review findings (encapsulation, non_exhaustive, ADR stubs, CI)

### Other

- rustfmt fixes for tdm_elsevier.rs and tier_3_elsevier_allowlist
- Merge branch 'feat/slice-18-tdm-aps' into feat/slice-19-tdm-elsevier
- rustfmt fixes for tdm_aps.rs
- *(slice-6)* real-world DOI fixture set
- *(slice-5)* apply 7 advisory refactors from PR #84 review
- 4 design refinements from post-incorporation review
- *(fuzz)* cargo-fuzz harness for Doi/ArxivId/Ref::parse + smoke CI ([#59](https://github.com/sotashimozono/doiget/pull/59))
- *(security)* assert no outbound network in Phase 0 tests ([#60](https://github.com/sotashimozono/doiget/pull/60))
- *(core)* defensive vector count + truncation branch coverage ([#48](https://github.com/sotashimozono/doiget/pull/48))
- *(doiget-core)* add per-crate README for crates.io presentation ([#41](https://github.com/sotashimozono/doiget/pull/41))
- *(review)* philosophy/structure/drift fixes from doc review round 2
- Phase 0 skeleton — repo structure, normative specs, ADR scaffolding

Phase 0 (design + scaffolding). No version tag is published in this phase; the
workspace stays at `0.0.0` until Phase 6. See [docs/PHASES.md](docs/PHASES.md)
for the full Phase 0 deliverable checklist.

**Roadmap close-out.** Slice 6 lands the final piece of the
six-slice Phase-1 follow-up roadmap (Slice 1: metadata-only +
arxiv Atom; Slice 2: MCP `doiget_fetch_paper` + `doiget_batch_fetch`;
Slice 3: 100-entry safekey reference vectors; Slice 4: CanonicalRef +
provenance v1→v2 migration; Slice 5: PR #84 advisory refactors;
Slice 6: real-world fixture set). With this slice merged the
roadmap is complete; subsequent work tracks back to the normal
phase plan in [docs/PHASES.md](docs/PHASES.md).

**Phase 3 close-out begins.** Post-roadmap, the MCP tool surface
returns to the Phase 3 baseline (`docs/MCP_TOOLS.md` §1 — ten tools).
Five tools were wired during Slice 1 / Slice 2 (`doiget_health`,
`doiget_capability_profile`, `doiget_metadata_only`,
`doiget_fetch_paper`, `doiget_batch_fetch`); Slice 7 onward closes
out the remaining five (`doiget_resolve_paper`, `doiget_info`,
`doiget_search_local`, `doiget_list_recent`, `doiget_paper_pdf_path`).

### Slice 22 — OIDC crates.io trusted-publishing

Phase 6 continuation. Turns on the `release` side of release-plz
so the workflow now (a) pushes an annotated git tag, (b) opens a
GitHub release with the CHANGELOG section as the body, and (c)
publishes each crate to crates.io — using OIDC trusted-publishing
instead of a long-lived `CARGO_REGISTRY_TOKEN`.

- **`release-plz.toml`**: `publish` and `git_release_enable` flipped
  to `true`. `publish_no_verify` stays on (CI already builds every
  commit; the registry-side dry-run is redundant).
- **`.github/workflows/release-plz.yml`**: split into two jobs.
  - `release-plz-pr`: unchanged behaviour, narrowed permissions
    (`contents: write` + `pull-requests: write` only — no
    `id-token`).
  - `release-plz-release` (new): runs after the PR job, has
    `id-token: write` so release-plz can mint a short-lived
    crates.io token via OIDC. Idempotent — on non-release pushes
    the step is a no-op.
  - Both jobs now use the canonical `release-plz/action@SHA` ref
    (the prior `MarcoIeni/release-plz-action` is a redirect; same
    SHA, same release).
  - Workflow-level `permissions: contents: read` is the new least-
    common-denominator; each job widens only what it needs.

**Prerequisite (manual, one-time, before merge or first release-PR
merge):** the three crates (`doiget-core`, `doiget-cli`,
`doiget-mcp`) must be registered as Trusted Publishers on
crates.io. Without this, the `release` job will fail to publish.
Per crates.io's policy, the FIRST publish of each new crate has to
be done manually (Trusted Publishing only works for existing
crates).

### Slice 21 — release-plz integration (Phase 6 foundation)

First Phase-6 slice. Wires `release-plz` so every push to `main`
opens or updates a single "release PR" that bumps the workspace
version (currently `0.0.0`) and prepends a versioned section to
`CHANGELOG.md`. Tagging, GitHub releases, and `cargo publish` are
intentionally NOT enabled in this slice — those land alongside
OIDC trusted-publishing and sigstore signing in subsequent Phase 6
slices.

- **New** `release-plz.toml` at the repo root. `git_release_enable
  = false`, `publish = false`, `publish_no_verify = true`,
  `changelog_path = "CHANGELOG.md"`. Lists `doiget-core` /
  `doiget-cli` / `doiget-mcp` as managed packages (they share a
  single workspace version).
- **New** `.github/workflows/release-plz.yml`. Triggers on `push:
  main` and `workflow_dispatch`. Permissions: `contents: write` +
  `pull-requests: write` (only enough to open / update the release
  PR). Concurrency group `release-plz-${{ github.ref }}` prevents
  duplicate runs. SHA-pinned actions:
  - `actions/checkout@de0fac2e…` (v6.0.2) with `fetch-depth: 0`
    so release-plz can walk the full conventional-commit history.
  - `dtolnay/rust-toolchain@29eef336…` (stable, for the workspace
    `cargo` invocation release-plz makes internally).
  - `MarcoIeni/release-plz-action@064f4d1e…` (v0.5.129) with
    `command: release-pr` — never `release`, so it cannot tag or
    publish.

### Slice 20 — Per-source HTTP header hook (Phase 5 follow-up)

Closes the Slice 18/19 known-limitation by letting Tier-3 TDM
sources attach authentication headers on the wire.

- **New API** `HttpClient::fetch_bytes_with_headers(source, url,
  headers: &[(&str, &str)])` (`crates/doiget-core/src/http.rs`).
  Header names/values are validated up-front against the
  visible-ASCII subset (RFC 7230 §3.2); invalid headers return
  the new `HttpError::InvalidHeader { name, reason }` variant
  before the request is sent. `fetch_bytes` / `fetch_pdf` keep
  their existing signatures and pass `&[]` internally — no caller
  needs to change.
- **APS source** now sends `X-API-Key: $DOIGET_KEY_APS` on the
  outgoing GET. Wiremock happy-path test asserts the header is
  present (`header("x-api-key", TEST_KEY)` matcher); removing the
  header would now fail the test.
- **Elsevier source** now sends `X-ELS-APIKey: $DOIGET_KEY_ELSEVIER`
  on the outgoing GET, with the matching `header("x-els-apikey",
  TEST_KEY)` wiremock assertion.
- **`HttpError::InvalidHeader`** is mapped to `None` in the
  `From<&HttpError> for Option<DenialContext>` table — it is a
  caller-bug signal, not an ADR-0023 denial, and collapses to
  `ErrorCode::InternalError` via the existing wildcard arm in
  `From<HttpError> for ErrorCode`.
- **Stale notes removed**: the "header not on wire" `NOTE:` blocks
  inside `tdm_aps.rs` / `tdm_elsevier.rs::fetch` and the
  `KEY_ENV_VAR` doc-comments now describe the wired behaviour.

### Slice 19 — Elsevier ScienceDirect TDM source (Phase 5c)

Third Phase-5 / Tier-3 slice — closes the Phase 5a/b/c trio. Adds
the `tdm-elsevier` source: a metadata-only Elsevier ScienceDirect
TDM fetcher that turns a DOI into the
`{full-text-retrieval-response: {coredata, ...}}` envelope from
`/content/article/doi/<DOI>?httpAccept=application/json`. Whole
module compile-gated by the `tdm-elsevier` Cargo feature.

- **New module** `crates/doiget-core/src/sources/tdm_elsevier.rs`,
  declared in `sources/mod.rs` under
  `#[cfg(feature = "tdm-elsevier")] pub mod tdm_elsevier;`.
- **Three-gate activation**: Cargo feature `tdm-elsevier` compiled
  in + `DOIGET_KEY_ELSEVIER=<api-key>` +
  `DOIGET_AGREE_TDM_ELSEVIER=1`.
- **Transport gate**: new `tier_3_elsevier_allowlist()` in
  `crates/doiget-core/src/http.rs` mapping `"tdm-elsevier"` to
  `api.elsevier.com` + `*.elsevier.com`.
- **Provenance**: emits `LogEvent::Fetch` rows with
  `capability: Capability::TdmElsevier`.
- **Metadata-only**: `FetchResult.pdf_bytes` is always `None`.
- **Known limitation** (shared with Slice 18): Elsevier requires
  `X-ELS-APIKey`. `HttpClient` has no per-source header hook yet, so
  the header is NOT attached on the wire. Wiremock tests pass with
  header matching disabled. A follow-up slice will add the hook
  used by BOTH APS and Elsevier.
- **Tests**: three wiremock cases — happy path (DOI percent-encoded
  in path + `httpAccept=application/json` query param), no-grant
  `NotEligible`, missing-wrapper `SourceSchema`.
  `#[serial_test::serial]` because the happy-path mutates
  `DOIGET_KEY_ELSEVIER`.

### Slice 18 — APS Harvest TDM source (Phase 5b)

Second Phase-5 / Tier-3 slice. Adds the `tdm-aps` source: a
metadata-only APS Harvest TDM fetcher that turns a DOI into the
single article record from `/v2/article/<DOI>`. Whole module
compile-gated by the `tdm-aps` Cargo feature.

- **New module** `crates/doiget-core/src/sources/tdm_aps.rs`,
  declared in `sources/mod.rs` under
  `#[cfg(feature = "tdm-aps")] pub mod tdm_aps;`.
- **Three-gate activation**: Cargo feature `tdm-aps` compiled in +
  `DOIGET_KEY_APS=<api-key>` + `DOIGET_AGREE_TDM_APS=1`.
- **Transport gate**: new `tier_3_aps_allowlist()` in
  `crates/doiget-core/src/http.rs` mapping `"tdm-aps"` to
  `harvest.aps.org` + `*.aps.org`.
- **Provenance**: emits `LogEvent::Fetch` rows with
  `capability: Capability::TdmAps`.
- **Metadata-only**: `FetchResult.pdf_bytes` is always `None`.
- **Known limitation**: APS expects the API key in the `X-API-Key`
  header. `HttpClient` does not yet expose a per-source header hook,
  so the header is NOT attached on the wire in this slice — wiremock
  tests pass with header matching disabled. The wiring will be added
  alongside Slice 19 (Elsevier needs the same hook). See in-file
  TODO and `docs/SOURCES.md` §4 follow-up.
- **Tests**: three wiremock cases — happy path (DOI percent-encoded
  in path), no-grant `NotEligible`, non-object response
  `SourceSchema`. `#[serial_test::serial]` because the happy-path
  mutates `DOIGET_KEY_APS`.

### Slice 17 — Springer Nature OA TDM source (Phase 5a)

First Phase-5 / Tier-3 slice. Adds the `tdm-springer` source: a
metadata-only Springer Nature TDM fetcher that turns a DOI into the
first matching `records[]` entry from `/openaccess/json`. Whole
module compile-gated by the `tdm-springer` Cargo feature so default
release binaries never include the host pattern or env-var read
path (ADR-0002).

- **New module** `crates/doiget-core/src/sources/tdm_springer.rs`,
  declared in `sources/mod.rs` under
  `#[cfg(feature = "tdm-springer")] pub mod tdm_springer;`.
- **Three-gate activation** (`docs/CAPABILITY.md` §2): Cargo feature
  `tdm-springer` compiled in + `DOIGET_KEY_SPRINGER=<api-key>` +
  `DOIGET_AGREE_TDM_SPRINGER=1`. `can_serve` checks
  `profile.tdm_springer.is_some()`; `fetch` re-checks the grant AND
  re-reads the key env var defensively, fail-closing as
  `NotEligible` if either is missing at fetch time.
- **Transport gate**: new `tier_3_springer_allowlist()` in
  `crates/doiget-core/src/http.rs` (also feature-gated) maps the
  source key `"tdm-springer"` to `api.springernature.com` plus the
  `*.springernature.com` wildcard. The orchestrator unions this into
  the active allowlist only when the feature is on.
- **Provenance**: emits `LogEvent::Fetch` rows with
  `capability: Capability::TdmSpringer` (already defined in
  `provenance.rs` from Phase 0).
- **Metadata-only**: `FetchResult.pdf_bytes` is always `None` for
  Phase 5a. Following the OA PDF link in the returned record is
  deferred until the eight ADR-0019 safeguards are wired through the
  orchestrator.
- **Tests**: three wiremock cases — happy path (asserts
  `?q=doi:...&api_key=...` query params), no-grant `NotEligible`,
  empty-`records` `SourceSchema`. `#[serial_test::serial]` because
  the happy-path test mutates `DOIGET_KEY_SPRINGER`.

### Slice 16 — `doiget graph <ref>` CLI subcommand (Phase 4)

Final Phase-4 slice. Adds the `doiget graph <ref>` subcommand that
wraps `doiget_core::citation_graph::expand` and emits the result
as pretty-printed JSON on stdout. Mirrors the
`doiget_expand_citation_graph` MCP tool (Slice 15) wire shape.

- **New module** `crates/doiget-cli/src/commands/graph.rs`, declared
  in `commands/mod.rs` under
  `#[cfg(feature = "citation")] pub mod graph;`. Default build
  (`oa-only`) excludes the module entirely.
- **CLI surface**:
  `doiget graph <ref> [--depth N] [--total N] [--per-paper N]`
  (feature-gated `Command::Graph` variant). DOI seeds only; arXiv
  ids are rejected at the orchestrator layer.
- **`build_http_client` fix**: production path now also unions
  `tier_2_allowlist()` (gated on the `citation` feature) so the
  `openalex` source key passes the redirect closure. Test path
  recognizes `DOIGET_OPENALEX_BASE` env var for wiremock routing.
  Mirrors the parallel fix applied to `doiget-mcp/src/lib.rs`
  during Slice 15.
- **Output**: pretty JSON of `GraphResult { seed_work_id, nodes,
  edges, truncated, total_visited }` on stdout. Uses
  `writeln!(stdout().lock(), ...)` per `docs/SECURITY.md` §3 (the
  workspace `print_stdout` lint is denied; `writeln!` against an
  explicit `stdout().lock()` is the sanctioned escape hatch).
- **2 e2e tests** in new `tests/graph_e2e.rs` (whole file
  `#![cfg(feature = "citation")]`-gated): subprocess run via
  `assert_cmd` against a wiremocked OpenAlex; asserts the stdout
  JSON shape (`seed_work_id`, `total_visited`, `nodes` / `edges`
  array lengths, `truncated`). Plus a non-async test that
  rejects arXiv seeds with non-zero exit.

This closes Phase 4. Eleven Phase-4-baseline MCP tools wired,
3 Tier 2 metadata sources implemented, citation-graph BFS
orchestrator with ADR-0010 hard caps in place, and the
`doiget graph` CLI subcommand now lets users walk graphs
without standing up an MCP host.

### Slice 15 — `doiget_expand_citation_graph` MCP tool (Phase 4)

Wires the 11th MCP tool (Phase 4 from `docs/MCP_TOOLS.md` §1).
The tool always advertises in `tools/list`; the body returns
`NOT_IMPLEMENTED` when this binary was built without the `citation`
Cargo feature, and runs the live BFS expansion when it was.

- **New `citation` Cargo feature** on `doiget-mcp` that turns on
  `doiget-core/citation` (which itself enables `doiget-core/metadata`,
  pulling in `OpenalexSource`).
- **`doiget_expand_citation_graph(ref, depth?, total?, per_paper?)`**
  tool method on `Server`. Always present in the type system —
  the `#[tool_router]` macro can't see cfg-gated methods, so the
  feature gate lives only in the body. `ExpandCitationGraphInput`
  is similarly unconditional.
- **Wire envelope** (success):
  `{ ok: true, ref, seed_work_id, nodes, edges, truncated, total_visited }`.
  Error path uses the existing `read_path_error_envelope` shape,
  mapping `GraphError::CapabilityDenied` → `CAPABILITY_DENIED`,
  `SeedNotIndexed` → `NO_OA_AVAILABLE`, `Log` → `LOG_ERROR`,
  `Source` → `NETWORK_ERROR`.
- **`build_fetch_context` HTTP allowlist update**: production path
  now unions `tier_2_allowlist()` (from Slice 10) so the `openalex`
  source key is accepted by the redirect closure. Test path
  recognizes `DOIGET_OPENALEX_BASE` env var for wiremock routing.
- **`tools/list` assertion** added to `initialize_handshake.rs`.
- **3 e2e tests** in new `tests/expand_citation_graph_e2e.rs`
  (whole file `#![cfg(feature = "citation")]`-gated): a 3-node
  wiremocked graph (W0001 → W0002, W0003), invalid-ref →
  `INVALID_REF`, arXiv seed → `INVALID_REF`.

### Scope deferred to Slice 15b

`doiget_bibtex_export` and `doiget_csl_export` were originally part of
Slice 15 but defer to a follow-up slice because they require new
BibTeX/CSL renderer helpers in `doiget-core::store::metadata` that
the CLI's `bib.rs` / `csl.rs` currently keep CLI-internal. Slice
15b will move those renderers into `doiget-core` and add the two
MCP tools as thin wrappers over `Store::read + renderer`.

### Slice 14 — Citation graph BFS expansion (ADR-0010, Phase 4)

Citation-graph orchestrator backing the upcoming
`doiget_expand_citation_graph` MCP tool (Slice 15) and `doiget graph`
CLI subcommand (Slice 16).

- **New module** `crates/doiget-core/src/citation_graph.rs`,
  compile-gated by the `citation` Cargo feature (which itself
  enables `metadata` so `OpenalexSource` is available).

- **`expand(seed_doi, caps, source, profile, ctx)`** runs a BFS
  walk via OpenAlex. The seed `Doi` is resolved through
  `OpenalexSource::fetch` (so the seed lands in the audit trail
  via the documented path); subsequent works are fetched directly
  via `ctx.http.fetch_bytes("openalex", url)` for Work-ID lookups
  (the redirect allowlist already permits the `openalex` source
  key from Slice 10). Each successful fetch appends one
  `LogEvent::Fetch` row under `Capability::Metadata`. Failed
  fetches log `LogResult::Err` rows and continue the walk with
  `truncated = true`.

- **ADR-0010 hard caps enforced via `GraphCaps::clamped`**:
  `MAX_DEPTH = 3`, `MAX_TOTAL = 100`, `MAX_PER_PAPER = 20`. Any
  caller-supplied value is clamped before walking — this is the
  load-bearing enforcement point per the ADR's binding contract.
  `truncated: true` is set on the result when any cap is hit.

- **Cycle detection** via `HashSet<String>` of visited Work IDs.
  Duplicate parents still get edges added (so structural cycles
  are visible in the result) but are not re-queued.

- **TDM-free invariant**: per ADR-0010, this module never consults
  any Tier 3 source. Even S2 / DOAJ are not used — only OpenAlex
  exposes `referenced_works[]` in a single round-trip, so the
  walker is OpenAlex-only by design.

- **New `GraphError` enum**: `Source(FetchError)`, `Log(LogError)`,
  `SeedNotIndexed`, `CapabilityDenied`. Provenance-log failures
  abort the expansion (fail-closed per
  `docs/PROVENANCE_LOG.md` §5).

- **`DOIGET_OPENALEX_BASE` env var** is read at Work-ID fetch
  time so wiremock tests can swap the origin. Production callers
  leave the env unset and the default `https://api.openalex.org`
  applies.

- **3 unit tests** in `citation_graph::tests` green:
  `caps_clamps_to_adr_0010_maxima`, `expand_walks_depth_2_graph`
  (a 4-node wiremocked graph: W0001 seed → W0002/W0003 → W0004),
  `expand_without_capability_flag_errors`.

### Slice 11 — OpenAlex source implementation (Phase 4 / Tier 2)

First concrete Tier 2 source. Adds `OpenalexSource` behind the
`metadata` Cargo feature gate plus runtime capability check
(`profile.metadata.openalex`).

- **New module** `crates/doiget-core/src/sources/openalex.rs`
  declared in `sources/mod.rs` under
  `#[cfg(feature = "metadata")] pub mod openalex;`. Default build
  (`oa-only`) excludes the module entirely so no Tier 2 code paths
  ship in the default release binary.

- **Production constructor `OpenalexSource::new(contact_email)`**
  hard-codes `https://api.openalex.org` as the base URL.
  **Test-only constructor `with_base`** lets wiremock substitute an
  `http://127.0.0.1:N` origin via a future `DOIGET_OPENALEX_BASE`
  env var (orchestrator wiring lands in a follow-up).

- **`Source` impl wire shape:**
  - `name() = "openalex"`
  - `can_serve(profile, ref_) = profile.metadata.openalex && Ref::Doi(_)`
  - `fetch`: `GET <base>/works/<doi>?mailto=<contact>`, parses the
    Work record JSON, emits one `LogEvent::Fetch` provenance row
    under `Capability::Metadata` (per `docs/PROVENANCE_LOG.md` §3),
    returns `FetchResult { source: "openalex", license: "unknown",
    pdf_bytes: None, metadata_json: Some(work) }`.
  - Metadata-only contract: `pdf_bytes` is always `None`
    (`docs/SOURCES.md` §4).

- **Defensive shape check**: an OpenAlex response missing the `id`
  field is treated as an error payload and surfaces as
  `FetchError::SourceSchema` with the first 200 chars of the body
  in the hint.

- **Defense-in-depth capability gate**: even if `can_serve` is
  bypassed, `fetch` rejects with `FetchError::NotEligible` when
  `profile.metadata.openalex == false`.

- **4 unit tests** in `sources::openalex::tests` (all green):
  happy path (asserts `display_name` + `referenced_works[0]`),
  arXiv ref rejection, capability-flag-off rejection, malformed
  response → `SourceSchema`.

### Slice 12 — Semantic Scholar source implementation (Phase 4 / Tier 2)

Second concrete Tier 2 source. Adds `S2Source` behind the `metadata`
Cargo feature gate. Same shape as `OpenalexSource` (Slice 11) with
S2-specific differences:

- **Endpoint**: `GET <base>/graph/v1/paper/DOI:<doi>?fields=title,year,citationCount,references`
- **Optional `api_key`**: stored as `Option<String>`; absent means the
  request is sent unauthenticated (S2's public Graph API rate limit
  applies). The `x-api-key` header is not yet threaded through
  `HttpClient::fetch_bytes` — adding it is a follow-up; the
  `api_key` field exists to reserve the API surface so a future
  per-request header hook lands without changing constructors.
- **Defensive shape check**: an S2 response missing the `paperId`
  field surfaces as `FetchError::SourceSchema`.
- **`Source` impl**: `name() = "semantic_scholar"`,
  `can_serve = profile.metadata.semantic_scholar && Ref::Doi(_)`,
  `fetch` emits one provenance row under `Capability::Metadata` and
  returns `pdf_bytes: None` (metadata-only contract per
  `docs/SOURCES.md` §4).

2 unit tests in `sources::s2::tests` green: happy path (asserts
`title` + `references[0].paperId`), capability-flag-off rejection.

### Slice 13 — DOAJ source implementation (Phase 4 / Tier 2)

Third concrete Tier 2 source. Adds `DoajSource` behind the
`metadata` Cargo feature gate. DOAJ has no direct DOI-lookup
endpoint, so doiget queries the article search API and takes the
first result.

- **Endpoint**: `GET <base>/api/search/articles/doi:<doi>?pageSize=1`
  (Lucene-style `doi:` filter; DOI suffix is percent-encoded but the
  `doi:` separator stays literal).
- **`Source` impl**: `name() = "doaj"`,
  `can_serve = profile.metadata.doaj && Ref::Doi(_)`, `fetch` emits
  one provenance row under `Capability::Metadata` and returns
  `pdf_bytes: None` (metadata-only contract per
  `docs/SOURCES.md` §4).
- **Empty results → `FetchError::SourceSchema`** with a synthetic
  "doaj search returned 0 results for this DOI" message, so the
  orchestrator's Tier 2 fallback chain can move on to the next
  source cleanly.
- **`percent_encode_path_segment` helper**: hand-rolled (no
  `percent-encoding` crate) to keep the dependency surface stable;
  preserves the RFC 3986 unreserved set + `:` for the Lucene
  separator.

3 unit tests in `sources::doaj::tests` green: happy path (asserts
`bibjson.title`), empty-results-returns-SourceSchema, capability-
flag-off rejection.

### Slice 10 — Tier 2 redirect-allowlist scaffolding (Phase 4 starts)

First Phase-4 slice: lands the redirect-allowlist data for the three
Tier 2 metadata sources (`docs/SOURCES.md` §1 Tier-2 row). No source
impls yet — subsequent slices add OpenAlex (11), Semantic Scholar
(12), and DOAJ (13) concretely.

- **New `tier_2_allowlist()` function** in
  `crates/doiget-core/src/http.rs`. Sibling to the existing
  `tier_1_allowlist()` and `oa_publisher_allowlist()`. Returns three
  `SourceAllowlist` entries with the production hosts:
  - `"openalex"` → `api.openalex.org`
  - `"semantic_scholar"` → `api.semanticscholar.org`
  - `"doaj"` → `doaj.org` + `*.doaj.org`

- **No behavioral change yet.** The function is declared but not
  consumed by any source impl. Tier 2 source impls (Slice 11/12/13)
  will pass this list into `HttpClient::new` so the redirect closure
  denies off-list hosts under each Tier 2 source key.

- **Capability gate, unchanged.** `CapabilityProfile.metadata.{openalex,
  semantic_scholar, doaj}` and the `DOIGET_ENABLE_OPENALEX` /
  `DOIGET_ENABLE_S2` / `DOIGET_ENABLE_DOAJ` env vars were already
  wired during Phase 0; this slice does not touch them.

- **No new tests in this slice.** `tier_2_allowlist()` is pure data;
  a sibling unit test (mirroring `tier_1_allowlist_includes_crossref`)
  lands in Slice 11 alongside the first concrete source impl, so the
  assertion has a producer to protect.

### Slice 8 — Read-path MCP tools (4 tools)

Wires the four 100% local read-path MCP tools from the
`docs/MCP_TOOLS.md` §1 baseline. These tools never touch the
network, never write to the store, and never append provenance
rows — they expose the existing `Store` trait surface
(`Store::read`, `Store::list_recent`, `Store::search`) through
JSON-RPC.

- **`doiget_info(ref)`** — read the metadata TOML for a stored
  entry. Returns `{ ok: true, ref, safekey, metadata: <object>|null }`.
  A missing entry surfaces as `metadata: null` on a success
  envelope (not an error envelope) — the closed `ErrorCode` set
  has no `NotFound` variant, so the null-payload convention keeps
  the wire surface consistent with how `doiget_paper_pdf_path`
  reports a missing PDF.

- **`doiget_search_local(query, limit?)`** — case-insensitive
  substring search over title / authors / venue / publisher.
  Backed by `Store::search`, which today is a linear scan over
  `<root>/.metadata/*.toml` (a Phase 2+ tantivy or sqlite-fts
  index swaps in transparently behind the trait). `limit` defaults
  to 50 and is clamped to a maximum of 200.

- **`doiget_list_recent(limit?)`** — most-recently fetched entries
  by `[doiget].fetched_at` (RFC3339 UTC, `%Y-%m-%dT%H:%M:%SZ`).
  `limit` defaults to 50, capped at 200.

- **`doiget_paper_pdf_path(ref)`** — return the absolute path of a
  cached PDF if and only if the entry has one. **Never reads,
  parses, or transmits PDF content.** Returns
  `{ ok: true, ref, safekey, path: <string>|null, pdf_exists: bool }`.
  A missing metadata entry or a missing PDF file both surface as
  `path: null`. The path is computed as
  `<store_root>/<safekey>.pdf` and probed for existence with a
  single `Path::exists` call.

- **Input shape**
  `InfoInput { ref }`, `SearchLocalInput { query, limit? }`,
  `ListRecentInput { limit? }`, `PaperPdfPathInput { ref }`. All
  carry `schemars(deny_unknown_fields)` so an unknown wire field
  is rejected at the rmcp transport boundary.

- **No `dry_run` support** on any of these tools per
  `docs/MCP_TOOLS.md` §10 (`doiget_info`, `doiget_search_local`,
  `doiget_list_recent`, `doiget_paper_pdf_path` are in the "dry_run
  does not apply" set). The closed `deny_unknown_fields` schema is
  the enforcement point.

- **New e2e coverage**
  `crates/doiget-mcp/tests/read_path_e2e.rs` (6 tests, all
  green): two invalid-ref tests (`doiget_info`,
  `doiget_paper_pdf_path`), two empty-store tests
  (`doiget_search_local`, `doiget_list_recent`), and two
  no-entry success tests (`doiget_info`, `doiget_paper_pdf_path`).
  The empty-store path exercises an `FsStore` rooted at a
  `tempfile::TempDir` so the tests are hermetic and parallel-safe
  via `serial_test::serial` (env var mutation).

- **tools/list assertion update**
  `crates/doiget-mcp/tests/initialize_handshake.rs` now also
  asserts that all four Slice-8 tools appear in the `tools/list`
  response. All 6 existing handshake tests + 4 new assertions
  pass.

### Slice 7 — `doiget_resolve_paper` MCP tool

This slice wires the **sixth** Phase-3 tool: `doiget_resolve_paper`,
the audit-trail-preserving sibling of `doiget_metadata_only`. The new
tool resolves a DOI / arXiv id to live metadata through Crossref /
Unpaywall / arXiv (each consulted resolver still emits its own
`LogEvent::Fetch` provenance row, preserving the audit chain), but the
orchestrator MUST NOT write the metadata TOML to the store under any
code path — present or future. This is the binding contract that
distinguishes `resolve_paper` from `metadata_only`, codified directly
in the doc-comment on the new orchestrator function and re-stated in
the MCP tool description so an agent picking between the two tools
sees the difference without consulting the spec.

- **New core orchestrator**
  `doiget_core::orchestrator::resolve_only`. Today this delegates to
  `metadata_only` (which itself does not yet write to the store —
  the Phase 2.x TODO). The function's doc-comment fixes the
  future-divergence contract: when Phase 2.x lands the store-write
  for `metadata_only`, `resolve_only` MUST be refactored to call the
  inner dispatchers (`metadata_only_doi` + the arXiv-Atom path) with
  the store-write step excluded, NOT continue delegating. Splitting
  the function out as a named symbol now reserves the API slot so
  that future refactor lands purely inside `doiget-core` without
  touching the MCP tool wiring.

- **New MCP tool** `doiget_resolve_paper`
  (`crates/doiget-mcp/src/lib.rs`). Per-call semantics mirror
  `doiget_metadata_only`: the MCP server emits the
  `SessionStart` / `SessionEnd` bookend rows, each consulted
  `Source` emits its own `LogEvent::Fetch` row, and the orchestrator
  emits **no** `StoreWrite` row (no store mutation). `dry_run` is
  not a supported input field per `docs/MCP_TOOLS.md` §10/§211 — the
  new `ResolvePaperInput` struct uses `schemars(deny_unknown_fields)`
  so an attempted `dry_run` is rejected at the rmcp transport
  boundary before reaching the tool body. The tool description
  explicitly redirects agents to `metadata_only` with `dry_run: true`
  for preview use cases.

- **New e2e coverage**
  `crates/doiget-mcp/tests/resolve_paper_e2e.rs`:
  - `doiget_resolve_paper_invalid_ref_returns_invalid_ref_envelope`
    — a malformed `ref` collapses to the closed `INVALID_REF` error
    code via the same `Ref::parse` shim used by other tools.
  - `doiget_resolve_paper_arxiv_happy_path_returns_metadata_envelope`
    — an arXiv id is resolved through a wiremocked Atom feed; the
    success envelope carries `source = "arxiv"`,
    `license = "arxiv-default"`, `oa_url = null`, and the parsed
    metadata.
  - `doiget_resolve_paper_doi_crossref_happy_path_returns_metadata_envelope`
    — a DOI is resolved through a wiremocked Crossref response; the
    OA URL is extracted from `message.link[]` and surfaced in
    `oa_url`, `license` is `null` (Crossref does not carry a
    license; that channel is Unpaywall's).
  - All three tests carry the `// allow: outbound-network` posture
    marker; no `reqwest::*` imports are introduced — all HTTP
    terminates at `127.0.0.1` wiremock servers.

- **tools/list assertion update**
  `crates/doiget-mcp/tests/initialize_handshake.rs` now also asserts
  that `doiget_resolve_paper` appears in the `tools/list` response,
  matching the §1 table in `docs/MCP_TOOLS.md`.

- **No spec drift.** `docs/MCP_TOOLS.md` §1 already lists
  `doiget_resolve_paper` in the Phase-3 baseline table; this slice
  ships the implementation, not a new spec section. A follow-up
  documentation slice may add a §N normative subsection mirroring
  the `metadata_only` §11 / `fetch_paper` §4 detail blocks.

### Slice 9 — `mcp-smoke.yml` Phase-0 placeholder → real CI gate

Replaces the placeholder `mcp-smoke.yml` workflow with the actual
Phase-3 gate documented in `docs/MCP_TOOLS.md` §9. Two jobs:

- **`in-process-smoke`** — runs `cargo test -p doiget-mcp --tests`,
  exercising all rmcp tool-router methods via the in-process duplex
  pipe (`initialize_handshake`, `fetch_paper_e2e`, and any per-slice
  e2e binary that has landed — Slice 7 `resolve_paper_e2e` and
  Slice 8 `read_path_e2e` if those PRs have merged). Hermetic.

- **`stdout-purity`** — builds `doiget-cli` in release, spawns
  `target/release/doiget serve`, pipes
  `initialize` + `notifications/initialized` + `tools/list` to
  stdin, closes stdin, captures stdout, and asserts every non-blank
  line of stdout parses as a JSON object. This catches the failure
  mode that the in-process pipe cannot see: a banner / log / progress
  line accidentally written to the real stdout. Per
  `docs/SECURITY.md` §3, stdout is reserved for JSON-RPC frames
  only; this job is the load-bearing CI check for that invariant.

- **Logs uploaded as artifact** (`/tmp/mcp-smoke/`) on every run so a
  failed smoke is debuggable without re-running.

The previous workflow's `placeholder` job is replaced (it only
echoed a Phase-0 notice). The path filter is expanded to include
`crates/doiget-cli/**` and `crates/doiget-core/**` since the
subprocess-style probe depends on both crates.

### Slice 6 — Real-world DOI / arXiv fixture set

This slice curates a **frozen-snapshot fixture set** under
`tests/fixtures/real_world/` so the wiremock-driven test suite has
realistic Crossref / Unpaywall / arXiv response shapes to drive
through `doiget_core::orchestrator::metadata_only`. The set is
**closed and in-repo** — no live API is touched at test time, and
fixtures are refreshed only by deliberate human curation (see the
companion `README.md` for policy).

- **13 fixture entries** spanning 9 representative classes:
  - `doi-no-oa` (Crossref OK, `link[]` empty → no oa_url) — 1 entry
  - `doi-crossref` (Crossref OK with OA URL) — 6 entries covering
    Springer, PLOS, eLife, MDPI, Frontiers, and bioRxiv response
    shapes
  - `doi-crossref-fail-unpaywall` (Crossref 404 → Unpaywall fallback
    with license + OA URL) — 1 entry (Zenodo)
  - `doi-long-suffix` (safekey SHA-256 truncation boundary; 212-char
    suffix) — 1 entry
  - `doi-special-chars` (suffix with parens / slash / underscore →
    escape-collapse path in `Ref::safekey()`) — 1 entry
  - `arxiv-new` (modern `YYMM.NNNNN` id, Atom feed) — 1 entry
  - `arxiv-old` (`subject-class/NNNNNNN` id) — 1 entry
  - `arxiv-versioned` (`...vN` suffix) — 1 entry

- **New reference test**
  `crates/doiget-core/tests/real_world_fixtures_e2e.rs` walks
  `tests/fixtures/real_world/index.toml` and for each enabled
  `[[entry]]` mounts the frozen response on a `wiremock::MockServer`,
  points the orchestrator at it via the `DOIGET_CROSSREF_BASE` /
  `DOIGET_UNPAYWALL_BASE` / `DOIGET_ARXIV_BASE` env vars, and
  asserts `safekey`, `source`, `title`, `oa_url`, and `license`
  match the per-entry `expected.toml`. The test carries the
  `// allow: outbound-network` posture-lint marker (no `reqwest::*`
  imports; all traffic terminates at `127.0.0.1`).

- **Curation policy**
  ([`tests/fixtures/real_world/README.md`](tests/fixtures/real_world/README.md)):
  - Each fixture is `provenance = "hand-crafted"` (synthesized to
    match the documented API shape) or `"snapshot-from-real-api"`
    (captured once with `curl` then trimmed). The slice-6 set is
    entirely hand-crafted to side-step third-party redistribution
    ambiguity and keep each file ≤ 5 KB.
  - **Refresh is deliberate, not routine.** Refresh an entry only
    when (a) a test exposes a real upstream shape change, or (b) the
    entry's expected output is provably wrong. Document the refresh
    in the entry's `notes` field and bump `last_refreshed_iso`.
  - **No PDFs in this fixture set.** PDF licensing is publisher-
    specific; the synthetic `%PDF-fake-bytes` payloads in
    `crates/doiget-cli/tests/fetch_doi_oa_pdf_e2e.rs` and
    `crates/doiget-mcp/tests/fetch_paper_e2e.rs` cover the PDF leg.
  - The `disabled = true` per-entry flag is the escape hatch for
    keeping CI green while a snapshot is being updated.

- **Scope**
  - The fixture set covers the **metadata response shape**, not the
    PDF leg or the `fetch_paper` / `batch_fetch` store-write path
    — those are already exercised by Slice 1 / Slice 2 wiremock
    tests with synthetic payloads.
  - Entry count is intentionally bounded (target 10–15); the goal
    is "representative shapes covered", not "exhaustive corpus".

### Slice 5 — PR #84 review advisory refactors (code simplification)

This slice addresses the seven Advisory-tier findings (A2 - A8) from
the PR #84 multi-agent review. Every change is behavior-preserving and
internal-only — the public Rust API, the CLI wire surface, the MCP
tool envelopes, and the provenance-log shape are bit-identical before
and after this slice. (Advisory item A1 — `expected: Option<Vec<String>>`
— had already landed in PR #85 refinement #3 and required no further
work here.)

- **(A2 / A3)** Collapsed the single-field `FetchOptions { dry_run: bool }`
  / `BatchOptions { dry_run: bool }` option bundles and their
  back-compat `run(input) -> run_with_options(input, default)`
  wrappers into bare `dry_run: bool` parameters on
  `doiget_cli::commands::fetch::run_with_options` and
  `doiget_cli::commands::batch::run_with_options`. The struct shape
  was YAGNI and the wrappers only existed to spare tests a
  `..::default()` literal. Call sites (CLI `main.rs` clap dispatch,
  four `tests/*_e2e.rs` integration tests) updated in the same slice.

- **(A4)** Replaced the duplicate `build_test_client_for_http` helper
  inside `doiget_core::http::tests` with a one-line delegation to the
  public `HttpClient::new_for_tests_allow_http` constructor. The two
  paths had drifted into byte-identical re-implementations; the
  delegation keeps the security-load-bearing redirect-policy + builder
  in one place.

- **(A5)** Extracted the `struct EnvGuard` test fixture into the shared
  `crates/doiget-cli/tests/common/env_guard.rs` module (with
  `tests/common/mod.rs` declaring `pub mod env_guard;`). Four
  integration-test binaries (`fetch_arxiv_e2e`, `fetch_dry_run_e2e`,
  `fetch_doi_oa_pdf_e2e`, `batch_e2e`) had each defined a private
  `EnvGuard` with subtly different snapshot-and-restore behavior;
  consolidated on the strictly-safer snapshot-and-restore variant.

- **(A6)** Derived `FetchPlan::redirect_allowlists_loaded` from
  `tier_1_allowlist() + oa_publisher_allowlist()` instead of a
  hardcoded `vec!["crossref","unpaywall","arxiv","oa-publisher"]`.
  Wire output is bit-identical today; the change prevents future drift
  if a new allowlist source is added to the production HTTP client.

- **(A7)** Fixed a docstring section reference in
  `crates/doiget-mcp/src/lib.rs` — `metadata_only_error_envelope` now
  cites ADR-0023 §1 (the top-level optionality of `denial_context`)
  instead of §3 (which covers per-subfield optionality and applies
  only when `denial_context` is present).

- **(A8)** Tightened the doc comments on reserved
  `DenialReason::SchemaDrift`, `HostInBlockList`, `RateLimitWindow`,
  and `SsrfPrivateAddress` variants — each now states `Reserved — no
  producer wired yet. Will be emitted by <future component> once that
  component lands.` so the "unused variant" status is explicit on the
  public API surface.

### Slice 4 — CanonicalRef impl + provenance log v1→v2 migration (BREAKING)

This slice ships the audit-identity layer that [ADR-0021](docs/DECISIONS/0021-canonical-tuple-identity.md)
reserved as spec-only at Phase 1 and lands as
[ADR-0024](docs/DECISIONS/0024-canonical-ref-impl.md). The
provenance-log row shape changes; existing v1 logs MUST be migrated
before this binary will read them.

- **(E.1)** New public types `doiget_core::CanonicalRef` and
  `doiget_core::SourceType` re-exported from the crate root per
  [`docs/PUBLIC_API.md`](docs/PUBLIC_API.md) §1 + §9. The digest
  algorithm is the NORMATIVE
  `SHA256(source_type | 0x00 | source_id | 0x00 | resolver_profile | 0x00 | version_or_empty)`
  from ADR-0021 §1 — `version_or_empty` is the empty byte sequence
  when `version` is `None`, NOT a sentinel. Added
  `impl Ref { pub fn promote(&self, resolver_profile: &str, version: Option<&str>) -> CanonicalRef }`
  as the ergonomic construction path. 16 golden digest vectors in
  `crates/doiget-core/src/canonical.rs::tests` cross-check the
  streaming impl against an in-test reference SHA-256
  reimplementation.

- **(E.2)** **BREAKING** — provenance log schema bump v1 → v2. New
  `pub const doiget_core::provenance::LOG_SCHEMA_VERSION: &str = "v2"`.
  Every `LogRow` now carries two new fields:
  - `schema_version: String` (literal `"v2"`).
  - `canonical_digest: Option<String>` (64 lowercase hex chars, or
    `null` on session bookend rows).
  Both fields participate in the SHA-256 hash chain. The lex-first
  top-level key of the canonical-JSON shifts from `capability` to
  `canonical_digest` (n<p at byte index 2). `#[serde(deny_unknown_fields)]`
  + non-defaulted `schema_version` mean v1 rows fail to parse loudly
  rather than producing silent hash mismatches.
  [`docs/PROVENANCE_LOG.md`](docs/PROVENANCE_LOG.md) §3 + new §3.1
  document the wire surface and migration recipe.

- **(E.3)** One-shot migration:
  `doiget_core::provenance::migrate_v1_to_v2(log_path, dry_run) -> Result<MigrationReport, LogError>`.
  Idempotent (re-running on a v2 log is a no-op) and dry-runnable.
  Live runs stage to `<log_path>.v2-migrated`, verify the staged file
  passes `verify()`, back up the original to `<log_path>.v1-backup`,
  then atomically rename onto the live path. Exposed via the CLI as
  `doiget provenance migrate [--dry-run]`
  (`crates/doiget-cli/src/commands/provenance.rs`).

- **(E.4)** `resolver_profile` threaded through every Fetch /
  StoreWrite provenance-log write. Crossref, Unpaywall, and arXiv
  source impls now mint a `CanonicalRef` under their own resolver
  name; the orchestrator mints a distinct digest under
  `"oa-publisher"` for the DOI PDF leg. A single DOI fetch through
  Crossref + Unpaywall + oa-publisher therefore produces THREE
  distinct `canonical_digest` values in the audit log, matching
  ADR-0021 Context §2.

- **(E.5)** MCP envelope additions per ADR-0021 §4:
  - `doiget_fetch_paper` result envelope gains a `resolver_profile`
    string field.
  - `doiget_metadata_only` result envelope gains a `resolver_profile`
    string field.
  - `doiget_batch_fetch` per-row entries gain a `resolver_profile`
    string field on success rows.
  In Slice 4 the field equals `source` verbatim; kept distinct so
  future slices can decouple "which resolver wrote to disk" from
  "which resolver is the audit identity". `docs/MCP_TOOLS.md` §5 +
  §11 typescript unions updated.

- **(E.6)** [ADR-0024](docs/DECISIONS/0024-canonical-ref-impl.md)
  (Accepted) supersedes [ADR-0021](docs/DECISIONS/0021-canonical-tuple-identity.md)'s
  spec-only posture for implementation; the §1–§4 NORMATIVE shape of
  ADR-0021 remains binding. INDEX updated.

- **(E.7)** Golden migration fixture at
  `tests/fixtures/provenance/migration_v1_to_v2.json` (7 representative
  v1 rows: session bookends, Crossref / Unpaywall / oa-publisher /
  arXiv fetch legs, a StoreWrite, and a Resolve err for an invalid
  ref). Four end-to-end migration tests in
  `crates/doiget-core/tests/provenance_migration_e2e.rs` assert
  dry-run preview correctness, byte-equality of each row's
  `canonical_digest` against the independent
  `CanonicalRef::new(...).digest_hex()` path, idempotency on
  re-run, and that a dry-run preview on a v2 log does not touch
  disk.

- **(E.8)** This CHANGELOG entry.

- **(E.9)** Test coverage added: 16 canonical-digest goldens, 4
  migration e2e tests, and the existing source / orchestrator /
  MCP / CLI test suites updated to thread `canonical_digest`
  through every `RowInput` construction site (orchestrator
  StoreWrite + oa-publisher Fetch, all three Source impls, MCP
  session bookends, CLI session bookends, batch Resolve err). All
  192+ pre-existing tests stay green; no behavioral regressions.

**BREAKING.** Existing v1 access logs at `~/.config/doiget/access.log`
MUST be migrated via `doiget provenance migrate` before this binary
will read them. The audit-log verifier rejects unmigrated v1 rows
with a `corrupted log at line N` error.

No new runtime dependencies. `hex` and `sha2` were already in the
workspace deps (used by `safekey` truncation and existing log hashing).

### Slice 3 — safekey 100 reference test vectors

- **(D.1)** Expanded `tests/fixtures/safekey/vectors.json` from the
  13-entry Phase 0 placeholder to the full NORMATIVE 100-entry set
  declared by [docs/SAFEKEY.md](docs/SAFEKEY.md) §5 and ADR-0007.
  Vectors are grouped by purpose so every branch of the algorithm
  (`docs/SAFEKEY.md` §3) is exercised:
  - 25 × Group A — canonical DOI mapping (varied registrant widths
    4-7 digits, slash/dot/dash/mixed-case suffixes, real-publisher-shape
    patterns from synthetic Crossref test prefixes).
  - 25 × Group B — escape/collapse/trim edges: spaces, `+`, `;`, `:`,
    `,`, `&`, `=`, `?`, `#`, `*`, `|`, parentheses/brackets/braces,
    extra slashes, dash runs (NOT collapsed), underscore runs
    (collapsed), dot runs (NOT collapsed), leading `-`/`_`, trailing
    `.`/`_`, and an all-forbidden suffix that collapses to the bare
    `doi_10.<reg>` prefix.
  - 10 × Group C — length > 192 truncation + 8-hex SHA-256 suffix
    branch: 181-char, 200-char, 250-char, and 500-char `aaaa…` cases,
    a mixed `abab…` repeat, a `xyz-` repeat, a forbidden-char repeat
    (`foo bar foo bar…`), an `A1B2C3.` repeat, a `pqr-stu.` repeat,
    and a `mixed.case-data_` repeat. Each pins the exact byte 192/
    `_`/8-hex-suffix output produced by `Ref::safekey`.
  - 20 × Group D — arXiv basic + version + old-style category/serial:
    modern `YYMM.NNNNN`, `vN` and `vNN` version suffixes, old-style
    `hep-th/9711200`, `math.AG/0301001`, `cond-mat/9501001v3`,
    `gr-qc`, `hep-ph`, `astro-ph`, `math.DG`, `cs.LG`, and 5-digit
    serial corner cases.
  - 10 × Group E — non-ASCII inputs covering CJK (Chinese, Japanese
    kanji + katakana), Greek, Cyrillic, Arabic, Hebrew, mixed
    ASCII + non-ASCII, and emoji. Each uses a distinct ASCII prefix
    so the resulting safekeys do not collide (per the existing
    collision-caveat note in the fixture).
  - 10 × Group F — synthetic stress: all-underscore suffix, single-
    char suffix, the exact 192-byte boundary (no hash), 191-byte
    under-boundary, all-dots, all-dashes, alternating dot/dash, all-
    forbidden punctuation, the one-of-each-allowed-special `a-b.c_d`,
    and a surrounding-whitespace case.

  The two intentionally-colliding vectors (`foo bar` and `foo  bar`)
  are preserved and called out in the fixture header so the
  `_`-run-collapse step stays pinned.

- **(D.2)** Tightened `safekey_matches_reference_vectors` in
  `crates/doiget-core/src/lib.rs::tests` from `assert!(len >= 13)` to
  `assert_eq!(len, 100)`, so the fixture cannot silently grow or shrink
  without a coordinated ADR-0007 / SAFEKEY.md bump. The iteration body
  already covers every entry — no other test changes were needed.

- **(D.3)** Upgraded `.github/workflows/safekey-vectors.yml` from a
  fixture-schema-only validator to a full parity gate: a new
  `cargo test -p doiget-core --lib --no-default-features --features
  oa-only safekey_` step runs the NORMATIVE 100-vector test and the
  pre-existing `safekey_truncates_long_inputs_with_sha256_suffix`
  long-input test on every PR/push that touches `safekey/**`,
  `lib.rs`, or the workflow file. Added a hard `100`-count check in
  the `jq` schema step. The cross-tool Julia parity check
  (BiblioFetch.jl ↔ doiget) remains DEFERRED to Phase 2 per
  `docs/PHASES.md` §2 ("Pre-flight items"); the workflow header
  comment now states this explicitly.

- **(D.4)** Flipped the `tests/fixtures/safekey/vectors.json` entry in
  `docs/PHASES.md` §"Test fixtures" from `- [ ] … (13/100; full set
  Phase 0 final)` to `- [x] … 100 reference test vectors.`

No new runtime dependencies. No public API changes. Verification:
`cargo fmt --check`, `cargo build --workspace`, `cargo test
--workspace`, `cargo clippy --workspace --all-targets -- -D warnings`,
`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
--no-default-features --features oa-only`, `cargo deny check` all
green locally.

### Slice 2 — MCP doiget_fetch_paper + doiget_batch_fetch

- **(C.1)** Extracted the single-fetch and batch-fetch orchestrators
  out of `doiget-cli::commands::{fetch,batch}` into
  `doiget_core::orchestrator::{fetch_paper, batch_fetch}` siblings to
  Slice 1's `metadata_only`. The CLI's `run_with_options` now
  delegates; behaviour is preserved (the existing CLI fetch/batch
  e2e suites stay green).
- **(C.2)** New MCP tools:
  - `doiget_fetch_paper(ref, dry_run?)` — resolves and downloads one
    PDF. Honors `dry_run: true` per ADR-0022 (returns a `FetchPlan`
    envelope without touching network or store). Failure envelope
    carries `denial_context` per ADR-0023.
  - `doiget_batch_fetch(refs[], dry_run?)` — bulk variant capped at
    `MAX_BATCH_REFS = 100`. Returns one result entry per ref;
    per-ref errors do NOT fail the whole call (matches CLI batch
    semantics). `dry_run` returns `{ok:true, dry_run:true,
    plans:[...]}`.
- **(C.3)** New `pub const doiget_core::MAX_BATCH_REFS: usize = 100;`
  and `FetchError::TooManyRefs { got, max }` variant (additive on
  `#[non_exhaustive]` enum; collapses to `ErrorCode::InvalidRef` at
  the public boundary — `TooManyRefs` is a request-shape failure,
  not a denial, so `denial_context` stays `None`).
- **(C.4)** 25 new MCP integration tests in
  `crates/doiget-mcp/tests/fetch_paper_e2e.rs` plus expanded
  coverage in `initialize_handshake.rs`: `tools/list` advertises
  both new tools; INVALID_REF / TOO_MANY_REFS / dry_run /
  happy-path / partial-failure all exercised.

After Slice 2 the MCP `Server` exposes 5 of the 9 Phase 3 baseline
tools (`doiget_health`, `doiget_capability_profile`,
`doiget_metadata_only`, `doiget_fetch_paper`, `doiget_batch_fetch`).
Remaining: `doiget_resolve_paper`, `doiget_info`, `doiget_search_local`,
`doiget_list_recent`, `doiget_paper_pdf_path`.

### Slice 1 — metadata_only orchestrator + arXiv Atom feed

- **(A)** `doiget_metadata_only` non-dry-run path wired through the new
  `doiget_core::orchestrator::metadata_only` function. Replaces the
  Phase 1 `NOT_IMPLEMENTED` stub. The MCP envelope follows
  [`docs/MCP_TOOLS.md`](docs/MCP_TOOLS.md) §11 NORMATIVE shape
  (`{ok:true, ref, source, license, oa_url, metadata, schema_version}`).
  Failure envelopes carry a structured `denial_context` channel for
  denial-class errors per
  [ADR-0023](docs/DECISIONS/0023-denial-context-structured.md);
  transport-level (`NETWORK_ERROR`) failures omit it. DOI dispatch is
  Crossref-first with Unpaywall as a fallback; the Crossref OA URL
  (`message.link[].URL`) is surfaced in `oa_url` but never followed
  (the spec contract that distinguishes this tool from
  `doiget_fetch_paper`). The orchestrator honors the same
  `DOIGET_*_BASE` test-override surface the CLI already accepts so a
  single wiremock fixture drives both crates. Existing `dry_run: true`
  preview behavior (ADR-0022) is unchanged.
- **(B)** `doiget_core::sources::arxiv::ArxivSource` now produces
  `FetchResult::metadata_json` populated from the arXiv Atom feed
  (`https://export.arxiv.org/api/query?id_list=<id>`). XML parsing
  uses [`quick-xml`](https://crates.io/crates/quick-xml) as a
  streaming event walker — no DOM allocation, no `serde-xml-rs`
  (deprecated). The Atom call is best-effort during a full fetch: a
  failure logs `tracing::warn!` and falls back to a PDF-only result
  (`metadata_json = None`) so existing end-to-end tests are unchanged.
  A new public helper `ArxivSource::fetch_metadata_only` is the entry
  point for the orchestrator's arXiv branch; it MUST NOT touch the
  PDF endpoint and emits its provenance row under
  `Capability::Metadata` to distinguish metadata-only from full
  fetches without breaking
  [`docs/PROVENANCE_LOG.md`](docs/PROVENANCE_LOG.md) §3.
- Test surface added: 3 `parse_atom_feed` unit tests, 3 new arXiv
  `Source::fetch` / `fetch_metadata_only` wiremock-driven unit tests,
  6 `orchestrator` helper unit tests, a new
  `crates/doiget-core/tests/arxiv_metadata_e2e.rs` integration suite,
  and 3 new `doiget_metadata_only` MCP integration tests (arXiv happy
  path, DOI Crossref happy path, simulated network failure). The
  pre-existing `doiget_metadata_only_default_dry_run_false_returns_not_implemented_stub`
  test was deleted (the stub is gone). All `cargo fmt --check`,
  `cargo build --workspace`, `cargo test --workspace`,
  `cargo clippy --workspace --all-targets -- -D warnings`, and
  `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
  --no-default-features --features oa-only` are green locally.

### Added

#### Workspace skeleton
- Cargo workspace with three published members: `doiget-core`, `doiget-cli`,
  `doiget-mcp`. Optional Phase 7 `doiget-obsidian` crate is declared as
  `exclude`d (off by default per [ADR-0008](docs/DECISIONS/0008-crate-decomposition.md)
  and [docs/SCOPE.md](docs/SCOPE.md)).
- `Cargo.lock` baselined at the workspace root so `cargo audit` and reproducible
  builds run against a pinned dependency graph.
- `rust-toolchain.toml` pinning the `stable` channel; declared MSRV `1.86`
  (workspace `rust-version`).
- `clippy.toml` and `deny.toml` shared at the workspace root; `deny.toml` bans
  `openssl` / `native-tls` so the rustls-only TLS posture is enforced by CI.

#### Normative specs ([docs/](docs/))
- [LEGAL.md](docs/LEGAL.md), [SCOPE.md](docs/SCOPE.md),
  [SECURITY.md](docs/SECURITY.md), [STORE.md](docs/STORE.md),
  [SAFEKEY.md](docs/SAFEKEY.md), [CAPABILITY.md](docs/CAPABILITY.md),
  [PROVENANCE_LOG.md](docs/PROVENANCE_LOG.md), [ERRORS.md](docs/ERRORS.md),
  [CONFIG.md](docs/CONFIG.md), [CACHE.md](docs/CACHE.md),
  [PUBLIC_API.md](docs/PUBLIC_API.md), [MCP_TOOLS.md](docs/MCP_TOOLS.md),
  [SOURCES.md](docs/SOURCES.md).
- Supporting docs: [ARCHITECTURE.md](docs/ARCHITECTURE.md),
  [PHASES.md](docs/PHASES.md), [MIGRATION.md](docs/MIGRATION.md),
  plus [docs/INTEGRATION/](docs/INTEGRATION/).

#### Architecture Decision Records ([docs/DECISIONS/](docs/DECISIONS/))
- ADR-0001 stdio-only transport
- ADR-0002 TDM feature-gated, never in published binaries
- ADR-0003 PDF content out of scope
- ADR-0004 BiblioFetch coexistence
- ADR-0005 capability profile as a type-gate
- ADR-0006 provenance log fail-closed
- ADR-0007 safekey algorithm
- ADR-0008 crate decomposition
- ADR-0009 MVP = Tier 1 only
- ADR-0010 citation-graph hard cap
- ADR-0011 phase plan v1
- ADR-0012 MCP tool naming
- ADR-0013 CI baseline
- ADR-0014 docs class system
- ADR-0015 no telemetry
- ADR-0016 foundation crates
- ADR-0017 output mode resolution
- ADR-0018 Obsidian one-direction
- ADR-0019 eight safeguards
- [INDEX.md](docs/DECISIONS/INDEX.md)

#### CI workflows ([.github/workflows/](.github/workflows/))
- `ci.yml` — fmt, clippy (deny warnings; `expect` / `unwrap` allowed in tests),
  build, test against the declared MSRV.
- `audit.yml` — `cargo audit` against pinned `Cargo.lock`; `CDLA-Permissive-2.0`
  whitelisted.
- `posture-lint.yml` — repo-posture invariants (scoped to source paths).
- `codeql.yml` — CodeQL static analysis (Phase 0 baseline).
- `msrv-drift.yml` — weekly MSRV-vs-stable drift check; Phase 6 release-plz
  slot reserved.
- `cross-tool-compat.yml` — Phase 2 placeholder (BiblioFetch.jl ↔ doiget
  cross-tool round-trip).
- `mcp-smoke.yml` — Phase 3 placeholder (MCP stdio smoke test).
- `safekey-vectors.yml` — schema validation for
  `tests/fixtures/safekey/vectors.json`.

#### Repo hygiene
- `.github/dependabot.yml` — weekly cargo + github-actions updates, no
  auto-merge.
- `.github/FUNDING.yml`.
- `.github/CODEOWNERS` — auto-review assignment for NORMATIVE files.
- `.github/SECURITY.md` — disclosure pointer (Phase 0).
- `.github/PULL_REQUEST_TEMPLATE.md`.
- `.github/ISSUE_TEMPLATE/` — `bug_report.yml`, `feature_request.yml`,
  `question.yml`, `config.yml`.
- `.gitattributes` — LF normalization.
- `.editorconfig`.
- Root ignore for `*.tmp.*` (Dropbox / editor autosave artifacts).

#### Test fixtures scaffold
- `tests/fixtures/golden/` layout documented for Phase 1 (see
  `tests/fixtures/golden/README.md`).
- `tests/fixtures/safekey/vectors.json` — 13/100 reference vectors for the
  safekey algorithm; the remaining 87 are a Phase 0 deliverable generated in
  coordination with BiblioFetch.jl per [docs/SAFEKEY.md](docs/SAFEKEY.md).

#### OA PDF fetch from DOI (Phase 1)
- `doiget fetch <DOI>` now resolves the OA URL from Unpaywall's
  `best_oa_location.url_for_pdf` (preferred) or `best_oa_location.url`, and
  fetches the PDF via the synthetic `oa-publisher` source key whose redirect
  allowlist is documented in
  [docs/REDIRECT_ALLOWLIST.md](docs/REDIRECT_ALLOWLIST.md) §3.4. Closes the
  Phase 1 success criterion ([docs/PHASES.md](docs/PHASES.md) §4) for the
  Crossref + Unpaywall path. The OA-publisher allowlist is informed-best-
  effort; OA URLs whose host is outside the list, or whose body fails the
  PDF magic-byte check, log a `Fetch err / source=oa-publisher /
  error_code=NETWORK_ERROR` row and fall back to metadata-only success
  (partial-success semantics — the metadata is still useful).

#### Safekey derivation (Phase 1)
- `doiget-core`: `impl Ref { pub fn safekey(&self) -> Safekey }` implementing
  the NORMATIVE algorithm from [docs/SAFEKEY.md](docs/SAFEKEY.md) §3 — `doi_` /
  `arxiv_` prefix, replace any character outside `[A-Za-z0-9._-]` with `_`,
  collapse `_` runs, trim edges, and (for refs longer than 192 chars) append a
  SHA-256(raw) 8-hex tag after a 192-byte ASCII-safe prefix. Binding spec
  shared with BiblioFetch.jl per
  [ADR-0007](docs/DECISIONS/0007-safekey-algorithm.md) (#39).
- `safekey_matches_reference_vectors` test loads
  `tests/fixtures/safekey/vectors.json` via `include_str!` and asserts
  bit-identical output across all 13 reference vectors (#39).

### Changed
- Bumped `reqwest` from `0.12` to `0.13` (#30). The umbrella `rustls-tls`
  feature was removed upstream and replaced with composable pieces; switched
  to `rustls + webpki-roots` (rustls backend + bundled Mozilla WebPKI roots),
  preserving the rustls-only TLS posture. `openssl` / `native-tls` remain
  banned by `deny.toml`.
- Dependabot dependency refreshes: `thiserror` 1 → 2, `toml` 0.8 → 1.1,
  `sha2` 0.10 → 0.11, `toml_edit` 0.22 → 0.25, `actions/checkout` 4.1.1 →
  6.0.2.
- CI: bumped MSRV to `1.85` then aligned with declared MSRV `1.86`; refreshed
  action SHAs; scoped `posture-lint` to source paths; allow `expect` / `unwrap`
  in tests; whitelisted `CDLA-Permissive-2.0`.
- `doiget-core` safekey tests hardened: `safekey_matches_reference_vectors`
  now asserts `>= 13` vectors (not `== 13`) so the test survives the clean
  expansion to the NORMATIVE 100-entry set per
  [docs/SAFEKEY.md](docs/SAFEKEY.md) §5 without re-touching the test;
  added `safekey_truncates_long_inputs_with_sha256_suffix` exercising the
  `> 192` branch (synthetic 220-char DOI suffix; asserts 201-char shape, `_`
  separator at byte 192, lowercase hex suffix, determinism, and exact
  SHA-256 hash content per [docs/SAFEKEY.md](docs/SAFEKEY.md) §3 step 5).
  No new dependencies (#48).
- Bumped `reqwest` `0.13.1` → `0.13.3` and `rustls-platform-verifier` `0.6.2`
  → `0.7.0`; the standalone `webpki-roots` reqwest feature flag was dropped
  (merged into `rustls` upstream in 0.13.2+, cert-bundle behaviour preserved).
  The rustls-platform-verifier bump transitively advances `jni` `0.21.1` →
  `0.22.4` (Android-only target dep), which in turn moves to `thiserror ^2`
  and removes `thiserror 1.0.69` from the workspace lockfile entirely
  (`thiserror 2.0.18` only). Reduces future RUSTSEC exposure surface by
  proactively eliminating the dual-version `thiserror 1.x` transitive before
  any advisory lands (#49).
- `Doi::parse` / `ArxivId::parse` / `Ref::parse` return
  `Result<Self, RefParseError>` (renamed from the documented `ErrorCode`
  placeholder; see PR #55,
  [`docs/PUBLIC_API.md`](docs/PUBLIC_API.md) §4). `RefParseError` is
  `#[non_exhaustive]` and funnels to `ErrorCode::InvalidRef` at the public
  MCP / CLI boundary via `impl From<RefParseError> for ErrorCode`, so the
  `INVALID_REF` surface seen by external callers is unchanged.
- `CapabilityProfile::from_env` resolves TDM env vars per
  [`docs/CAPABILITY.md`](docs/CAPABILITY.md) §2 (Phase 1; supersedes the
  Phase 0 always-tier-1 stub).

### Fixed
- `audit.yml`: removed the temporary in-CI `cargo generate-lockfile` step now
  that `Cargo.lock` is checked in (commit `cf94535`).
- Removed an accidentally-committed editor temp file and added `*.tmp.*` to
  `.gitignore` to prevent recurrence.

#### Discussion #12 — external review incorporation (musaabhasan)

This PR lands the spec + Phase-1 implementation slice for the five
musaabhasan items raised on
[Discussion #12](https://github.com/sotashimozono/doiget/discussions/12).
Spec changes are NORMATIVE; implementation is staged so the dry-run preview
and structured denial channel ship now and the `CanonicalRef` audit identity
is reserved for Phase 2 (per ADR-0021 §3).

##### Added
- [ADR-0021](docs/DECISIONS/0021-canonical-tuple-identity.md) (**spec-only**)
  reserves `CanonicalRef = (source_type, source_id, resolver_profile, version)`
  as the Phase-2 audit identity; Phase 1 keeps `safekey` keyed on `Ref` so
  the BiblioFetch.jl round-trip contract from ADR-0007 stays unchanged.
- [ADR-0022](docs/DECISIONS/0022-dry-run-mode.md) and
  [ADR-0023](docs/DECISIONS/0023-denial-context-structured.md)
  (**accepted + implemented this PR**) — `--dry-run` mode and structured
  `denial_context` on the public error envelope.
- `doiget-core::DenialReason` (closed enum, 8 variants, snake_case wire)
  and `doiget-core::DenialContext` (`#[serde(deny_unknown_fields)]`) per
  [PUBLIC_API.md §8](docs/PUBLIC_API.md). `From<&HttpError> for
  Option<DenialContext>` (in `crate::http`) and `From<&FetchError> for
  Option<DenialContext>` (in `crate::source`) implement the ADR-0023 §4
  mapping table — `RedirectDenied` / `OversizedBody` / `NotAPdf` /
  `InsecureRedirect` produce a populated context, `Network` /
  `HttpStatus` / `UnknownSource` map to `None`.
- `HttpError::RedirectDenied { source_key, host, expected_hosts }` carries
  an allowlist snapshot so the structured channel can populate
  `denial_context.expected` without re-looking-up the source allowlist.
- `doiget-core::dry_run::{FetchPlan, PdfSourcePlan, RateLimitBudget,
  build_fetch_plan, build_dry_run_envelope}` per ADR-0022 §1 (NORMATIVE
  wire shape). Lives in `doiget-core` so both `doiget-cli` (the
  `--dry-run` flag) and `doiget-mcp` (the `dry_run: true` tool variants)
  emit byte-identical envelopes.
- `doiget fetch <ref> --dry-run` and `doiget batch <path> --dry-run` CLI
  flags. The dry-run path emits a `FetchPlan` JSON envelope on stdout and
  returns `Ok(())` without opening the provenance log, building the HTTP
  client, or writing to the store — verified by
  `tests/fetch_dry_run_e2e.rs` (no wiremock; any accidental network hit
  would fail). The CLI subcommand variants `Command::Fetch { ref_,
  dry_run }` and `Command::Batch { path, dry_run }` thread the flag
  through new `pub async fn run_with_options` entry points; the
  historical `pub async fn run(input)` signatures remain as `Default`-arg
  delegators so existing in-process integration tests compile unchanged.
- `doiget_metadata_only` MCP tool ([`docs/MCP_TOOLS.md`](docs/MCP_TOOLS.md)
  §11). Phase 1 wires the **dry-run** path only (returns the same
  `FetchPlan` envelope as the CLI); the non-dry-run path returns
  `{ok:false, error:{code:"INTERNAL_ERROR", message:"metadata_only is not
  yet wired in Phase 1; only dry_run is supported"}}` with a
  `// TODO(phase-1.x):` for the metadata-only orchestrator that will land
  in a follow-up PR.
- New normative spec sections: [ERRORS.md](docs/ERRORS.md) §3.1 + §5.1
  (denial_context wire surface), [MCP_TOOLS.md](docs/MCP_TOOLS.md) §5 +
  §10 + §11 (denial_context envelope, dry-run preview,
  `doiget_metadata_only`), [PUBLIC_API.md](docs/PUBLIC_API.md) §8
  (DenialReason / DenialContext) + §9 (forward-looking CanonicalRef
  note), [SAFEKEY.md](docs/SAFEKEY.md) §3.1 (filename-derivation inputs
  MUST NOT include `Content-Disposition` / redirect URL path /
  server-suggested filename — clarifies existing impl posture; no
  algorithm change).

##### Tests added
- `denial_*` round-trip + `deny_unknown_fields` tests in
  `crates/doiget-core/src/lib.rs::tests` (5 tests).
- `From<&HttpError> for Option<DenialContext>` per-variant tests in
  `crates/doiget-core/src/http.rs::tests` (5 tests).
- `From<&FetchError> for Option<DenialContext>` per-variant tests in
  `crates/doiget-core/src/source.rs::tests` (3 tests).
- Pure-function `FetchPlan` shape tests in
  `crates/doiget-core/src/dry_run.rs::tests` (6 tests).
- `crates/doiget-cli/tests/fetch_dry_run_e2e.rs` end-to-end
  side-effect-free integration test (4 tests: DOI dry-run no writes,
  arXiv dry-run no writes, DOI envelope shape pin, arXiv envelope shape
  pin).

##### Changed
- `camino` workspace dep gains the `serde1` feature in
  `crates/doiget-core/Cargo.toml` so `Utf8PathBuf` fields on `FetchPlan`
  serialize. (`doiget-cli` already enabled the same feature.)

##### Post-incorporation review refinements (items 2/3/4/5)

Four refinements landed on top of the C1/C2/I1–I7 review-fix commit to
harden the wire contracts the previous commits introduced:

- **(2)** ADR-0021 §1 (canonical-digest): made the `version_or_empty`
  byte-sequence semantics fully unambiguous — `version = None` MUST
  serialize as the empty byte sequence (zero bytes), NOT a `"null"` /
  `"none"` / `"-"` sentinel. Docs-only; Phase 2 implementations
  (`CanonicalRef`) can no longer disagree about the missing-version
  digest.
- **(3)** `DenialContext.expected: Vec<String>` → `Option<Vec<String>>`.
  `None` = "producer did not populate this field for this reason";
  `Some(vec![])` = "explicit empty allowlist". The previous shape
  collapsed both states, leaving an LLM agent unable to disambiguate
  "field not applicable" from "field applies but allowlist happens to
  be empty". Updated in `doiget-core/src/lib.rs` (struct + 4 tests),
  `doiget-core/src/http.rs` (4 `From` arms + 4 tests),
  `doiget-core/src/source.rs` (1 `From` arm + 1 test),
  `doiget-core/tests/redirect_denied_denial_context_e2e.rs` (2 tests),
  plus ADR-0023 §3 + §4, ERRORS.md §3.1, MCP_TOOLS.md §5, PUBLIC_API.md
  §8. New
  `denial_context_expected_some_empty_vec_preserves_explicit_empty_allowlist`
  test pins the disambiguation on the wire.
- **(4)** Added `FetchPlan.candidate_hosts_are_upper_bound: bool` (always
  `true` in Phase 1). Machine-encodes ADR-0022 §4 ("Honesty about
  candidate uncertainty") directly into the dry-run envelope, so an
  agent can detect the upper-bound semantics of `candidate_hosts`
  without consulting the spec. Updated `doiget-core/src/dry_run.rs`
  (struct + producer + new test), ADR-0022 §1 + prose, MCP_TOOLS.md §10.
- **(5)** Added `ErrorCode::NotImplemented` (wire form `"NOT_IMPLEMENTED"`).
  Distinct from `INTERNAL_ERROR` (a bug) and `CAPABILITY_DENIED` (a
  runtime config gate). `doiget_metadata_only`'s non-dry-run stub
  changed from `INTERNAL_ERROR` to `NOT_IMPLEMENTED` so agents react
  with "wait for next minor release" rather than "report a bug". The
  `metadata_only_error_envelope` helper now takes a typed `ErrorCode`
  rather than `&str` (the I6 lesson from review-pr A5: free-form
  string codes can drift from the SCREAMING_SNAKE_CASE rendering
  without the compiler noticing). Test
  `doiget_metadata_only_default_dry_run_false_returns_internal_error_stub`
  → `..._returns_not_implemented_stub`. Updated `doiget-core/src/lib.rs`
  (enum), `doiget-mcp/src/lib.rs` (stub + helper),
  `doiget-mcp/tests/initialize_handshake.rs` (renamed test),
  ERRORS.md §1 + §2 (new variant + semantics row).

[Unreleased]: https://github.com/sotashimozono/doiget/compare/main...HEAD
