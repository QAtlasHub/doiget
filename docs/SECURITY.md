# Security

> **Status: NORMATIVE.** This document defines binding security contracts and threat
> surfaces. Implementations and reviewers MUST address each surface before introducing
> code in the affected area. Changes require a new ADR in [`DECISIONS/`](DECISIONS/).

For vulnerability reporting, see [`../CONTACT.md`](../CONTACT.md). **Do not file a public
issue for security disclosures.**

---

## 1. Threat surfaces

### 1.1 Input — DOI / arXiv id strings

Source: CLI argument or MCP tool argument. Trust level: **untrusted**.

| Vector | Mitigation |
|---|---|
| Path traversal in DOI suffix | Strict regex (`^10\.\d{4,9}/[A-Za-z0-9._/()-]+$`); `safekey` algorithm escapes all characters outside `[A-Za-z0-9._\-_]` (see [`SAFEKEY.md`](SAFEKEY.md)). |
| Excessively long suffix | `DOI_SUFFIX_MAX_LEN = 256` chars; longer inputs are rejected with `INVALID_REF`. |
| Regex DoS | Validation regex is anchored, deterministic, no nested quantifiers. |
| Log injection (CR / LF / control chars) | Provenance log is JSON Lines; all string fields are JSON-escaped, control chars become `\uXXXX`. |

### 1.2 HTTP responses

Source: publisher / source API. Trust level: **partially trusted** (TLS-authenticated host, content-typed payload).

| Vector | Mitigation |
|---|---|
| Oversized PDF | Streaming download with body cap (`PDF_MAX_BYTES = 100_000_000`); writes to a temp file, validated then renamed. |
| Malformed JSON | `serde_json` strict mode; deserialization errors map to `STORE_ERROR` or `NETWORK_ERROR`. |
| Magic-byte mismatch | PDFs are checked for `%PDF-` header. Files failing this are deleted and the fetch errors. |
| Slowloris-style stalled response | `reqwest` per-request timeouts (`connect 10s`, `read 60s`, total `300s`). |

### 1.3 HTTP redirects

| Vector | Mitigation |
|---|---|
| Redirect to `file://`, `data:`, internal | reqwest is configured with redirect policy `RedirectPolicy::custom`: only `https://` redirects allowed. |
| Redirect to attacker host | Per-source allowlist of redirect target hosts; redirects outside the allowlist abort the fetch. See [`REDIRECT_ALLOWLIST.md`](REDIRECT_ALLOWLIST.md). |
| Redirect loop | `redirect_limit = 10`. |
| Open-redirect SSRF chain | Tool inputs never accept URLs (only DOI / arXiv id). All URLs are constructed from validated source-side templates. |

### 1.4 MCP server inputs

Source: MCP host (LLM agent loop). Trust level: **untrusted** — even when the host is a
trusted application, the agent may relay attacker-controlled paper text or hallucinated
identifiers.

| Vector | Mitigation |
|---|---|
| Hallucinated DOI | `INVALID_REF` returned; never crashes the server. |
| Prompt injection from paper abstract | doiget never reads paper content; tool inputs are typed, not free text. |
| `fetch_url(url: ...)` style abuse | Permanent non-goal. No tool accepts a URL. |
| Crafted long ref to overflow log | `DOI_SUFFIX_MAX_LEN = 256` truncate-or-reject before log. |
| Per-session fetch flood | `MAX_CONCURRENT_FETCHES = 5` (process-wide), `MCP_BATCH_MAX_SIZE = 100`, queue depth `MCP_QUEUE_DEPTH_MAX = 100` returns `RATE_LIMITED`. |
| Crafted Crossref response that misroutes a fetch | All fetch URLs are constructed from validated identifiers; we trust the source's TLS-authenticated response only for OA URL discovery, then re-validate the resulting URL against the per-source allowlist. |

### 1.5 Filesystem (Store, Cache, Log)

| Vector | Mitigation |
|---|---|
| Path traversal in safekey | `safekey` algorithm replaces every character outside `[A-Za-z0-9._\-_]` with `_`. Reference test vectors in [`SAFEKEY.md`](SAFEKEY.md). |
| Concurrent writers (BiblioFetch.jl + doiget) | `flock` on `<safekey>.toml.lock`, 5s timeout. ([`STORE.md`](STORE.md) §Contract 2) |
| Partial write on crash | Write to `<safekey>.toml.tmp` → `fsync` → `rename` → `fsync` parent. ([`STORE.md`](STORE.md) §Contract 3) |
| Log file tampering | SHA-256 hash chain; `chattr +a` attempted on Linux; `doiget audit-log --verify` recomputes the chain. |
| Disk-full DoS via large PDFs | Per-fetch size cap; on disk-full the fetch errors and the partial temp file is cleaned up. |
| Credential file readable to other users | Startup warns if `credentials.toml` permissions are not `0600` on POSIX. |

### 1.6 Secrets / credentials

| Vector | Mitigation |
|---|---|
| Bundled API key in binary | Banned by code review; CI greps source for known publisher key formats. No constant string in source matching `sk-`, `Bearer `, etc. |
| Logged in raw form | All credential types are `secrecy::Secret<String>`; `Display` and `Debug` print `****`; `tracing` uses a redactor for known field names. |
| Leaked via error message | Errors avoid printing source URLs that contain query-param keys (e.g., `?apikey=...`). |
| Persisted in shell history | Recommend `~/.config/doiget/credentials.toml` over inline env in shell rc; documented in [`CONFIG.md`](CONFIG.md). |

### 1.7 PDF content (after fetch)

doiget does **not** parse PDF content (ADR-0003). Malicious PDFs (embedded JS, exploits)
are stored as opaque blobs; their handling is the responsibility of any downstream tool
the user pipes the path into.

This is a deliberate design choice. doiget does not implement countermeasures for
malicious PDFs because doiget does not interact with their content.

### 1.8 Concurrent processes (multiple `doiget` invocations)

| Vector | Mitigation |
|---|---|
| Race on store write | `flock` (see 1.5). |
| Log write interleaving | Process-local mutex on log appender; fsync per write in audit-grade mode. |
| Cache race | `~/.cache/doiget/` writes go through atomic rename. |

### 1.9 Supply chain

| Vector | Mitigation |
|---|---|
| Malicious dependency update | `cargo-vet` audit chain; `cargo-deny` allowlist; pinned `Cargo.lock`. |
| Hijacked author GitHub account | 2FA required; verified-signed commits enforced on `main`; release workflow gated by GitHub Environment with manual approval. |
| Malicious release artifact swap | Sigstore keyless signing of release binaries; verifiable with `cosign verify-blob`. |
| `cargo publish` token leak | Use crates.io trusted publishing (OIDC) — no long-lived token in repo. |
| 3rd-party Action injection | All Actions pinned by SHA, not floating tag; Dependabot updates SHAs. |
| Reproducible builds | `Cargo.lock` committed; `rust-toolchain.toml` pins rustc; `RUSTFLAGS` fixed in release-plz.yml. |

### 1.10 Network side channel

doiget cannot prevent third parties (ISP, institution DNS resolver, transit network) from
observing **the existence** of fetches, even with correctly configured TLS:

- DNS lookups for `api.elsevier.com`, `unpaywall.org`, etc., are visible to the resolver.
- TLS SNI is plaintext on networks that do not implement Encrypted ClientHello.

doiget honors the user's `HTTPS_PROXY` environment variable; users who require
unobservability should configure their network layer (Tor, VPN) externally. doiget does
not provide its own proxying or anonymization.

doiget sends a stable `User-Agent` header per fetch to comply with each source's
politeness policy:

```
User-Agent: doiget/<version> (+https://github.com/sotashimozono/doiget)
```

### 1.11 Auto-update / telemetry

doiget contains no auto-update path, no version check, no crash report transmission, and
no usage analytics. (ADR-0015) These are denied at the dependency level via `cargo-deny`
to prevent inadvertent introduction.

## 2. Defense-in-depth controls

The following controls are established:

- [`Cargo.lock`](../Cargo.lock) committed.
- `cargo audit` and `cargo deny check` in CI (`audit.yml`).
- `cargo-vet` baseline.
- `posture-lint.yml` denying telemetry / HTTP server / self-update crate imports.
- `safekey-vectors.yml` validating 100 reference vectors against the algorithm.
- `cross-tool-compat.yml` round-tripping a sample DOI through BiblioFetch.jl + doiget.
- Branch protection on `main`: required PR review, status checks must pass, signed
  commits.
- Author 2FA mandatory.

## 3. MCP server additional controls

- `clippy::print_stdout` denied workspace-wide (and especially in `doiget-mcp`).
- `tracing-subscriber` global writer redirected to stderr; `std::panic::set_hook`
  redirects panic output to stderr.
- `mcp-smoke.yml` asserts that `doiget serve | head -c 1` over its stdin/stdout produces
  only well-formed JSON-RPC frames after `initialize` (zero stray bytes on stdout).
- All tool inputs validated with `serde` strict mode and explicit JSON Schema declared in
  `inputSchema`.

## 4. Release additional controls

- crates.io trusted publishing (OIDC).
- GitHub Environment-protected release workflow (manual approval).
- Sigstore keyless signing of binaries.
- `cargo-sbom` SPDX SBOM per release artifact.
- musl-static (Linux), universal (macOS), msvc (Windows). No glibc, gnu, or openssl
  variants.

## 5. Vulnerability disclosure

See [`../CONTACT.md`](../CONTACT.md) §"Security disclosures". Do not file a public issue.

## 6. Limitations (transparently acknowledged)

doiget cannot defend against:

- A user who deliberately misconfigures their environment to violate a publisher ToS.
- A network adversary who can rewrite TLS connections (CA compromise).
- An OS-level adversary with root / Administrator on the user's machine.
- A compromise of an upstream publisher API responding with malicious URLs that resolve
  to a host inside the per-source allowlist.

These are out of scope for doiget's threat model and are noted here to set realistic
expectations.
