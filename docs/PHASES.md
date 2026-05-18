# Phase plan

> **Status: INFORMATIVE.** Snapshot of the implementation plan as of 2026-05-05.
> Effort estimates are best-guess and will drift; the Phase **content** is the binding
> part. Substantive changes to Phase content require an ADR.

## 1. Phase headline

| Phase | Scope | Effort |
|---|---|---|
| **0** | Repo bootstrap, normative specs, ADRs, Cargo workspace skeleton, CI | 5–7 days |
| **1** | Core resolver + Tier 1 sources + `fetch` / `batch` CLI | 1.5 weeks |
| **2** | Store + `info` / `search` / `bib` / `csl` | 1 week |
| **3** | MCP server + 9 tools + strict stdio | 1.5 weeks |
| **MVP shipping point** | | **5 weeks** |
| 3.5 | Early marketplace draft + landing page (overlap with Phase 4) | concurrent |
| 4 | Tier 2 sources + citation graph | 1.5 weeks |
| 5a | TDM Springer Nature OA — author opt-in to start | 1 week + 1–2 wk cooldown |
| 5b | TDM APS Harvest — author opt-in to start | 1 week + 1–2 wk cooldown |
| 5c | TDM Elsevier ScienceDirect — author re-decision required | 1 week |
| 6 | Release: OIDC + sigstore + SBOM + landing page polish + signed-tag pipeline (ADR-0025) | 1 week |
| 7 | Optional features (`vault`, `obsidian`) | per feature |

> **Phase 6 release note (ADR-0025, live).** Phase 6 has landed. Releases are
> **tag-driven** ([ADR-0025](DECISIONS/0025-tag-driven-release.md)): the
> maintainer pushes one signed workspace tag — `vX.Y.Z` (stable, from `main`)
> or `vX.Y.Z-beta.N` (beta, from `next`). A mandatory version gate runs first
> and, on pass, publishes all three crates to crates.io via OIDC, sigstore-signs
> the binaries, emits an SBOM, and opens the GitHub Release. `release-plz` (the
> original Slice 21/22 release-PR mechanism) was **retired**: there is no
> perpetual release PR and no per-crate `doiget-<crate>-vX.Y.Z` tags. `0.1.0`–
> `0.1.3` were cut by the old release-plz flow; **`v0.2.0`** (the current
> release: all three crates on crates.io, signed binaries + SBOM on the GitHub
> Release) was cut by the ADR-0025 pipeline. See [`CHANGELOG.md`](../CHANGELOG.md)
> for released versions and ADR-0025 for the design + release runbook.

**Phase 5 may be skipped or deferred indefinitely**; the decision is data-driven from
Phase 0–4 production usage and any publisher-side correspondence. *(Not skipped: all
three TDM sub-phases shipped — see status table below.)*

### Per-phase completion status

Status is derived from the [`CHANGELOG.md`](../CHANGELOG.md) section headings
and the slice/PR entries within them. `0.1.1`–`0.1.3` are backed by the
release-plz-era per-crate `doiget-{core,cli,mcp}-v0.1.x` git tags (dated
2026-05-17); `0.2.0` onward is backed by a single signed workspace `vX.Y.Z`
tag (ADR-0025 retired the per-crate scheme). No dates are invented: the `0.0.0`
cut date (2026-05-15) is the `## [0.0.0]` CHANGELOG heading only — there is no
`v0.0.0` tag. The TDM work (Slices 17–19) is recorded under the `## [0.0.0]`
CHANGELOG section, i.e. the 2026-05-15 dev cut, not the later tags.

| Phase | Status | Evidence (CHANGELOG slice / version) |
|---|---|---|
| **0** | Complete (2026-05-15) | Phase-0 skeleton + normative specs + ADRs + 9 CI workflows; `0.0.0` section |
| **1** | Complete (2026-05-15) | Core resolver, Tier 1 Crossref/Unpaywall/arXiv, `fetch`/`batch`/audit-log (#64–#78); Slices 1–6 |
| **2** | Complete (2026-05-17) | Store + metadata read path; `bib`/`csl` exports via Slice 15b (`0.1.1`); BiblioFetch round-trip (#121, `0.1.2`) |
| **3** | Complete (2026-05-17) | MCP server + 10-tool baseline (Slices 7–9); strict stdio (Slice 9 `stdout-purity`) |
| **4** | Complete | Tier 2 OpenAlex/S2/DOAJ + citation graph (Slices 10–16, ADR-0010) |
| **5a** | Complete | Springer Nature OA TDM (Slice 17) |
| **5b** | Complete | APS Harvest TDM (Slice 18) |
| **5c** | Complete | Elsevier ScienceDirect TDM (Slice 19) + per-source header hook (Slice 20) |
| **6** | Complete (live) | release-plz PR flow (Slices 21/22) cut `0.1.0`–`0.1.3`, then migrated to the ADR-0025 tag-driven pipeline; **`v0.2.0`** released that way |
| **7** | Not started | Optional `vault`/`obsidian` crate is `exclude`d in `Cargo.toml` (ADR-0008) |

The Phase-0 checklist in §2 below is retained as the historical deliverable record;
its boxes reflect what was committed. Per-version release dates live in
[`CHANGELOG.md`](../CHANGELOG.md).

## 2. Phase 0 deliverable checklist

Phase 0 is complete when **all** of the following are committed.

### Code

- [x] Cargo workspace with members `doiget-core`, `doiget-cli`, `doiget-mcp`.
- [x] `cargo build` succeeds (default features = `oa-only`).
- [x] `cargo build --no-default-features` succeeds (sanity: nothing under `oa-only` is actually load-bearing for compilation in Phase 0).
- [x] `doiget --help` runs (no subcommands implemented yet).

### Configuration files

- [x] [`Cargo.toml`](../Cargo.toml) workspace + features matrix.
- [x] `Cargo.lock` committed.
- [x] [`rust-toolchain.toml`](../rust-toolchain.toml) — MSRV 1.86 declared (channel: stable).
- [x] [`deny.toml`](../deny.toml) — banned crate list.
- [x] [`clippy.toml`](../clippy.toml) — workspace lints.
- [x] [`.cargo/config.toml`](../.cargo/config.toml) — build flags.

### NORMATIVE docs

- [x] [`README.md`](../README.md)
- [x] [`LICENSE`](../LICENSE)
- [x] [`CONTACT.md`](../CONTACT.md)
- [x] [`docs/LEGAL.md`](LEGAL.md)
- [x] [`docs/SCOPE.md`](SCOPE.md)
- [x] [`docs/SECURITY.md`](SECURITY.md)
- [x] [`docs/STORE.md`](STORE.md)
- [x] [`docs/SAFEKEY.md`](SAFEKEY.md)
- [x] [`docs/CAPABILITY.md`](CAPABILITY.md)
- [x] [`docs/PROVENANCE_LOG.md`](PROVENANCE_LOG.md)
- [x] [`docs/ERRORS.md`](ERRORS.md)
- [x] [`docs/CONFIG.md`](CONFIG.md)
- [x] [`docs/CACHE.md`](CACHE.md)
- [x] [`docs/PUBLIC_API.md`](PUBLIC_API.md)
- [x] [`docs/MCP_TOOLS.md`](MCP_TOOLS.md)
- [x] [`docs/SOURCES.md`](SOURCES.md)
- [x] [`docs/REDIRECT_ALLOWLIST.md`](REDIRECT_ALLOWLIST.md)
- [x] [`CHANGELOG.md`](../CHANGELOG.md) (Keep a Changelog format)

### INFORMATIVE docs

- [x] [`CONTRIBUTING.md`](../CONTRIBUTING.md)
- [x] [`docs/ARCHITECTURE.md`](ARCHITECTURE.md)
- [x] [`docs/PHASES.md`](.) (this document)
- [x] [`docs/MIGRATION.md`](MIGRATION.md) (placeholder; full content Phase 2)
- [x] [`docs/DECISIONS/INDEX.md`](DECISIONS/) + ADRs 0001–0020

### CI workflows

- [x] `.github/workflows/ci.yml` (fmt, clippy, test on a 3 OS × 3 features matrix)
- [x] `.github/workflows/audit.yml` (cargo audit + cargo deny)
- [x] `.github/workflows/posture-lint.yml` (forbidden term / crate guard)
- [x] `.github/workflows/codeql.yml` (Phase 0 onward)
- [x] `.github/workflows/safekey-vectors.yml` (vector-set parity)
- [x] `.github/workflows/cross-tool-compat.yml` (BiblioFetch.jl round-trip; placeholder until Phase 2)
- [x] `.github/workflows/msrv-drift.yml` (weekly: detect transitive-dep MSRV bumps past declared 1.86)
- [x] `.github/dependabot.yml` (auto-merge disabled)

### Repo-level settings (manual; cannot be committed)

- [ ] Branch protection on `main`: required PR review, required status checks, signed
      commits. *(unverified: repo-settings/external — not observable from the tree.)*
- [ ] 2FA mandatory on the maintainer account.
      *(unverified: repo-settings/external.)*
- [ ] Default branch protection includes Action SHA pinning policy.
      *(unverified: repo-settings/external — note: the workflow actions themselves
      are SHA-pinned in-tree, e.g. `.github/workflows/release-plz.yml`
      `actions/checkout@de0fac2…`, `rust-lang/crates-io-auth-action@bbd816…`
      (`release-plz/action` was removed with the ADR-0025 migration); the
      branch-level enforcement *policy* is still a repo-settings claim.)*

### Test fixtures

- [x] `tests/fixtures/safekey/vectors.json` — 100 reference test vectors.
- [x] `tests/fixtures/golden/` — placeholder.

### Pre-flight items (must be confirmed before Phase 0 begins)

- [x] Confirm BiblioFetch.jl's current safekey algorithm matches
      [`SAFEKEY.md`](SAFEKEY.md) §3, or arrange a coordinated bump.
      *(verified: the 100-vector NORMATIVE parity set landed in Slice 3 and the
      cross-tool store round-trip — typed `[bibliofetch]` table + unknown scalar
      preservation — landed in CHANGELOG `0.1.2` / issue #121, exercised in
      `crates/doiget-core/src/store/metadata.rs`.)*
- [x] Confirm BiblioFetch.jl's current TOML `schema_version`.
      *(verified: `schema_version = "1.0"` is the binding value in
      [`STORE.md`](STORE.md) §; the round-trip test asserts unknown/foreign
      tables survive read-modify-write, so a BiblioFetch-written schema is
      preserved.)*
- [ ] Verify maintainer's `crates.io` account is set up for trusted publishing (OIDC).
      *(partial — the OIDC release job is wired in-tree (Slice 22,
      `.github/workflows/release-plz.yml`), but the one-time crates.io Trusted
      Publisher registration is unverified: repo-settings/external.)*
- [ ] Verify GitHub Environment is configurable for the release workflow.
      *(unverified: repo-settings/external.)*

## 3. Phase 0 working principles

- **Docs and code can be written in parallel.** Multiple agents (or a person and an
  agent) can advance docs and code skeleton concurrently.
- **No source impl in Phase 0.** Phase 0 ships the workspace skeleton only; actual
  `Source::fetch` implementations begin in Phase 1.
- **No external services touched.** Phase 0 makes no real fetches.

## 4. Phase 1 readiness criteria (informational)

Phase 1 begins when Phase 0's deliverable checklist is complete and the pre-flight items
have been confirmed. Phase 1's success criterion is:

- `doiget fetch <DOI>` succeeds for at least one Open Access DOI from each of Crossref,
  Unpaywall, and arXiv.
- `doiget batch <refs.txt>` honors the rate cap and writes a hash-chained provenance
  log.
- `cargo test --workspace` is green.

## 5. Tracking

Phase progress is tracked through:

- The **per-phase completion status table** in §1 above (the authoritative
  current-state view).
- `CHANGELOG.md` — each unit of work lands as a numbered **Slice** entry tagging
  its phase (e.g. *"Slice 14 — Citation graph BFS expansion (ADR-0010, Phase 4)"*);
  released versions carry their date in the `## [X.Y.Z] - YYYY-MM-DD` heading.
  This is the real tracking mechanism — there are **no** `phase-N` GitHub issue
  labels (the original plan to use them was never adopted).
- `git log` / git tags (`doiget-{core,cli,mcp}-vX.Y.Z`) for release dates.

When a Phase completes, the §1 status table is updated with the completion date
(derived from the CHANGELOG slice / tag, never invented) and a one-line evidence
pointer; the next Phase's slices then begin landing in `CHANGELOG.md`.
