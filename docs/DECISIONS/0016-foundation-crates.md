# 0016 - Common foundation crates + deny list

- **Date:** 2026-05-05
- **Status:** Proposed
- **Supersedes:** -
- **Source:** Discussion #13

## Context

This is a stub ADR. The full Context, Decision, and Consequences sections will be
expanded during Phase 0 from the corresponding GitHub Discussion review threads.
The summary below is sufficient for ADR INDEX.md and for cross-references in the
NORMATIVE specs to resolve.

## Decision

Workspace pins core dependencies (tokio, clap, thiserror, anyhow, tracing, secrecy, reqwest, camino, fs2, serde, toml, chrono, sha2, hex, uuid, ulid, url) at known-good versions. cargo-deny enforces a closed license allow-list, bans HTTP server frameworks, telemetry SDKs, self-update crates, and the openssl family in favor of rustls.

## Consequences

See the source Discussion for the analyzed positives and negatives. The binding
result is captured in the Decision above and is referenced from the relevant
NORMATIVE doc(s) under docs/.

To revise this decision, write a new ADR with Status: Accepted and Supersedes: 0016,
and update this file's Status to Superseded by NNNN per CONTRIBUTING.md.
