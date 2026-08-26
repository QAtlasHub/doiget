# 0051 - Contributions carry a relicensable grant, recorded as a commit sign-off

- **Date:** 2026-08-25
- **Status:** Accepted
- **Supersedes:** -
- **Source:** - (maintainer decision; no source Discussion)

## Context

The `## License` section this ADR replaces said, in full, that contributions are licensed
under the MIT License. That is inbound = outbound, and it is a one-way door: under it,
changing doiget's outbound license later requires permission from every past
contributor, because each holds copyright in their own contribution and granted only
MIT. The expensive part is not the legal work. It is that some contributors become
unreachable, and one unreachable contributor pins the license permanently.

doiget is at the single moment when that cost is zero. `git shortlog -sne --all`:

```
370  sotashimozono <...@g.ecc.u-tokyo.ac.jp>
313  Souta <...@g.ecc.u-tokyo.ac.jp>
  2  Souta Shimozono <...@gmail.com>
 38  dependabot[bot]
  4  github-actions[bot]
```

One human, under three git identities. The two bots produce dependency bumps and release
metadata, and neither is a legal person who could hold or grant copyright. So the
maintainer holds all of it today, and the first external PR merged under inbound = MIT
ends that permanently for whatever it touches.

This is not a decision to relicense. doiget keeps shipping on crates.io under an
OSI-approved license; its distribution and its MCP-ecosystem reach both depend on that.
It is a decision to keep the *option*, which is free now and unpurchasable later.

The shapes the option keeps open are concrete: a copyleft core with separately licensed
Tier 2/3 connectors — the split ADR-0002 already draws at build-feature level — and an
embedding license for downstream products. Neither can be offered under MIT, because MIT
grants both for nothing.

## Decision

**D1. The inbound grant is broader than the outbound license.** Contributors grant a
perpetual, worldwide, irrevocable, sublicensable copyright license permitting
distribution *under any license terms*. The wording follows the Apache ICLA §2 grant;
the sublicense right is the part that carries relicensing.

**D2. A patent grant with defensive termination accompanies it**, in the Apache ICLA §3
shape: the license covers claims necessarily infringed by the contribution, and
terminates for any entity that files patent litigation over it.

**D3. The outbound promise is written down.** Public releases remain available under an
OSI-approved open source license. D1 without D3 reads as a reserved right to close the
source, and would suppress exactly the contributions the project wants. D3 costs nothing
the project intends to do anyway — AGPL is OSI-approved, so the copyleft-core shape
survives it. What D3 forecloses is going proprietary outright, which is already ruled
out.

**D4. Assent is a `Signed-off-by` trailer, checked in CI.** No CLA bot, no signature
database. The record lives in `git log`, so it is auditable from a clone alone and no
hosted service has to outlive the project. `.github/workflows/dco.yml` fails a PR whose
commits lack a well-formed trailer, exempting bot-authored commits and the merge commits
GitHub creates.

**D5. Not retroactive.** Everything merged before this ADR stays MIT-only, and
CONTRIBUTING.md says so. Given the shortlog above this forfeits nothing: the only
pre-existing non-maintainer commits are bot-authored dependency bumps.

## Consequences

- Future contributions can be dual-licensed or relicensed without a contributor hunt.
  The pre-0.8.10 tree stays MIT forever, and the crates already published cannot be
  recalled; this ADR does not pretend otherwise.
- `dco.yml` is added but is deliberately **not** promoted to a required status check —
  the required checks remain the two `test` jobs. It blocks within its own job, so a
  missing sign-off is visible on the PR without perturbing the auto-merge chain.
- Sign-off adds one flag (`git commit -s`) to the maintainer's own loop. Forgetting it
  turns a check red before merge rather than after, which is the cheap direction.
- Some contributors refuse broad CLAs on principle. That is the real cost, paid in a
  currency doiget has little of. If it ever becomes the binding constraint, D1 can be
  narrowed by a superseding ADR — narrowing is always available, widening is not.
- **Unreviewed by counsel.** The text is assembled from the Apache ICLA pattern, and
  CONTRIBUTING.md states plainly that it is not legal advice. Before the option is
  exercised commercially — an actual relicense, or an embedding sale — the grant needs a
  lawyer's read. Adopting now and reviewing later is the right order, because the record
  this creates is the one thing that cannot be built retroactively.

## Alternatives rejected

- **DCO alone** (Linux, Docker). The DCO certifies origin and right-to-submit; it grants
  nothing beyond the project's stated outbound license. It would produce the same
  sign-off trailers and none of the relicensing ability — all of the friction, none of
  the point.
- **Copyright assignment** (FSF style). Strictly stronger and strictly worse to ask for:
  it takes the contributor's copyright instead of licensing it. D1 obtains the rights
  that are actually needed while the contributor keeps ownership.
- **CLA Assistant or a comparable bot.** Adds a GitHub App with write access plus an
  external signature store to a repo whose posture rules exist to keep that surface
  small (ADR-0001, ADR-0015). A git trailer outlives any hosted signature service.
- **Wait until a real contributor appears.** The trigger arrives as a merged PR — a good
  one, from someone helpful, at the exact moment when demanding a licensing agreement is
  most awkward. The policy is cheapest strictly before it is needed.
- **Relicense now** (AGPL plus a commercial license). Premature: it trades MCP-ecosystem
  adoption for revenue that has no customer yet. This ADR keeps the door open without
  walking through it.

If this decision is ever revisited, write a new ADR with `Supersedes: 0051`, and update
this file's `Status:` per `CONTRIBUTING.md`.
