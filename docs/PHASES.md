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
| 6 | Release: OIDC + sigstore + SBOM + landing page polish + auto-tag (`release-plz`) | 1 week |
| 7 | Optional features (`vault`, `obsidian`) | per feature |

> **Phase 6 auto-tag note.** Version bumps and tags are deferred to Phase 6 via
> [`release-plz`](https://release-plz.dev) — Conventional Commits drive a
> Cargo.toml version bump PR, which when merged tags and (with OIDC trusted
> publishing) ships to crates.io. Phase 0 carries no `release-plz.toml` yet;
> until Phase 6 the version stays `0.0.0` and tags are not minted.

**Phase 5 may be skipped or deferred indefinitely**; the decision is data-driven from
Phase 0–4 production usage and any publisher-side correspondence.

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
- [x] [`docs/DECISIONS/INDEX.md`](DECISIONS/) + ADRs 0001–0019

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
      commits.
- [ ] 2FA mandatory on the maintainer account.
- [ ] Default branch protection includes Action SHA pinning policy.

### Test fixtures

- [ ] `tests/fixtures/safekey/vectors.json` — 100 reference test vectors. (13/100; full set Phase 0 final)
- [x] `tests/fixtures/golden/` — placeholder.

### Pre-flight items (must be confirmed before Phase 0 begins)

- [ ] Confirm BiblioFetch.jl's current safekey algorithm matches
      [`SAFEKEY.md`](SAFEKEY.md) §3, or arrange a coordinated bump.
- [ ] Confirm BiblioFetch.jl's current TOML `schema_version`.
- [ ] Verify maintainer's `crates.io` account is set up for trusted publishing (OIDC).
- [ ] Verify GitHub Environment is configurable for the release workflow.

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

- This file's checklist.
- `CHANGELOG.md` `[Unreleased]` section.
- GitHub issues labeled `phase-0`, `phase-1`, etc.

When a Phase completes, this document gets updated with the completion date and a brief
summary, and the next Phase's checklist becomes active.
