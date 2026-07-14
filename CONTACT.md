# Contact

This document is the formal contact channel for **takedown requests**, **publisher concerns**,
**security disclosures**, and other correspondence to which doiget will respond on a defined
timeline.

For general questions, feature requests, or community discussion, please use
[GitHub Issues](https://github.com/QAtlasHub/doiget/issues) or
[GitHub Discussions](https://github.com/QAtlasHub/doiget/discussions).

## Primary contact

- **Maintainer:** Sota Shimozono
- **Email:** souta.shimozono@gmail.com
- **GitHub:** [@sotashimozono](https://github.com/sotashimozono)

## Service-Level Agreement (SLA)

| Stage | Target |
|---|---|
| First response (acknowledgement) | **within 7 calendar days** of receipt |
| Substantive response (actionable plan or resolution) | **within 30 calendar days** |

If you do not receive a first response within 7 days, please re-send the message and CC the
fallback contact below.

If the maintainer is unavailable due to extended absence (illness, conference travel,
personal emergency), the first-response window may extend up to **14 days**. The repository
README will carry an "Out of office" notice in such cases when feasible.

## Fallback contact

If the primary contact is unreachable for more than 14 days, you may file a public issue
with the title prefix `[CONTACT-FALLBACK]` on the
[doiget issue tracker](https://github.com/QAtlasHub/doiget/issues). This creates a
public record of attempted contact while preserving privacy of the underlying request body
(only the existence of the request is public).

## Takedown requests

If you are a copyright holder, publisher legal representative, or other party with a
legitimate concern about doiget's behavior or scope, please email the primary contact above
with the subject line:

```
[doiget takedown] <brief description>
```

Include in the body:

1. Your identity and the entity you represent.
2. The specific behavior, source integration, or feature of concern.
3. The legal or contractual basis for the request, with references where possible.
4. The remedy you are seeking (e.g., disabling a specific source, removing a feature,
   clarifying documentation, etc.).

The maintainer will:

- Acknowledge receipt within the SLA above.
- Substantively respond, including any concrete remedial action, within 30 days.
- Comply with reasonable, well-grounded requests including but not limited to feature
  removal, source disabling, or repository takedown.
- Document the action taken in [`CHANGELOG.md`](CHANGELOG.md), redacted as necessary.

## DMCA notices

doiget does not host or redistribute paper PDFs. PDFs retrieved by doiget reside on the
end user's local filesystem and are subject to that user's own access rights.

If you nevertheless believe a doiget feature or release artifact infringes copyright in a
manner that is doiget-attributable rather than user-attributable, please follow the
takedown procedure above. If a formal DMCA notice is required:

- Send to the primary contact with subject `[doiget DMCA]`.
- Include all elements required by 17 U.S.C. §512(c)(3).
- The maintainer reserves the right to file a counter-notice in cases where the request
  appears to misidentify doiget as a hosting provider for content that resides only on a
  user's local machine, or where the request appears to target a substantial non-infringing
  use of doiget as an automation tool.

## Security disclosures

If you believe you have found a security vulnerability in doiget (including but not limited
to: secret leakage, SSRF, log injection, supply-chain compromise, privilege escalation in
the MCP transport), please **do not** file a public issue. Email the primary contact with
subject:

```
[doiget security] <one-line summary>
```

The maintainer will:

- Acknowledge within the SLA.
- Coordinate a fix and a coordinated public disclosure.
- Credit reporters who wish to be acknowledged in the changelog and a future advisory.

See also [docs/SECURITY.md](docs/SECURITY.md) for the threat model and supply-chain
practices.

## Out of scope

This contact channel is **not** a customer support line for end-user CLI questions, MCP
host configuration help, or feature requests. Please use GitHub Issues or Discussions for
those.

This channel is also **not** a route for requests to bypass access control mechanisms,
share credentials, host paper content, or otherwise undermine the posture documented in
[docs/LEGAL.md](docs/LEGAL.md). Such requests will be declined.
