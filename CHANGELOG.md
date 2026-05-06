# Changelog

All notable changes to doiget will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

`doiget-core` is the only crate with strict semver guarantees during the 0.x line; CLI
flag changes and `doiget-mcp` tool spec changes will be called out explicitly here.

## [Unreleased]

Phase 0 (design + scaffolding). No version tag is published in this phase; the
workspace stays at `0.0.0` until Phase 6. See [docs/PHASES.md](docs/PHASES.md)
for the full Phase 0 deliverable checklist.

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

### Fixed
- `audit.yml`: removed the temporary in-CI `cargo generate-lockfile` step now
  that `Cargo.lock` is checked in (commit `cf94535`).
- Removed an accidentally-committed editor temp file and added `*.tmp.*` to
  `.gitignore` to prevent recurrence.

[Unreleased]: https://github.com/sotashimozono/doiget/compare/main...HEAD
