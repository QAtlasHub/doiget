# Redirect allowlist

> **Status: NORMATIVE.** Binding for the doiget HTTP redirect policy. Changes require
> an ADR.

## 1. Purpose

Defense-in-depth against open-redirect SSRF and against publisher / metadata-source
responses being abused to misroute a fetch to an attacker-controlled host. Even though
[`SECURITY.md`](SECURITY.md) §1.3 already restricts redirects to `https://` and bounds
the redirect chain to ten hops, those mitigations alone do not stop a redirect to an
arbitrary attacker-owned HTTPS host. The redirect allowlist closes that gap by
constraining each source's redirect targets to a small, source-specific set of hosts
that the source legitimately uses.

The allowlist is consulted on **every** redirect hop, not only the final location, and
on the OA URL discovered through metadata sources before the actual PDF fetch is issued
(see [`SECURITY.md`](SECURITY.md) §1.4 entry on Crossref re-validation).

## 2. Format

The allowlist is a structured table keyed by `source` (the same `source` key used in
[`SOURCES.md`](SOURCES.md) §1).

### 2.1 Required fields per source

| Field | Type | Description |
|---|---|---|
| `source` | string | Source key. MUST match a `source` value in [`SOURCES.md`](SOURCES.md) §1 (e.g. `crossref`, `unpaywall`, `arxiv`). |
| `redirect_hosts` | array of strings | Allowed redirect target host patterns. Each entry is either an exact FQDN or a wildcard suffix pattern as defined in §2.2. |

### 2.2 Host matching rule (NORMATIVE)

Each entry in `redirect_hosts` matches a candidate redirect target host as follows.

1. The candidate host is the lowercased hostname of the redirect target URL — i.e.
   the value of `Url::host_str()` after parse, lowercased. Port, path, query, and
   fragment are ignored. Userinfo is rejected unconditionally.
2. **Exact-FQDN form**: an entry without a leading `*.` matches only when the
   candidate host is byte-identical to the entry, after lowercasing.
3. **Suffix-glob form**: an entry of the form `*.<suffix>` matches when the candidate
   host either equals `<suffix>` exactly **or** ends with `.<suffix>`. This means
   `*.example.com` matches both `example.com` and `cdn.example.com`, but does **not**
   match `notexample.com`.
4. The matching rule is byte-level on the lowercased ASCII form of the host. IDN
   hosts MUST be Punycoded before comparison; raw Unicode in `redirect_hosts` is a
   spec violation and rejected at config-load time.
5. A redirect is permitted if and only if at least one entry in the source's
   `redirect_hosts` matches. No global fallback; an empty or missing
   `redirect_hosts` for a source means "no redirects permitted from this source".

### 2.3 Reference encoding

The allowlist data is stored as a single TOML document at
`crates/doiget-core/src/sources/redirect_allowlist.toml`, embedded into the binary via
`include_str!` and parsed once at process start. The Phase 1 implementation is
expected to consume that file. Schema:

```toml
# Reference TOML form. The file declares one [[source]] entry per integrated source.
[[source]]
source = "crossref"
redirect_hosts = [
  "api.crossref.org",
  "*.crossref.org",
]

[[source]]
source = "unpaywall"
redirect_hosts = [
  "api.unpaywall.org",
]
# ...
```

The exact list of entries is given in §3.

## 3. Phase 1 entries

> **Informed-best-effort.** The hosts below are the canonical hosts each Tier 1
> source advertises in its public API documentation as of this document's authoring.
> They are NOT a substitute for empirical validation. The Phase 1 implementation MUST
> validate this list by replaying real fetches against representative DOIs / arXiv
> ids and adjusting the allowlist before merging the Phase 1 fetcher to `main`. Any
> entry below marked `(unverified)` MUST be either confirmed by a real fetch or
> removed.

The Tier 1 sources are taken from [`SOURCES.md`](SOURCES.md) §1: Crossref, Unpaywall,
arXiv. Tier 2 / Tier 3 sources are out of scope for Phase 1; see §4.

### 3.1 `crossref`

| Field | Value |
|---|---|
| `source` | `crossref` |
| `redirect_hosts` | `api.crossref.org`, `*.crossref.org` |

Notes:

- `api.crossref.org` is the documented endpoint host.
- `*.crossref.org` covers any internal Crossref subdomain redirects (e.g. legacy or
  CDN-fronted variants).
- Crossref's `link` array can contain publisher-side OA URLs whose host is NOT under
  `crossref.org`. Those URLs are NOT followed under the `crossref` source's
  allowlist; they are instead handed to the publisher-side fetch path, which is
  governed by the allowlist of the source that owns that publisher (Phase 2+
  responsibility — see §4).

### 3.2 `unpaywall`

| Field | Value |
|---|---|
| `source` | `unpaywall` |
| `redirect_hosts` | `api.unpaywall.org` |

Notes:

- `api.unpaywall.org` is the documented endpoint host.
- Unpaywall's response describes an OA URL hosted on a third-party server (publisher,
  preprint server, institutional repository). Redirects encountered while fetching
  that OA URL are NOT subject to the `unpaywall` allowlist; they are subject to the
  allowlist of the source that owns the publisher host. In Phase 1 the only Tier 1
  publisher-host source is `arxiv`; OA URLs that resolve to non-allowlisted hosts
  abort the fetch.

### 3.3 `arxiv`

| Field | Value |
|---|---|
| `source` | `arxiv` |
| `redirect_hosts` | `arxiv.org`, `export.arxiv.org`, `*.arxiv.org` |

Notes:

- `arxiv.org` and `export.arxiv.org` are the documented endpoint hosts (HTML / API
  vs. metadata export).
- `*.arxiv.org` covers redirects to subdomains arXiv may use for PDF delivery
  (`(unverified)` — Phase 1 implementation MUST confirm whether arXiv actually
  redirects PDF requests to a subdomain, and either keep or drop this entry based on
  observation).
- arXiv MAY in some configurations redirect to a CDN host outside `arxiv.org`. If
  the Phase 1 fetcher observes such a redirect, the response is to add the
  CDN's host suffix here via ADR — NOT to silently widen the allowlist at runtime.

## 4. Phase 2 / Phase 3 entries

| Source | Tier | Phase | Status |
|---|---|---|---|
| `openalex` | 2 | 4 | (reserved) |
| `semantic-scholar` | 2 | 4 | (reserved) |
| `doaj` | 2 | 4 | (reserved) |
| `springer-tdm` | 3 | 5a | (reserved) |
| `aps-tdm` | 3 | 5b | (reserved) |
| `elsevier-tdm` | 3 | 5c | (reserved) |

Each `(reserved)` entry will be filled in when its owning Phase begins, via the
update process in §5. Until then, attempts to use these sources are blocked by their
Cargo feature gate ([`SOURCES.md`](SOURCES.md) §3) and never reach the redirect
policy.

## 5. Update process

Changes to this allowlist are user-impacting: a fetch that previously worked may
stop working (a redirect target host is removed) or a fetch that previously failed
may start working (a host is added). Both directions are subject to the same
process:

1. **ADR.** Add or update a `docs/DECISIONS/NNNN-redirect-allowlist-<source>.md` ADR
   that names the source, lists the host(s) added or removed, and explains why
   (e.g., "observed in real fetch traces", "publisher migrated CDN").
2. **CHANGELOG.** Add an entry under `[Unreleased] -> Changed` (or `Added` / `Removed`
   as appropriate) in [`CHANGELOG.md`](../CHANGELOG.md) referencing the ADR.
3. **Reference file.** Update the TOML reference file described in §2.3.
4. **Tests.** Update or add a test in `crates/doiget-core/tests/` that asserts the
   new entry matches / does not match the relevant host strings, including the
   suffix-glob negative case (`notexample.com` MUST NOT match `*.example.com`).

The Phase 1 implementation MAY treat the very first population of the §3 entries as
in-scope of the Phase 1 ADR series rather than minting a separate allowlist ADR per
source. Subsequent changes always require a dedicated ADR.

## 6. Non-goals

- This document does NOT govern the initial fetch URL; that is constructed from
  validated identifiers via source-side URL templates and is bounded by the
  `https://`-only redirect policy in [`SECURITY.md`](SECURITY.md) §1.3.
- This document does NOT define rate-limiting, retry behavior, or politeness; see
  [`SOURCES.md`](SOURCES.md) §6.
- This document does NOT govern outbound DNS, proxying, or anonymization; see
  [`SECURITY.md`](SECURITY.md) §1.10.
