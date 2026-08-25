# 0041 - Tier-3 TDM sources are scoped to their publisher's DOI prefixes

- **Date:** 2026-08-23
- **Status:** Accepted
- **Supersedes:** -
- **Source:** #442 (the sources were unreachable), #430 (blocked on it)

## Context

The three Tier-3 TDM sources shipped with no caller: implemented, feature-gated,
transport-allowlisted, unit-tested, and never invoked by the fetch path (#442).
Wiring them in raised a question their unit tests never had to answer, because
nothing ever asked them about a DOI they did not own.

`can_serve` was:

```rust
profile.tdm_aps.is_some() && matches!(ref_, Ref::Doi(_))
```

Any DOI, as long as the grant is present. Harmless while unreachable. Once wired,
a user who enables `tdm-aps` would send **every** DOI they look up to
`harvest.aps.org` — Elsevier DOIs, IEEE DOIs, DOIs from publishers APS has no
relationship with.

That is two problems, not one. It is impolite (`SOURCES.md` §6 sets a politeness
posture doiget otherwise keeps carefully). And it is a disclosure: it tells a
publisher the shape of a reading list they have no part in. doiget's privacy
claim is that it holds no credentials and runs locally; leaking the user's
queries to three publishers because they enabled one would undercut it.

Three options:

- **(a)** no scoping — consult every enabled TDM source for every DOI
- **(b)** scope by DOI registrant prefix, with a fixed default list
- **(c)** derive the publisher from Crossref metadata and route on that

## Decision

**(b) — each source declares the prefixes its publisher registered, and is
consulted only for those DOIs.**

```rust
pub(crate) const PUBLISHER_PREFIXES: &[&str] = &["10.1103"];   // tdm_aps
```

Checked in two places: `can_serve` (defensive, per the existing double-gate
pattern) and the orchestrator, which checks it *before* credentials so the two
failure modes stay distinguishable in the trace.

Prefix lists verified against the Crossref registrant registry
(`api.crossref.org/prefixes/<prefix>`) on 2026-08-23:

| feature | prefixes | registrant as returned by Crossref |
|---|---|---|
| `tdm-aps` | `10.1103` | American Physical Society (APS) |
| `tdm-elsevier` | `10.1016`, `10.1006`, `10.1053` | Elsevier BV |
| `tdm-springer` | `10.1007`, `10.1038`, `10.1057`, `10.1140` | Springer Science and Business Media LLC |

Scoped this way the disclosure is nil: a publisher is only ever told about DOIs
it issued, and resolving such a DOI goes through them anyway.

**(c) was rejected** even though it is more accurate. It only works when Crossref
answered — and the TDM chain runs precisely when Crossref did *not*, so the
routing information would be missing exactly when it is needed. It would also
make the publisher contacted depend on a prior network response, which is harder
to reason about and to test than a constant.

**(a) was rejected** because the cost falls on the publisher and on the user's
privacy, and neither of them is the one who chose it.

## Consequences

**Positive.**

- An enabled TDM source is only ever told about DOIs its publisher registered.
- A wrong-publisher DOI is reported as `WrongPublisher`, distinct from
  `Disabled`. The user is not told to go find an API key that would not have
  helped — which is the failure the #438 attempt trace exists to prevent.
- Enabling all three costs at most one request per DOI, not three.

**Negative / accepted.**

- The lists are conservative and a publisher may own prefixes not in them, so
  some DOIs will not reach a source that could have served them. **A miss is not
  silent**: it appears in the attempt trace as
  `not consulted (DOI prefix 10.xxxx is not <publisher>)`, so it is diagnosable
  from the error message rather than looking like a lookup failure. That is the
  deliberate trade — a visible false negative over an invisible disclosure.
- The lists are static data that can drift as publishers acquire imprints.

**Revisit** if a user reports a missing prefix (the trace names it, so the report
will be precise), or if enough accumulate that a runtime override earns its keep.
