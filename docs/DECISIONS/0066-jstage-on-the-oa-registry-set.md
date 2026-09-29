# 0066 - J-STAGE on the opt-in OA-registry set

- **Date:** 2026-09-29
- **Status:** Accepted
- **Supersedes:** -
- **Amends:** the `trust_oa_registries` set ([`docs/CONFIG.md`](../CONFIG.md) §3.1; [0037](0037-doaj-on-oa-publisher.md) kept that set opt-in)
- **Builds on:** [0027](0027-redirect-allowlist-oa-publisher-physics.md), [0039](0039-publisher-hosts-stay-off-allowlist.md), [0046](0046-vendor-claims-are-normative-links-are-pointers.md)
- **Source:** #646 (#474, Gap 3)

## Context

J-STAGE (JST) is the platform of record for Japanese domestic journals. Measured
2026-09-29: six of eight J-STAGE DOIs sampled from J-STAGE's own search API have an OA
location in Unpaywall (`gold` / `bronze`, `url_for_pdf` on `www.jstage.jst.go.jp`), and
J-STAGE serves the PDF to a scripted client (200 `application/pdf`). doiget refused
every one: the host was on no allowlist.

J-STAGE's Terms and Policies (read 2026-09-29) permit use "such as private use or
citation" and prohibit "Acts of downloading a large amount of the Registered Data using
mechanical or equivalent means".

The `trust_oa_registries` set was defined as registries and repositories "whose
*purpose* is open distribution, never a publisher platform". J-STAGE is a platform, but
a national, non-commercial one whose default is free access; a restricted article on it
answers an HTML login page, not a PDF.

## Decision

**D1: `www.jstage.jst.go.jp` joins the opt-in `trust_oa_registries` set** -- not the
default `oa-publisher` list. The user's opt-in covers the terms' concern: fetching the
papers one reads is private use, and a large mechanical download is a choice the user
makes, knowingly.

**D2: The set's rule is widened, and says so.** Registries and repositories whose
purpose is open distribution, **or a national open-access platform whose default is
free access** -- never a commercial publisher platform. The two safeguards that make
the difference hold: only a location Unpaywall reports as open is followed (LEGAL §2a
(a)), and the `%PDF-` check refuses the login page a restricted article answers.

## Consequences

- With `trust_oa_registries = true`, J-STAGE's gold / bronze articles fetch.
- A commercial platform is still not a candidate for this set; ADR-0039's rule for
  publisher hosts is unchanged.
