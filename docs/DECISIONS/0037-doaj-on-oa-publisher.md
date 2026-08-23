# 0037 - `doaj.org` on `oa-publisher` unconditionally; `trust_oa_registries` for the rest

- **Date:** 2026-08-22
- **Status:** Accepted
- **Supersedes:** - (amends the `docs/REDIRECT_ALLOWLIST.md` §3.4 NORMATIVE host set, in the manner of [0027](0027-redirect-allowlist-society-hosts.md))
- **Source:** #405 item 3, #409 (which found the asymmetry), #410

## Context

0.8.7 shipped `[network] trust_oa_registries` (#410), an opt-in flag adding a
curated set of OA registries — DOAJ, SciELO, Zenodo, OSF, HAL, CORE — to the
allowlist. It was chosen over widening the default because widening a default
supply-chain posture deserves an ADR, and there was not one.

#409 then established a fact that changes the DOAJ half of that argument:

```rust
// crates/doiget-core/src/http.rs, tier_2_allowlist()
SourceAllowlist::new("doaj", vec!["doaj.org".to_string(), "*.doaj.org".to_string()]),
```

**`doaj.org` is already trusted in this codebase.** Just under the `"doaj"`
*metadata* source key, not under `"oa-publisher"` — the key a Unpaywall-discovered
PDF redirect is actually checked against. And `tier_2_allowlist()` is wired into
the CLI only under `#[cfg(feature = "citation")]`, so in a stock `cargo install`
build that trust is not reachable at all.

So the question is not "should doiget extend trust to DOAJ" — the project already
decided it trusts DOAJ. It is "why do two keys disagree about a host we have
already accepted".

[ADR-0027](0027-redirect-allowlist-society-hosts.md) answered exactly this shape
for `*.aps.org`, which was trusted under the feature-gated `tdm-aps` key and was
promoted to `oa-publisher` — in its words, "making the trust unconditional across
feature configurations rather than feature-gated".

0027 also drew a line for what it would *not* add: `hdl.handle.net` and
`ruj.uj.edu.pl`, "open-ended repository / handle surfaces, not a bounded
society-publisher host". DOAJ falls on the include side of that line — it is a
bounded, curated index of vetted open-access journals, not an open-ended surface.

The cost of the disagreement is concrete. `10.1109/access.2024.3495502` (IEEE
Access, **gold OA**) is denied at `doaj.org` on a stock install, while a green-OA
copy on an institutional repository is reachable behind one documented flag. For
an OA-first tool that polarity is backwards.

## Decision

**Add `doaj.org` and `*.doaj.org` to `oa_publisher_allowlist()`** (`http.rs` +
`REDIRECT_ALLOWLIST.md` §3.4 + the site projection), unconditionally, on the
0027 precedent.

**Keep `trust_oa_registries` for the remaining five** — SciELO, Zenodo, OSF, HAL,
CORE. None of them appears anywhere in `http.rs` today, so for those the flag is
genuinely *new* trust, not the repair of an internal disagreement, and opt-in is
the right default. DOAJ is removed from the flag's curated set: keeping it in both
places would be harmless but misleading about where the trust comes from.

This is option 3 of the three laid out on #410.

## Consequences

**Positive.**

- The two keys agree. A host the project already trusts for metadata is trusted
  for the PDF leg it was always going to redirect to.
- Gold OA is reachable out of the box, matching the tool's stated purpose.
- Consistent with 0027 rather than a new principle.

**Negative / accepted.**

- The default allowlist grows by one registrable domain plus its subdomains. DOAJ
  is a non-profit index that serves article links, not arbitrary content; the
  supply-chain exposure is a redirect target we already accept from the same
  organisation on another key.
- `trust_oa_registries`, shipped one release earlier with DOAJ in it, changes
  meaning slightly. It is one release old, opt-in, off by default, and documented
  in `CONFIG.md` §3.1; the section is updated in the same change.

**Explicitly still out of scope.** IEEE / ACM / SIAM / AMS — see
[0039](0039-publisher-hosts-stay-off-allowlist.md).
