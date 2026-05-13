+++
title = "Contact"
description = "Where to send takedown notices, security disclosures, and other formal correspondence."
weight = 90
+++

The canonical contact file is
[CONTACT.md]({{ config.extra.github_url }}/blob/main/CONTACT.md) in
the repository. This page reproduces its key entry points.

## Email

**General correspondence, takedown notices, security disclosures, and
publisher inquiries:** [souta.shimozono@gmail.com](mailto:souta.shimozono@gmail.com)

Please prefix the subject line with `[doiget]` so the message is
routed correctly.

## Security disclosures

For security issues, please follow the disclosure process documented in
[SECURITY.md]({{ config.extra.github_url }}/blob/main/docs/SECURITY.md).
Coordinated disclosure is welcome; please give a reasonable embargo
window (90 days by default) before public disclosure.

## Takedown requests

doiget does not host content. It retrieves PDFs through official APIs to
the user's own local filesystem. If you believe a specific source
integration violates your terms of service, please email the address
above with the source name and the offending behavior, and the relevant
build feature will be evaluated for removal or further gating.

## GitHub

- Public issues / PRs: [{{ config.extra.github_url | replace(from="https://", to="") }}]({{ config.extra.github_url }})
- Discussions: [{{ config.extra.github_url }}/discussions]({{ config.extra.github_url }}/discussions)
