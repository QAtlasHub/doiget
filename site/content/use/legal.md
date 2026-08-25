+++
title = "Legal posture"
description = "This document is the canonical statement of doiget's legal posture. It exists so that"
weight = 140
+++

# Legal posture

> **Status: NORMATIVE.** This document defines binding contracts. Implementations and
> contributors MUST conform. Changes require a new ADR in [`DECISIONS/`](DECISIONS/) and
> review by the maintainer.

This document is the canonical statement of doiget's legal posture. It exists so that
users, contributors, publisher legal teams, and future reviewers can locate a single
authoritative description of what doiget is, what it is not, and the basis on which it
operates.

---

## 1. Posture (one paragraph)

doiget is a general-purpose automation tool for retrieving academic papers via official
publisher APIs. It only attempts retrieval through (a) public Open Access sources and
(b) credentials the user has personally configured for their own institutional or personal
subscriptions. It does not bypass any access control mechanism, redistribute papers, host
content, operate as a SaaS, or bundle any publisher API keys. Users are responsible for
ensuring they have the right to access the content they request and for compliance with
each source's Terms of Service.

## 2. Own-network-only access (binding constraint)

A core design constraint of doiget — added explicitly to the posture in 2026-05 — is that
**doiget only retrieves content that is reachable through the running user's own network
and credentials.** doiget does not proxy, share, or relay access of any kind across user
boundaries.

This is enforced structurally rather than only by documentation:

- Default released binaries include only Open Access sources (Crossref, Unpaywall, arXiv).
- Institutional / TDM source code paths are gated by Cargo features (`tdm-elsevier`,
  `tdm-aps`, `tdm-springer`) and are **not present** in the default published binary; a
  user wishing to enable them must rebuild from source. See [`SCOPE.md`](SCOPE.md) and
  ADR-0002.
- Even when compiled in, TDM sources require both an explicit per-publisher
  agreement environment variable (`DOIGET_AGREE_TDM_<PUBLISHER>=1`) **and** a
  user-provided API key. Both must be present, otherwise the source is unavailable at
  runtime. See [`CAPABILITY.md`](CAPABILITY.md).
- A hard-coded rate limit (5 concurrent fetches, 5/second — stricter still for
  sources whose terms say so, e.g. arXiv at 1 request / 3 s) prevents bulk-scraping
  patterns and cannot be overridden by configuration.

## 3. Tool-neutrality framing

doiget is positioned as a **general-purpose automation tool** in the sense familiar from
prior cases involving recording devices, format converters, and protocol clients. A
browser is not held liable for the contents a user fetches with it; a feed reader is not
held liable for the feeds a user subscribes to.

doiget likewise:

- **Performs no PDF content interpretation, summarization, or republication.**
  PDFs are stored as opaque blobs; doiget does not extract text, run OCR,
  generate summaries, or parse citations from PDF content. Bibliographic
  metadata (title / authors / venue / abstract / keywords) is consumed from
  publisher APIs and stored in the local TOML metadata for `bib` / `csl` /
  `search_local` operations — that is bibliographic indexing, distinct from
  content interpretation. The PDF content boundary is documented as a
  Permanent Non-Goal in [`SCOPE.md`](SCOPE.md) and ADR-0003.
- Receives all access credentials from the running user, not from the maintainer.
- Records every fetch in a local provenance log under user control (best-effort
  tamper-evident; see [`PROVENANCE_LOG.md`](PROVENANCE_LOG.md) §8).
- Operates only on the local user's behalf, with no network listening surface.

Tool-neutrality is a framing principle, not a guarantee against any specific legal
outcome in any specific jurisdiction. See §5 below.

## 4. The user is the contract party

For every source doiget integrates with, the **user** is the party who:

- Holds the API key (where required).
- Accepts the source's Terms of Service (typically by registering for the API).
- Bears institutional access rights (e.g., campus subscription).
- Is identified in API request audit logs by their key, IP, or institutional credential.

doiget the project is not a contracting party with any publisher. doiget the maintainer
does not hold publisher API keys, does not negotiate publisher contracts, and does not
operate any service that proxies user requests through a maintainer-controlled endpoint.

## 5. Jurisdictional caveat

The posture above relies on:

- The general principle of tool-neutrality (informed by, but not legally identical to,
  cases like *Sony Corp. of America v. Universal City Studios, Inc.* (US, 1984)).
- The structural fact that the user is the contract party with each source.
- The absence of any access-control circumvention.

These are reasoned, defensible positions. They are **not** specific case-law guarantees in
any jurisdiction. The doiget maintainer is based in Japan; major publisher entities are
based variously in the United States, the Netherlands, Germany, and elsewhere.
Cross-border Internet utilities like doiget are subject to whichever jurisdiction's
courts a party chooses to invoke. A reasonable, well-grounded takedown request from any
jurisdiction will be evaluated on its merits per [`../CONTACT.md`](../CONTACT.md).

doiget makes no claim that the posture above will prevail in any specific case in any
specific jurisdiction. The posture is offered in good faith and is operationally
defended by the safeguards in §6.

## 6. Safeguards

doiget's posture is defended by two distinct kinds of safeguard. The distinction
matters: the first kind is a control the codebase or CI pipeline mechanically
enforces; the second is a policy commitment the maintainer makes and intends to
honor but which a determined contributor or future maintainer could weaken
without machine-checkable resistance.

### 6a. Enforced controls (5)

These are mechanically enforced by code, type system, Cargo, or CI. Removing
them requires changing source files that are gated by branch protection.

1. **No bundled credentials.** No publisher API key is shipped in any doiget
   binary. Credentials are read at runtime from environment variables or
   `~/.config/doiget/credentials.toml`, wrapped in `secrecy::Secret`, and never
   logged in raw form. *Enforced by:* `secrecy::Secret` types in
   `doiget-core`; `tracing` redactor; CI grep for embedded key patterns.

2. **Opt-in TDM agreement (per-publisher).** Each TDM-class source requires
   the user to set `DOIGET_AGREE_TDM_<PUBLISHER>=1` AND provide
   `DOIGET_KEY_<PUBLISHER>`. Missing or partial configurations fail closed at
   `CapabilityProfile::from_env`. *Enforced by:* `CapabilityProfile`
   resolution algorithm ([`CAPABILITY.md`](CAPABILITY.md) §2 rules 2 and 3).

3. **Compile-time feature gating.** Each TDM source is behind a Cargo feature
   (`tdm-elsevier`, `tdm-aps`, `tdm-springer`). Default builds and crates.io
   artifacts contain no TDM source code. *Enforced by:* `Cargo.toml`
   `[features]` declarations; `posture-lint.yml` import-pattern grep;
   ADR-0002.

4. **Runtime CapabilityProfile.** All `Source` implementations require a
   `&CapabilityProfile` parameter at the type level. A source whose capability
   is not granted at startup cannot be invoked. *Enforced by:* `Source` trait
   signature in `doiget-core`; `#[non_exhaustive]` on `CapabilityProfile`;
   ADR-0005.

5. **Hard-coded rate limit.** `MAX_CONCURRENT_FETCHES = 5` and
   `MAX_FETCHES_PER_SECOND = 5.0` are library constants. The struct
   `RateLimits` exposes only `HARD_CODED`; field visibility is `pub(crate)`,
   so external callers cannot synthesize a `RateLimits` with different
   values.

   Those two are the **ceiling**, not the whole limit. A source whose vendor
   publishes something stricter is held to the stricter value, from the
   `SOURCE_RATE_OVERRIDES` table (ADR-0045) — arXiv, for instance, at one
   request every three seconds over a single connection. Table entries are
   library constants for the same reason `RateLimits` is: an override a
   caller could supply would hand back exactly what this safeguard
   withholds. `RateLimits::backoff_ms_for` and `max_concurrent_for` return
   the stricter of the global value and the entry, so an entry can only ever
   tighten. *Enforced by:* `pub(crate)` field visibility,
   `#[non_exhaustive]` on `RateLimits`, the `max`/`min` in those two
   accessors, and smoke tests in `lib.rs::tests` and
   `rate_limiter.rs::tests` — including one that fails the build if a table
   entry is looser than the global cap and would therefore be silently
   ignored.

### 6b. Policy commitments (3)

These are commitments the maintainer makes, but a future contributor could
violate them without the type system or CI reliably catching it. They are
real safeguards in the sense that the maintainer intends to keep them and
will reject contradicting PRs, but they rely on human review.

6. **User responsibility documented.** [`SOURCES.md`](SOURCES.md) lists every
   source's official ToS link and explicitly states the user's responsibility
   for compliance. The README front-loads this point in the Posture section.
   *Mechanism:* documentation; CI does not assert that the wording remains in
   place over time.

7. **Takedown contact with SLA.** [`../CONTACT.md`](../CONTACT.md) defines an
   SLA-bound channel (7 days first response, 30 days substantive response)
   for publisher legal teams or other parties with concerns. *Mechanism:*
   maintainer commitment; the SLA itself is not machine-asserted.

8. **Marketing-language self-policing.** A CI workflow
   (`.github/workflows/posture-lint.yml`) scans **`README.md` only** for
   prohibited terms (`bypass`, `circumvent`, `free papers`, `sci-hub`) and
   fails any PR that introduces them in the README. *Scope deliberately
   narrow:* the policy / legal docs (LEGAL, SCOPE, CONTACT, CONTRIBUTING)
   legitimately need to use these words to describe what doiget does **not**
   do. README is the front-page marketing surface where positive uses are
   the actual concern. The other steps in `posture-lint.yml` (forbidden HTTP
   server / telemetry / TLS-backend imports) scan source code and ARE
   enforced controls; they belong in §6a above and are listed there
   indirectly via #3.

### Why the split matters

A reader (publisher legal team, security researcher, future maintainer) who
reads "safeguards" and assumes mechanical controls will be over-confident if
items 6–8 are presented identically to 1–5. The wording in §1 (and in
README's Posture section) intentionally uses neutral language; this section
spells the distinction out so the picture stays honest.

## 7. Risk planning

doiget does not publish probability estimates of legal action because we lack data to
ground them. We instead **plan against the worst plausible case**: a single contested
takedown or formal legal action whose remediation cost remains within the maintainer's
self-described affordable bound (on the order of ¥1–3 million in the worst plausible
scenario).

The eight safeguards above are designed to reduce both the probability and the severity
of such an event without relying on probability assumptions.

## 8. Permanent non-goals (legal-relevant subset)

The full list is in [`SCOPE.md`](SCOPE.md). Items relevant here:

- No SaaS / hosted `doiget.example` service.
- No MCP HTTP / SSE / WebSocket transport (would shift doiget toward multi-tenant).
- No paper hosting, redistribution, or "share-vault" feature.
- No credential sharing between users.
- No bulk download mode (the rate limit is the hard upper bound).
- No `tdm-all` umbrella feature flag (each TDM source must be opted in individually).

## 9. Provenance log

Every fetch is recorded locally in `~/.config/doiget/access.log` as a JSON Lines record
with a SHA-256 hash chain. The log is **fail-closed**: a fetch that cannot be logged is
not allowed to proceed. See [`PROVENANCE_LOG.md`](PROVENANCE_LOG.md) for the format and
ADR-0006 for the design rationale.

The log is local-only. doiget does not transmit log data anywhere.

## 10. Telemetry and self-update

doiget contains no telemetry, no phone-home, no version check, no crash report
transmission, and no self-update mechanism. These are permanent non-goals (ADR-0015) and
are enforced by `cargo-deny` denials of relevant crates.

## 11. Inquiries

For takedown requests, formal legal correspondence, or security disclosures, see
[`../CONTACT.md`](../CONTACT.md).

For general questions and discussion, please use
[GitHub Discussions](https://github.com/QAtlasHub/doiget/discussions).
