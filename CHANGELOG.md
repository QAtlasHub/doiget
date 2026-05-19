# Changelog

All notable changes to doiget will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

`doiget-core` is the only crate with strict semver guarantees during the 0.x line; CLI
flag changes and `doiget-mcp` tool spec changes will be called out explicitly here.

## [Unreleased]

### Fixed

- **[portability]** `doiget` now installs **everywhere** — `cargo install
  doiget-cli` no longer requires cmake/nasm/go, and the published Linux
  binary runs on old glibc (Ubuntu 20.04 / RHEL 8 / HPC boxes). Root
  cause: reqwest's `rustls` feature pulled the aws-lc-rs crypto provider
  (heavy C toolchain) and the release binary was dynamically linked
  against the runner's glibc. Now: `reqwest` uses `rustls-no-provider`
  with the `ring` crypto provider (cc + perl only), installed as the
  process default in `doiget-core`'s `http` module; the release Linux
  artefact is a static `x86_64-unknown-linux-musl` build. TLS posture is
  unchanged (rustls-only, platform-verifier roots; `deny.toml` allowlist
  still satisfied). See ADR-0020 Amendment 1.

## [0.2.1-beta.4] - 2026-05-19

### Added

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

## [0.2.1-beta.3] - 2026-05-19

### Fixed

- **[mcp/core]** `doiget_metadata_only` now writes the metadata TOML to
  the store (`<root>/.metadata/<safekey>.toml`) — the documented
  `docs/MCP_TOOLS.md` §11 SIDE EFFECT that was previously a `TODO`
  (orchestrator returned provenance rows but zero disk artifacts, so
  `doiget_info` after `doiget_metadata_only` returned `metadata: null`).
  Implemented as a new `metadata_only_to_store` wrapper around the
  unchanged **pure** `metadata_only`; `doiget_resolve_paper`
  (`resolve_only`) keeps delegating to the pure resolver, so its
  `docs/MCP_TOOLS.md` §1 "NEVER writes a metadata TOML" contract now
  holds *structurally* (the store-write lives in a separate entry point
  `resolve_only` does not call) and cannot regress. e2e tests assert
  metadata-only persists a readable TOML and resolve-only writes
  nothing. (#139)

## [0.2.1-beta.2] - 2026-05-19

### Fixed

- **[repo]** `LICENSE` is now the verbatim 21-line SPDX MIT body. The
  trailing `---` separator + paper-licensing `Note:` paragraph (which
  pushed GitHub's `licensee` classifier below its match threshold, so the
  repo showed `licenseInfo: Other`) is removed; the identical posture
  statement already lives in `docs/LEGAL.md` and the site posture page.
  Restores MIT classification for crates.io / SPDX / shields. (#157)

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
