# 0020 - reqwest TLS feature stack (rustls-only, webpki bundled)

- **Date:** 2026-05-06
- **Status:** Proposed
- **Supersedes:** -
- **Source:** PR #30, PR #49

## Context

The binding constraint is set by `deny.toml`, which bans the OpenSSL family
outright in favour of pure-rustls TLS (cross-references `docs/SECURITY.md` §1.9
and ADR-0016). The exact ban entries are:

```toml
# OpenSSL family — TLS is via rustls only (docs/SECURITY.md §1.9)
{ name = "openssl" },
{ name = "openssl-sys" },
{ name = "native-tls" },
```

Because `native-tls` is also banned, reqwest's default TLS path is unavailable;
the workspace must select a rustls-based feature stack with `default-features =
false` and explicitly opt into the wire-format and TLS pieces it actually uses.

Two recent PRs adjusted that feature stack as upstream reqwest evolved:

- **PR #30 (reqwest 0.12 → 0.13):** the old umbrella feature `rustls-tls` was
  split by reqwest 0.13 into composable pieces (`rustls` for the backend +
  aws-lc-rs crypto provider + platform-verifier roots, and `webpki-roots` for
  bundled Mozilla WebPKI roots). The bump renamed our feature list from
  `rustls-tls` to `rustls + webpki-roots` to reproduce the 0.12 behaviour.
- **PR #49 (reqwest 0.13.3 + rpv 0.7):** reqwest 0.13.2+ now bundles webpki
  roots automatically when `rustls` is enabled, so the explicit `webpki-roots`
  flag was dropped to avoid feature drift and silent duplication.

The current `Cargo.toml` line is therefore:

```toml
reqwest = { version = "0.13", default-features = false, features = [
    "rustls", "json", "gzip", "brotli", "stream",
] }
```

This is sufficient to satisfy the `deny.toml` allowlist (no openssl family in
the dependency graph) while keeping HTTPS, JSON deserialisation, gzip/brotli
content-decoding, and streaming response bodies available to `doiget-core`.

This is a stub ADR. The full Context, Decision, and Consequences sections will be
expanded during Phase 0 from the corresponding GitHub Discussion review threads.
The summary below is sufficient for ADR INDEX.md and for cross-references in the
NORMATIVE specs to resolve.

## Decision

The workspace pins reqwest with `default-features = false` and the feature set
`["rustls", "json", "gzip", "brotli", "stream"]`. The `rustls` feature is the
single source of TLS: it pulls in the rustls backend, the aws-lc-rs crypto
provider, the platform verifier, and (since reqwest 0.13.2) bundled Mozilla
WebPKI roots — which together map onto the `deny.toml` allowlist (no openssl,
no openssl-sys, no native-tls). Future reqwest version bumps MUST audit
reqwest's feature surface for renames, splits, or merges (as happened with the
0.12 → 0.13 split of `rustls-tls`, and with the 0.13.2 absorption of
`webpki-roots`) before the bump merges; a `cargo tree -e features` diff against
the previous lockfile is the minimum check.

## Consequences

See the source PRs (#30, #49) for the analyzed positives and negatives. The
binding result is captured in the Decision above and is referenced from the
relevant NORMATIVE doc(s) under docs/ (notably `docs/SECURITY.md` §1.9 and
ADR-0016).

To revise this decision, write a new ADR with Status: Accepted and Supersedes: 0020,
and update this file's Status to Superseded by NNNN per CONTRIBUTING.md.
