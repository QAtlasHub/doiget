# Architecture Decision Records

> **Status: NORMATIVE (index).** Lists every accepted ADR. Each ADR file is itself
> NORMATIVE for its scope. Superseded ADRs are kept in place for history and marked
> `Status: Superseded by NNNN`.

ADR format follows [Michael Nygard's template](https://cognitect.com/blog/2011/11/15/documenting-architecture-decisions):
**Context · Decision · Consequences · Status**.

To revise a decision, write a new ADR that supersedes the old one.
See [`../../CONTRIBUTING.md`](../../CONTRIBUTING.md) §"ADR workflow".

## Index

Status column reconciled 2026-05-17 against `CHANGELOG.md` slices (issue #150).
"Accepted" rows note the implementing slice/PR in the ADR file's `Status:` line.

| # | Title | Status | Implemented by | Source Discussion |
|---|---|---|---|---|
| 0001 | MCP transport is stdio only | Accepted | posture-lint + Slice 9 stdout-purity | #4 |
| 0002 | TDM sources are compile-time feature-gated | Accepted | Slices 17/18/19 | #5 |
| 0003 | PDF content processing is permanently out of scope | Accepted (Amended by 0032: narrowed to PDF-*blob* processing; structured HTML/XML full-text is in scope) | standing policy (SCOPE.md #1 + posture-lint) | #9 |
| 0004 | BiblioFetch.jl coexistence — shared store contract | Accepted (Amended by 0036: default root no longer `~/papers`; shared *format* contract unchanged) | Phase 1 store + #121 (`0.1.2`) | #1 / #2 |
| 0005 | CapabilityProfile gates source invocation at the type level | Accepted | PR #64/#65 (Phase 1) | #16 / #17 |
| 0006 | Provenance log is JSON Lines + SHA-256 hash chain (fail-closed) | Accepted | PR #61 + Slice 4 | #12 / #17 |
| 0007 | safekey algorithm with 100 reference test vectors | Accepted | PR #39 + Slice 3 | #1 §Contract 4 / #17 |
| 0008 | Workspace is split into doiget-core / -cli / -mcp (+ -obsidian opt) | Accepted | Phase 0 workspace | #14 / #18 |
| 0009 | MVP source list is Tier 1 only (Crossref / Unpaywall / arXiv) | Accepted | PR #67/#68/#69 (Phase 1) | #3 |
| 0010 | Citation graph hard-cap (depth=3, total=100, per-paper=20) | Accepted | Slice 14 | #6 |
| 0011 | Phase plan v1 — MVP at 5 weeks, Phase 5 deferred-by-default | Accepted | executed through Phase 6 | #7 |
| 0012 | MCP tool naming + structured ok-false errors | Accepted | Slices 1/2/7/8/15 | #8 |
| 0013 | CI baseline (9 workflows, OIDC publish, sigstore signing) | Accepted (release portion Superseded by 0025, PR #166) | Phase 0 + Slices 9/22 + `5a108dd` | #10 |
| 0014 | Docs split into NORMATIVE / INFORMATIVE with ADR change-control | Accepted | Phase 0 (in force) | #11 |
| 0015 | No telemetry / phone-home / self-update | Accepted | standing policy (SCOPE.md #10/#11 + posture-lint) | #12 |
| 0016 | Common foundation crates + deny list | Accepted | Phase 0 (deny.toml) | #13 |
| 0017 | Output mode resolution (flag > env > implicit > TTY > quiet) | Accepted | Slice 9 stdout-purity + Slice 1 + #144 (full ladder, 0.2.1-beta.7) | #14 |
| 0018 | Obsidian export is one-direction, optional Phase 7 | Proposed (Phase 7, unshipped) | — | #15 |
| 0019 | Eight-safeguard legal posture (5 social + 3 technical) | Accepted | standing posture (Phase 1 onward) | #16 |
| 0020 | reqwest TLS feature stack (rustls-only; ring provider) | Accepted (Amendment 1 2026-05-18: aws-lc-rs → ring, portability) | PR #30 / PR #49 / Amendment 1 | PR #30 / PR #49 |
| 0021 | Canonical-tuple identity for fetched papers (spec-only) | Superseded by 0024 (impl); §1–§4 NORMATIVE remains binding | Slice 4 (via 0024) | #12 |
| 0022 | Dry-run mode for fetch operations | Accepted | Slice 2 | #12 |
| 0023 | Structured `denial_context` on error envelopes | Accepted | Slices 1/2 | #12 |
| 0024 | CanonicalRef implementation + provenance log v1 → v2 migration | Accepted | Slice 4 | Slice 4 |
| 0025 | Tag-driven release with version gate + beta/stable lanes | Accepted (Amend. 5 2026-05-19: D6 `next`-primary; Amend. 6 2026-05-19: advisory `version-check` job, D1 unchanged; Amend. 7 2026-08-27: D2-G5 accepts `## [Unreleased]` on the beta lane, stable unchanged; **Amended by 0033** 2026-06-24: per-PR version-bump gate, D6 rule 4 direct hotfix retired) | PR #166 / Amend. 5 / Amend. 6 / Amend. 7 | maintainer review 2026-05-17 |
| 0026 | DOI suffix charset extension: permit `:` (SECURITY.md §1.1) | Accepted | #194 | #194 dogfood |
| 0027 | redirect-allowlist: add physics-society / diamond-OA hosts to `oa-publisher` | Accepted | #193 | #193 dogfood |
| 0028 | User-extensible capability gate (ToS+verified-curation posture; impersonation out-of-scope) | Accepted (design; slice TBD) | TBD | #220 / #223 |
| 0029 | Fetch chain: per-Ref multi-attempt resolution with attempt-level provenance | Accepted (design; slice TBD) | TBD | #222 / dogfood 2026-05-20 |
| 0030 | Bibliography input adapters (.bib / CSL-JSON) in `doiget-core`; new MCP tool `doiget_batch_from_bibliography` | Accepted (design; slice TBD) | TBD | #222 / Zotero distribution review 2026-05-20 |
| 0031 | Discovery search is Tier-1 OA metadata (always-on); `doiget search` defaults to external discovery | Accepted (design; PR1 slice) | PR1 (`feat/paper-search`) | #281 / feasibility read 2026-06-04 |
| 0032 | Structured full-text (HTML/XML) extraction in scope; PDF-blob processing stays out of scope | Accepted (design; PR4 ships ar5iv leg) | PR4 (`feat/paper-text`) | #281 item 3 / scope decision 2026-06-06 |
| 0033 | Per-PR version-bump enforcement + strict next→main promotion (amends 0025 §D6) | Accepted (0.8.0, #352) | `chore/version-bump-gate` | maintainer review 2026-06-24 (0.7.1 vanishing) |
| 0034 | arXiv source bundle + individual figure download | Accepted (0.8.0, #352) | `feat/343-source-bundle-figures` | #343 / dogfood 2026-06-24 |
| 0035 | `fetch --link`: surface fetched artifacts into the working tree | Accepted (0.8.0, #352) | `feat/344-fetch-link` | #344 / dogfood 2026-06-24 |
| 0036 | Default store root → `./papers` (cwd); amends 0004 co-location | Accepted (0.8.0, #352) | `feat/344-default-store-cwd` | #344 problem 1 / dogfood 2026-06-24 |
| 0037 | `doaj.org` promoted to the default `oa-publisher` allowlist; `trust_oa_registries` keeps the rest | Accepted (0.8.8) | `feat/adr-source-and-allowlist-decisions` | #405 item 3 / #409 |
| 0038 | Store root stays cwd-relative; 0036 reaffirmed against new evidence | Accepted (0.8.8) | `feat/adr-source-and-allowlist-decisions` | #406 |
| 0039 | IEEE / ACM / SIAM / AMS stay off `oa-publisher`; TDM credentials are the route | Accepted (0.8.8) | `feat/adr-source-and-allowlist-decisions` | #407 |
| 0040 | Source expansion gated by the existing `metadata` feature, runtime-gated off | Accepted (0.8.8) | `feat/adr-source-and-allowlist-decisions` | #413 |
| 0041 | Tier-3 TDM sources are scoped to their publisher's DOI prefixes | Accepted (0.8.9) | `fix/442-tier3-orchestrator-wiring` | #442 |
| 0042 | `tdm-ieee` ships against an inferred contract, marked unverified | Accepted (0.8.9) | `feat/430-tdm-ieee` | #430 |
| 0043 | The machine-readable surfaces carry the trace and the remediation | Accepted (0.8.9) | `feat/459-machine-readable-diagnostics` | #459 |
| 0044 | Tier-3 TDM sources are consulted on a blocked content leg, not on a Crossref miss | Accepted | `feat/458-tdm-content-leg` | #458 |
| 0045 | Per-source rate limits, taken as the stricter of the vendor's terms and the global cap | Accepted | `fix/493-per-source-rate-limits` | #493 |
| 0046 | In SOURCES.md, a claim about a vendor is normative; the URL where the vendor says it is a pointer | Accepted | `docs/495-496-sources-accuracy` | #495, #496 |
| 0047 | LEGAL.md's claims are read off the code, and an "enforced control" must name enforcement that exists | Accepted | `docs/494-legal-network-surface` | #494 |
| 0048 | The access ceiling is written down, and widening it amends the written form | Accepted | `docs/497-access-invariant` | #497 |
| 0049 | An unparsable ref is misuse (exit 2), and one function decides it | Accepted | `fix/492-invalid-ref-exit-code` | #492 |
| 0050 | credentials.toml carries the key; the agreement stays in the environment | Accepted | `feat/509-credentials-file` | #509 |
| 0051 | Contributions carry a relicensable grant, recorded as a commit sign-off | Accepted | `chore/cla-relicensing-grant` | - |
| 0052 | Crossref `link[]` is programme-scoped, so the fetch path does not carry it | Accepted | `feat/517-publisher-candidate` | #517 |
| 0053 | A DOI resolver is addressing, not hosting | Accepted | `fix/533-resolver-hop-is-addressing` | #533 |
| 0054 | An access refusal is a type, and it collapses to `NO_OA_AVAILABLE` | Accepted | `fix/538-typed-access-refusal` | #538 |

## Conventions

- Filename: `NNNN-<short-slug>.md` (e.g. `0001-stdio-only.md`).
- All ADRs reference relevant NORMATIVE docs (`../LEGAL.md`, etc.).
- An ADR is created **before** any code change that locks the decision.
- **An ADR flips from `Proposed` to `Accepted` when its decision is merged**
  (the implementing slice/PR is noted in the ADR's `Status:` line and in the
  "Implemented by" column above). An ADR whose decision is not yet realized
  stays `Proposed`.
- Once accepted, an ADR is never edited in place. To change a decision, write a new
  ADR with `Supersedes: NNNN` and update the old ADR's `Status:` to
  `Superseded by NNNN`.

## Why ADRs

If [GitHub Discussions](https://github.com/QAtlasHub/doiget/discussions) is ever
deleted (the maintainer's stated worst-case for this repo), the ADRs preserve the
binding decisions in the source tree itself. The Discussions link is for historical
context only.
