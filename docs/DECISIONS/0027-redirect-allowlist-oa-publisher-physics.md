# 0027 - redirect-allowlist: add physics-society / diamond-OA hosts to `oa-publisher`

- **Date:** 2026-05-19
- **Status:** Accepted
- **Supersedes:** - (amends the `docs/REDIRECT_ALLOWLIST.md` §3.4 NORMATIVE host set; §3.4 remains binding with the added hosts)
- **Source:** #193 (dogfood of a finite-temperature-MPS corpus via the citation graph, #187)

## Context

`docs/REDIRECT_ALLOWLIST.md` §3.4 / `oa_publisher_allowlist()`
(`crates/doiget-core/src/http.rs`) is the host set the orchestrator
trusts for Unpaywall-discovered OA PDF URLs. The list was
bio/medical-leaning (PMC, bioRxiv, medRxiv, PLOS, Frontiers, MDPI,
Springer/Nature/Wiley/Elsevier) with **no physics-society OA hosts**.

Dogfooding a real finite-temperature-MPS literature collection
batch-fetched 30 DOIs that OpenAlex reports as Open Access:
**24 succeeded; 7 were denied** with `CAPABILITY_DENIED`
(`reason = redirect_not_in_allowlist`) — the OA PDF *was discovered* by
Unpaywall but its host was off-list:

| host | count | nature |
|------|------:|--------|
| `link.aps.org` | 3 | APS green OA (PRB/PRL/PRX) |
| `scipost.org` | 1 | SciPost — 100% diamond OA, community-run |
| `iopscience.iop.org` | 1 | IOP (New J. Phys. etc.) |
| `ruj.uj.edu.pl` | 1 | institutional repo (Jagiellonian) |
| `hdl.handle.net` | 1 | Handle resolver → repo |

The §3 docstring already flags every existing entry `(unverified)` and
states they "MUST be confirmed by a real fetch"; this run is exactly
that empirical pass.

## Decision

Add to the `oa-publisher` allowlist (`http.rs` + §3.4 + the site
projection), with an in-code empirical-evidence comment:

- `*.aps.org` — APS (`link.aps.org` / `journals.aps.org`). The
  registrable domain is **already trusted** elsewhere in the codebase:
  `tier_3_aps_allowlist()` lists `*.aps.org` under the credentialed
  `tdm-aps` source key. This ADR extends that trust to the
  `oa-publisher` source key for the green/gold OA route.
- `scipost.org`, `*.scipost.org` — SciPost, the canonical community-run
  **diamond-OA** physics publisher. Refusing it is hard to justify on
  supply-chain grounds.
- `*.iop.org` — IOP Publishing (`iopscience.iop.org`).

These entries are recorded as **empirically verified** (distinct from
the surrounding `(unverified)` entries) because a real fetch observed
Unpaywall resolving to them.

### Out of scope (deliberately excluded)

`hdl.handle.net` and `ruj.uj.edu.pl` are open-ended repository / handle
surfaces, not a bounded society-publisher host. They are a separate
"institutional repository OA" question and are **not** added here. Per
§5 the exclusion is recorded so a future ADR can revisit it on its own
merits.

## Consequences

**Positive.**

- The finite-temperature-MPS corpus goes from 24/30 → ~30/30 OA PDF
  success; physics corpora generally stop being needlessly depressed.
- SciPost (diamond OA) and APS/IOP (society OA) — legitimately,
  unambiguously OA content — are now fetchable rather than silently
  degraded to metadata-only.
- `*.aps.org` trust is now consistent across the `tdm-aps` and
  `oa-publisher` source keys.

**Negative.**

- The trusted redirect surface widens by four host patterns. Mitigated:
  all are bounded registrable-domain wildcards for established
  physics publishers (no open redirector / handle resolver added); the
  same per-source redirect-closure (`SourceAllowlist::matches`,
  dot-boundary enforced) applies, and the addition is empirically
  driven rather than speculative.

**Process (REDIRECT_ALLOWLIST.md §5).**

1. **ADR** — this file.
2. **CHANGELOG** — `[0.2.1-beta.6] → Changed` entry referencing this ADR.
3. **Reference / projection** — §3.4 table + Host families updated;
   `site/content/developer/redirect-allowlist.md` re-projected via
   `scripts/sync_docs_to_site.sh`.
4. **Tests** — `oa_publisher_allowlist_matches_known_oa_hosts` asserts
   `link.aps.org` / `journals.aps.org` / `scipost.org` /
   `www.scipost.org` / `iopscience.iop.org` match, plus dot-boundary
   negatives (`notaps.org`, `evilscipost.org`, `notiop.org`).

To revise this decision, write a new ADR with
`Supersedes: 0027` and update this file's `Status:` per
`CONTRIBUTING.md`.
