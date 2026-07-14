# Security policy

doiget treats security reports seriously. This file is a pointer so GitHub's
Security tab can find the disclosure path. The full normative security policy
(threat surfaces, supply-chain controls, and posture) lives in
[`docs/SECURITY.md`](../docs/SECURITY.md); the contact channel and SLA live in
[`CONTACT.md`](../CONTACT.md).

## Reporting a Vulnerability

Please use **GitHub Private Vulnerability Reporting**:

  https://github.com/QAtlasHub/doiget/security/advisories/new

Public issues, public discussions, and public pull requests are **not** the
right channel for security reports. If GitHub PVR is unavailable to you, see
[`CONTACT.md`](../CONTACT.md) §"Security disclosures" for the documented email
fallback.

## Acknowledgement timeline

Per [`CONTACT.md`](../CONTACT.md) §"Service-Level Agreement":

- **First response:** within **7 calendar days** of receipt (extending up to
  14 days during documented extended absence).
- **Substantive response:** within **30 calendar days**.
- Coordinated disclosure is preferred. Reporters who wish to be credited will
  be acknowledged in `CHANGELOG.md` and any subsequent advisory.

## Scope

**In scope:** `doiget-core`, `doiget-cli`, `doiget-mcp` (under `crates/`).

**Out of scope:**

- Third-party services queried by doiget (Crossref, Unpaywall, arXiv, etc.).
  Report those to the respective providers.
- TDM features (Phase 5; opt-in user builds gated by a Cargo feature flag per
  [ADR-0002](../docs/DECISIONS/0002-tdm-feature-gated.md)) unless the issue
  also affects the default build.
- The contents of fetched PDFs ([ADR-0003](../docs/DECISIONS/0003-pdf-content-out-of-scope.md)).

## Supported versions

| Version                    | Status        |
|----------------------------|---------------|
| Latest released version    | Supported     |
| Pre-1.0 development (0.x)  | Best-effort   |
| Older 0.x releases         | Not supported |

See [`CHANGELOG.md`](../CHANGELOG.md) for the current release state.

## Reference

- [`docs/SECURITY.md`](../docs/SECURITY.md) — **NORMATIVE** security policy
  (threat surfaces, supply-chain controls, eight-safeguard legal stance per
  [ADR-0019](../docs/DECISIONS/0019-eight-safeguards.md), no telemetry per
  [ADR-0015](../docs/DECISIONS/0015-no-telemetry.md)).
- [`CONTACT.md`](../CONTACT.md) — contact channel, SLA, and email fallback.
