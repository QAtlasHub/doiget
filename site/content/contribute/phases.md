+++
title = "Phase plan"
description = "MVP-to-Phase-6 roadmap. The binding contract lives in docs/PHASES.md in the repo."
weight = 110
+++

The canonical phase plan is
[`docs/PHASES.md`]({{ config.extra.github_url }}/blob/main/docs/PHASES.md)
in the repository. This page is a thin orientation; the binding
content lives there.

## Phase headline

| Phase | Scope |
|---|---|
| **0** | Repo bootstrap, normative specs, ADRs, Cargo workspace, CI |
| **1** | Core resolver + Tier 1 sources + `fetch` / `batch` CLI |
| **2** | Store + `info` / `search` / `bib` / `csl` |
| **3** | MCP server + 10 tools + strict stdio |
| **3.5** | Marketplace draft + landing page (this site) |
| **4** | Tier 2 sources + citation graph |
| **5** | TDM Springer / APS / Elsevier (deferred-by-default) |
| **6** | Release: OIDC + sigstore + SBOM + auto-tag |
| **7** | Optional features (vault, obsidian) |

Until Phase 6 ships, the workspace version stays at `0.0.0` and no
crates.io publication occurs.

## Current state

See [`docs/PHASES.md`]({{ config.extra.github_url }}/blob/main/docs/PHASES.md)
for the live checklist. Substantive changes to phase scope require an
ADR per `docs/DECISIONS/`.
