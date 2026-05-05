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
- A hard-coded rate limit (5 concurrent fetches, 5/second) prevents bulk-scraping
  patterns and cannot be overridden by configuration.

## 3. Tool-neutrality framing

doiget is positioned as a **general-purpose automation tool** in the sense familiar from
prior cases involving recording devices, format converters, and protocol clients. A
browser is not held liable for the contents a user fetches with it; a feed reader is not
held liable for the feeds a user subscribes to.

doiget likewise:

- Performs no content interpretation, summarization, or republication.
- Receives all access credentials from the running user, not from the maintainer.
- Records every fetch in a local provenance log under user control.
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

## 6. The eight safeguards

### Social safeguards

1. **No bundled credentials.** No publisher API key is shipped in any doiget binary.
   Credentials are read at runtime from environment variables or
   `~/.config/doiget/credentials.toml`, wrapped in `secrecy::Secret`, and never
   logged in raw form.

2. **Opt-in TDM agreement.** Each TDM-class source requires the user to set
   `DOIGET_AGREE_TDM_<PUBLISHER>=1` as an explicit acknowledgement of the publisher's
   ToS. Missing or stale agreements fail closed.

3. **User responsibility documented.** [`SOURCES.md`](SOURCES.md) lists every source's
   official ToS link and explicitly states the user's responsibility for compliance. The
   README front-loads this point in the Posture section.

4. **Takedown contact.** [`../CONTACT.md`](../CONTACT.md) defines an SLA-bound channel
   (7 days first response, 30 days substantive response) for publisher legal teams or
   other parties with concerns.

5. **Marketing language self-policing.** A CI workflow
   (`.github/workflows/posture-lint.yml`) scans `README.md`, `docs/`, and any blog draft
   for prohibited terms ("bypass", "circumvent", "free papers", "Sci-Hub alternative")
   and fails any PR that introduces them.

### Technical safeguards

6. **Compile-time feature gating.** Each TDM source is behind a Cargo feature
   (`tdm-elsevier`, `tdm-aps`, `tdm-springer`). Default builds and crates.io artifacts
   contain no TDM source code. See ADR-0002.

7. **Runtime CapabilityProfile.** All `Source` implementations require a
   `&CapabilityProfile` parameter at the type level. A source whose capability is not
   granted at startup cannot be invoked. See [`CAPABILITY.md`](CAPABILITY.md) and
   ADR-0005.

8. **Hard-coded rate limit.** `MAX_CONCURRENT_FETCHES = 5` and
   `MAX_FETCHES_PER_SECOND = 5.0` are library constants, not configuration values. A user
   cannot override them by flag, env var, or configuration file. This prevents
   bulk-scraper recognition by any source.

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
[GitHub Discussions](https://github.com/sotashimozono/doiget/discussions).
