# 0039 - IEEE / ACM / SIAM / AMS stay off `oa-publisher`; TDM credentials are the route

- **Date:** 2026-08-22
- **Status:** Accepted
- **Supersedes:** - (settles the question [0027](0027-redirect-allowlist-society-hosts.md) §"Out of scope" deferred)
- **Source:** #407 (measured from a subscribing university network), #409

## Context

#407 reports that on a machine sitting directly on a subscribing university
network — egress `157.82.60.8`, RDAP `UTNET`, no tunnel — paywalled fetches still
fail, and identifies two independent causes:

1. `oa_publisher_allowlist()` has no `*.ieee.org`, `*.acm.org`, `*.siam.org` or
   `*.ams.org`, so doiget will not attempt the publisher leg for them.
2. The publisher's bot wall. One diagnostic request from that address, with a
   desktop browser User-Agent:

   ```
   $ curl -A "Mozilla/5.0 ... Chrome/120 ..." https://ieeexplore.ieee.org/document/8319344
   status=202   body=0 bytes
   ```

   `202 Accepted` with an empty body is a challenge holding response. The
   subscription is not the binding constraint; **being a scripted client is.**

Cause 1 is not an oversight. [ADR-0027](0027-redirect-allowlist-society-hosts.md)
scoped `oa-publisher` to physics-society and diamond-OA hosts and explicitly
deferred open-ended surfaces "so a future ADR can revisit it on its own merits".
This is that ADR.

The measurement in #407 is what makes it decidable, and it decides it against
adding them: **adding the hosts would not fix the fetch.** It would move the
failure from an allowlist denial — which is honest, immediate, and names a
policy — to a WAF challenge that looks like a successful `202`. That is strictly
worse diagnostics for zero new capability.

## Decision

**Do not add IEEE, ACM, SIAM or AMS hosts to `oa-publisher`.**

The supported route for programmatic access to those publishers is
per-publisher **TDM credentials**, the mechanism ADR-0002 already established and
`docs/CONFIG.md` §6 already documents for `[tdm.elsevier]`, `[tdm.aps]` and
`[tdm.springer]`. That interface is the one publishers intend programs to use and
it does not meet the web front end's bot wall at all.

`[tdm.ieee]` is **deferred, not rejected.** Adding it means the four touch points
an existing TDM source has (`sources/tdm_<vendor>.rs`, a transport allowlist in
`http.rs`, a Cargo feature per ADR-0002, a `[tdm.<vendor>]` block in CONFIG.md §6)
plus a ToS link in `SOURCES.md` — and it needs IEEE's actual API contract:
endpoint, auth header, response shape, and terms. None of that can be written
from the outside. Tracked as a follow-up.

0.8.7 ships the diagnostic instead: `doiget config doctor --network` reports which
publishers will talk to this client, classifies a `2xx` with an empty body as a
bot challenge rather than a success, and lists non-allowlisted hosts as
`not allowlisted` with no request sent. `docs/CONFIG.md` §6.1 states the honest
conclusion — IP-based subscription does not imply fetchability, and a proxy fixes
addressing but never a bot wall.

## Consequences

**Positive.**

- The default allowlist keeps its stated shape: OA routes only. Adding
  subscription publishers would blur the line that makes doiget deployable inside
  institutions and eligible for directory listings.
- The failure a user gets is the accurate one, at the earliest possible point,
  naming a policy they can act on.

**Negative / accepted.**

- IEEE / ACM / SIAM / AMS literature is not fetchable by doiget today, including
  from a subscribing network. For a corpus that is entirely IEEE — the case in
  #407 — doiget can resolve metadata but not full text.
- That gap closes only when someone signs up for the TDM APIs.
