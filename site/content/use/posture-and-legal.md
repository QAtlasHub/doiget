+++
title = "Posture & legal"
description = "What doiget will and will not do. Scope, eight safeguards, and legal posture."
weight = 80
+++

doiget is a general-purpose automation tool for retrieving papers through
official publisher and aggregator APIs. This page summarizes what doiget
will and will not do; the binding specs live in
[docs/SCOPE.md]({{ config.extra.github_url }}/blob/main/docs/SCOPE.md)
and
[docs/LEGAL.md]({{ config.extra.github_url }}/blob/main/docs/LEGAL.md).

## What doiget does

- Retrieves PDFs from sources where the user has access through one of:
  1. **Public Open Access sources** (default) &mdash; Crossref, Unpaywall, arXiv.
  2. **The user's own institutional or personal credentials** (opt-in,
     compile-time gated per publisher) &mdash; Springer Nature OA,
     APS Harvest, Elsevier ScienceDirect TDM.
- Writes the resulting PDF and a TOML metadata sidecar to the user's
  local filesystem under `~/papers/`.
- Maintains a cryptographic provenance log (JSON Lines + SHA-256 hash
  chain) of every fetch attempt.

## What doiget does not do

- **Does not bypass paywalls.** Sources that require credentials are
  compile-time gated and need user-supplied credentials to activate.
- **Does not host content.** Retrieved PDFs land on the user's own
  filesystem. doiget does not operate as a public mirror or SaaS.
- **Does not bundle publisher API keys.** Each user supplies their own.
- **Does not relicense fetched content.** The MIT license applies only
  to doiget's own source code; retrieved PDFs retain their original
  license, which is determined by each paper's own license, the
  publisher's API terms of service, and the user's access rights.
- **Does not send telemetry.** No phone-home, no auto-update, no
  analytics (ADR-0015).

## Eight safeguards (ADR-0019)

doiget enforces eight safeguards across five social and three technical
categories. The technical safeguards are:

1. **Capability profile gate** &mdash; sources are unreachable unless the
   capability profile resolved from env vars permits them.
2. **Redirect allowlist** &mdash; HTTP redirects are accepted only to a
   per-source allowlist; off-list hosts trip a structured
   `denial_context` error.
3. **Fail-closed provenance log** &mdash; any log-write failure aborts the
   fetch.

The five social safeguards (TDM author opt-in, takedown response,
removal mechanism, contact addressability, public posture) are
documented in
[docs/SCOPE.md]({{ config.extra.github_url }}/blob/main/docs/SCOPE.md).

## User responsibilities

Users are responsible for ensuring they have the right to access the
content they request and for compliance with each source's terms of
service. doiget is a tool; the legal posture of any fetched content is
determined by the user's own credentials, institutional affiliations,
and intended use.
