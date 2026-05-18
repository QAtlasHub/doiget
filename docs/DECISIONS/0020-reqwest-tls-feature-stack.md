# 0020 - reqwest TLS feature stack (rustls-only, webpki bundled)

- **Date:** 2026-05-06 (Amendment 1: 2026-05-18)
- **Status:** Accepted — implemented; `reqwest` on the rustls + platform-verifier stack (PR #30, refined PR #49); **Amendment 1** swaps the aws-lc-rs crypto provider for `ring` (portability); `openssl`/`native-tls` banned by `deny.toml`
- **Supersedes:** -
- **Source:** PR #30, PR #49; Amendment 1 — portability fix (musl-static + cmake-free `cargo install`)

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

## Amendment 1 (2026-05-18) — drop aws-lc-rs for `ring` (portability)

**Context.** reqwest 0.13's `rustls` feature transitively pulls the
**aws-lc-rs** crypto provider, whose `aws-lc-sys` build script requires a
heavy C toolchain (cmake + nasm/go/clang). Dogfooding `doiget` on a
university research box (Ubuntu 20.04, only `gcc 9.4` + `make` + `python3`,
no `sudo`) surfaced two failures that both trace to this provider:

- **F1 — glibc.** The published `ubuntu-latest` release binary is
  dynamically linked against the runner's glibc 2.38; it fails with
  `GLIBC_2.38 not found` on older but entirely reasonable targets
  (Ubuntu 20.04 / RHEL 8). The fix is a static `x86_64-unknown-linux-musl`
  build — but musl-static cross-builds fight aws-lc-sys's cmake/assembly.
- **F2 — `cargo install`.** `cargo install doiget-cli` (and `--locked`)
  fails at `aws-lc-sys` `cc_builder.rs` because cmake is absent. A user
  who cannot `sudo apt-get install cmake` cannot install doiget at all.

doiget does **not** need FIPS certification or the post-quantum key
exchange (the only capabilities that justify aws-lc-rs over `ring`).

**Decision.** Switch the crypto provider from aws-lc-rs to `ring`, which
builds with only `cc` + `perl` (no cmake), cross-compiles cleanly to
musl, and is the long-standing portable default in the rustls ecosystem:

- `reqwest` feature `rustls` → `rustls-no-provider` (rustls backend +
  `rustls-platform-verifier` roots, **no** bundled crypto provider).
- Add an explicit `rustls = { default-features = false, features =
  ["ring", "std", "tls12", "logging"] }` workspace pin (drops rustls's
  own `aws_lc_rs` default and `prefer-post-quantum`, which is aws-lc-rs
  only).
- `doiget-core`'s `http` module installs `ring` as the **process-default**
  `CryptoProvider` exactly once (`ensure_crypto_provider()`, `std::sync::Once`)
  before any `reqwest::Client` is built. This is mandatory: under
  `rustls-no-provider`, `reqwest::ClientBuilder::build` panics
  (`"No provider set"`) if no default provider is installed first.
- The release `sign` job builds the Linux artefact for
  `x86_64-unknown-linux-musl` (static); the artefact name
  `doiget-linux-x86_64` is unchanged so signing/SBOM/docs are stable.

**TLS posture is unchanged:** still rustls-only (no openssl/native-tls,
`deny.toml` allowlist still satisfied — verified by `cargo tree -i
aws-lc-sys` returning *no packages*), still using
`rustls-platform-verifier` for roots. Only the symmetric/asymmetric
primitive implementation changes (aws-lc-rs → ring), both audited and
Rustls-supported. The Decision section's version-bump audit rule still
applies; additionally, **any future reqwest/rustls bump MUST re-run
`cargo tree -i aws-lc-sys` and confirm it stays empty**, and MUST keep
the explicit `rustls` pin version-compatible with the `rustls` reqwest
resolves.

To revise this decision, write a new ADR with Status: Accepted and Supersedes: 0020,
and update this file's Status to Superseded by NNNN per CONTRIBUTING.md.
