# 0058 - Software citations from GitHub, and Zenodo's concept DOI

- **Date:** 2026-09-29
- **Status:** Accepted
- **Supersedes:** -
- **Amends:** [`docs/LEGAL.md`](../LEGAL.md) §2 (the hosts a default build can contact), per [0014](0014-docs-class-system.md); [`docs/REDIRECT_ALLOWLIST.md`](../REDIRECT_ALLOWLIST.md) §3
- **Builds on:** [0003](0003-pdf-content-out-of-scope.md) (nothing here reads a PDF), [0055](0055-error-disposition.md) (every code has a disposition)
- **Source:** #614

## Context

Papers cite their software, often as a GitHub release with no DOI, for
example `https://github.com/srwhite59/HFDMRG.jl/releases/tag/v0.1.0`.
`doiget cite` accepted DOIs and arXiv ids only, so these entries were written
by hand, which is what a doiget-managed bibliography exists to avoid. When
the software *is* archived on Zenodo, it has two kinds of DOI: one per version
and one concept DOI for every version. Nothing said which one an entry
carried.

## Decision

**D1: `cite` takes a GitHub repository or release URL.** `github.com/{owner}/{repo}`,
optionally with `/releases/tag/{tag}` or `/tree/{tag}`, is recognised before
the DOI / arXiv parser and cited as `@software` (biblatex; classic BibTeX
styles read an unknown type as `@misc`). The entry carries the title,
author(s), year, `version`, `url` and, when the repository's `CITATION.cff`
names one, `doi`. The URL is not a `Ref`: software is not a paper and has no
store entry, so no store, safekey or fetch path changes.

**D2: Two new hosts, asked only for a GitHub URL the caller names.**
`api.github.com`, for the repository, the release (`/releases/tags/{tag}`, or
`/releases/latest` for a bare repository URL) and a tag's commit date when the
tag has no release; and `raw.githubusercontent.com`, for `CITATION.cff` at that
tag. They have their own allowlist (`software_allowlist`, source keys `github`
and `github-raw`), registered by the CLI only and kept out of the Tier 1 list
a fetch plan is read from, behind the rate limiter and the provenance log like
every other request. Neither is contacted by `fetch`, `batch`, search or any MCP tool.
Requests are unauthenticated: GitHub allows 60 an hour per address, and
doiget sends no GitHub token. A 429 is reported as `retry_after`. A 403 stays
`CAPABILITY_DENIED` (`needs_config`): GitHub sends it both for the spent limit
and for a repository that is not public, the status cannot tell them apart,
and calling it retryable would have a private repository asked again every
30 s under repeat suppression (ADR-0057). `cite` and `verify` name both causes.
Each request is logged with the cited URL as its `ref` and no
`canonical_digest` (docs/PROVENANCE_LOG.md).

**D3: `CITATION.cff` wins where it speaks.** Its authors and title are the
authors' own statement of how to be cited, so they replace the repository
owner and name. Only the top-level `title`, `version`, `doi`, `date-released`
and `authors` are read, by a line reader rather than a YAML crate (no YAML
crate is a dependency, and adding one means a cargo-vet audit for five
fields). Nested blocks such as `preferred-citation` are skipped, so their
`authors` are never mistaken for the software's. A file the reader cannot
follow cites as if it were absent, and stderr says the author is then the
repository owner.

**D4: A Zenodo software record is cited by its concept DOI by default.** A
DataCite record whose `relatedIdentifiers` say `IsVersionOf` a DOI is a
version; `cite` uses that concept DOI, drops the version's number and landing
page, and says so on stderr. `--zenodo-version` keeps the version DOI and
still names the concept. A DOI that is itself the concept (`HasVersion`) is
named as such. A citation of software usually means the software, and the
concept DOI always resolves to the latest version.

**D5: `verify` checks a software entry still resolves.** A bibliography entry
with no DOI or arXiv id whose `url` is a GitHub repository or release is
software (`ParseError::SoftwareUrl`). `verify` asks GitHub whether the
repository, and the release or tag it names, still exist: `valid`, `absent`
on a 404, `unreachable` otherwise. `batch`, `missing` and
`doiget_batch_from_bibliography` report such an entry as software, with the
`cite` command that renders it, rather than as missing an identifier. It is
checked even under `[verify] on_missing_id = "skip"`: it is not missing an
identifier any more -- its URL is one `verify` can check, which is what #614
asked of it.

## Consequences

- LEGAL.md §2's host table gains `api.github.com` and
  `raw.githubusercontent.com`, marked as contacted only by `cite` / `verify`
  on a GitHub URL.
- A DataCite `Software` record now renders as `@software`, not `@misc`, with
  its `version` and `url`.
- Not covered: GitLab, Codeberg and Software Heritage. Each is a further host
  and a further decision.
