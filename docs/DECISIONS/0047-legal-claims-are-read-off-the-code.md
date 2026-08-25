# 0047 - LEGAL.md's claims are read off the code, and an "enforced control" must name enforcement that exists

- **Date:** 2026-08-26
- **Status:** Accepted
- **Supersedes:** -
- **Complements:** [0046](0046-vendor-claims-are-normative-links-are-pointers.md) — same audit, same class of defect, applied to claims about *doiget* rather than about vendors
- **Source:** #494 (network surface under-declared, `tdm-ieee` absent), #509 (a documented credentials file nothing reads)

## Context

`docs/LEGAL.md` is `Status: NORMATIVE` and says of itself that "users,
contributors, publisher legal teams, and future reviewers" should rely on it.
The 2026-08-25 audit and the correction pass that followed found four false
statements in it. They are all the same shape: someone wrote down what the
system was believed to do, and nothing brought the sentence back to the code.

**The shipped binary's network surface.** §2 said default binaries "include only
Open Access sources (Crossref, Unpaywall, arXiv)". Reading the production client
assembly in `crates/doiget-cli/src/commands/fetch.rs`, a default `oa-only` build
registers four allowlists: `tier_1_allowlist`, `oa_publisher_allowlist` (~20
publisher and repository patterns), `discovery_allowlist` (`api.openalex.org`,
ADR-0031, always-on) and `fulltext_allowlist` (`ar5iv.labs.arxiv.org`,
ADR-0032, always-on). **OpenAlex was absent from the list entirely** — a
third-party service, not an arXiv subdomain, contacted by the shipped binary with
no opt-in. `SOURCES.md` had documented both as always-on Tier 1 the whole time.

**`tdm-ieee`.** §2 and §6a.3 both enumerated three TDM features. There are four;
`tdm-ieee` landed with ADR-0042. `SOURCES.md` listed all four.

**A credentials file nothing reads.** §6a.1 said credentials come from env vars
"or `~/.config/doiget/credentials.toml`". The TDM resolver reads `std::env::var`
and nothing else, and no code path opens that file — while `CONFIG.md` §6
specifies it in full, including a 0600 permission warning that also does not
exist (#509).

**An enforcement clause naming nothing.** §6a.1 cited "*CI grep for embedded key
patterns*". There is no secret scanner in the tree; `posture-lint.yml` greps for
marketing terms, HTTP-server imports, telemetry imports and non-rustls TLS
backends. §6a is *defined* as "mechanically enforced by code, type system, Cargo,
or CI" — as distinct from §6b, the policy commitments a contributor could weaken
without machine-checkable resistance. An item in §6a whose enforcement clause
names nothing is in the wrong section, and that split is the whole point of
having two.

## Decision

**D1 — The network-surface claim is derived from the allowlist assembly, not
written from memory.** §2 now carries a table of the four allowlists the
production client is built from and names the code they come from, so a reader —
or a publisher's counsel — can check the claim against one function rather than
trusting a remembered summary.

This is why the claim rotted: `discovery_allowlist` and `fulltext_allowlist` were
added by ADR-0031 and ADR-0032, both correctly documented in `SOURCES.md`, and
nothing connected either to the sentence in LEGAL.md that enumerated hosts.

**D2 — Every item in §6a must name enforcement that exists.** An item whose
*Enforced by:* clause cites a mechanism that is absent either gets the mechanism
built or moves to §6b. Citing an imaginary control is worse than admitting a
policy commitment, because §6a is the half a reader is invited to verify.

The §6a.1 clause is corrected to the two controls that are real (`secrecy::Secret`
types, the `tracing` redactor). Building the secret scanner is a reasonable
follow-up; asserting one exists is not.

**D3 — A claim that turns out to describe an unimplemented feature is corrected
immediately, and the gap is filed separately.** The `credentials.toml` sentence
is removed from LEGAL.md now, and whether to implement `CONFIG.md` §6 is #509.
Leaving a known-false statement standing in the legal document while an issue is
open is what #472 was about.

`CONFIG.md` §6 is deliberately left as it is: editing it *is* the decision, and
that decision is not this ADR's to make.

## Consequences

**Positive.** The network-surface claim is falsifiable against one function.
§6a means what it says. A reader chasing an enforcement basis lands on something
that exists.

**Negative.** The §2 table duplicates knowledge held in `fetch.rs`, and duplicated
knowledge drifts — which is exactly how this ADR came to be needed. Naming the
function is a mitigation, not a fix. A CI check that diffs the documented host set
against the allowlists would be the real answer; it does not exist, and per D2
this ADR does not claim it does.

**Negative.** §6a.1 is now weaker on paper, having lost an enforcement clause. It
was always this weak; the clause was decoration.

## Alternatives rejected

- **Fold this into ADR-0046.** That ADR's subject is the vendor-claim / pointer
  distinction in `SOURCES.md`, a distinct idea. Broadening it to "all normative
  claims" would blunt the one thing it says sharply.
- **Leave the `credentials.toml` sentence until #509 is decided.** Ships a false
  statement in the legal document for the duration.
- **Move safeguard 1 to §6b** rather than correct its clause. The type-level
  controls are real and machine-enforced; only the clause was wrong.
