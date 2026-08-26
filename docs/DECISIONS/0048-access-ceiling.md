# 0048 - The access ceiling is written down, and widening it amends the written form

- **Date:** 2026-08-26
- **Status:** Accepted
- **Supersedes:** -
- **Source:** #497 (the operative invariant is not the believed one), #498 (the 2026-08-25 vendor-terms audit)

## Context

`docs/LEGAL.md` §1 and §2 state what doiget does **not** do: no access-control
circumvention, no proxying across user boundaries, no redistribution. All still
true, all still verifiable.

What was missing was the positive statement. Given a ref, *what is doiget
permitted to try, and where does it stop?*

The rule the project operated under informally was **"never go beyond what
Unpaywall reports"**. Clean, checkable, and no longer what the code does. It rose
twice:

- **ADR-0044** — when the OA content leg is blocked, an enabled Tier-3 source is
  asked for the bytes. For `tdm-aps` that returns a PDF from a location Unpaywall
  never listed.
- **#445** — when the OA chain and the arXiv preprint fallback are exhausted, the
  enabled optional sources are asked whether anyone else holds a copy, and the URL
  they report is tried. Also not from Unpaywall.

Both are opt-in, both have ADRs, both stay inside publisher-sanctioned APIs and
inside the redirect allowlist, and both record their outcome in the attempt trace.
**Neither is improper.** The defect is that the rule everyone believed was being
enforced was not the rule being enforced, and the gap had never been written down —
so ADR-0044 was reviewed against a ceiling that existed only as a shared belief.

An unstated invariant cannot be checked. That is how both widenings happened
without anyone deciding the ceiling had moved.

## Decision

**D1 — `LEGAL.md` gains §2a, stating the ceiling positively.** Two kinds of
location, and no third:

- **(a) a location an enabled source reported** — the URL appears verbatim in a
  response from a source the build *and* the runtime profile enabled;
- **(b) an endpoint built from a vendor's own documented URL scheme** for the
  identifier the user supplied.

Bounded on every request and every redirect hop by the host allowlist, by
credential-plus-consent for Tier 3, by prefix scoping for Tier 3, and by the
attempt trace.

**D2 — Every clause names where it is enforced, and is falsifiable by reading
that code.** §2a cites `optional_source_oa_url`'s dispatch, the `*_url`
constructors, the `redirect::Policy::custom` closure, `PUBLISHER_PREFIXES`, and
`SourceAttempt`. A reader who thinks a clause is false can go and check it in one
place.

**#497 proposed a candidate wording that this ADR rejects**, and the reason is
instructive. It suggested doiget "never constructs a content URL that no enabled
source reported". **That is false.** arXiv's `/pdf/<id>.pdf`, ar5iv's `/html/<id>`
and every Tier-3 TDM endpoint are constructed from an identifier, not reported by
anyone. Writing that sentence would have replaced an unstated invariant with a
stated false one — worse, because it reads as verified.

The issue anticipated this: *"That is a guess at the shape, not a proposal — the
exact wording should be derived by reading `orchestrator.rs` and the allowlist."*
It was, and the shape came out different.

The distinction that matters is not *reported vs constructed*. It is **documented
by the vendor vs guessed by us**. Constructing `/pdf/<id>.pdf` against arXiv's
published scheme is exactly as sanctioned as following a URL Unpaywall handed
over; inferring a publisher's URL pattern from observation would not be, and
§2a's "no third kind" is what forbids it.

**D3 — Widening (a) or (b) amends §2a in the same PR.** Not "should" — the ceiling
is only useful as something a reviewer can hold a diff against.

## Consequences

**Positive.** The argument for the current posture is written and true. It is no
longer "we never exceed Unpaywall" — which is false — but "every leg is a
publisher-sanctioned API or an index the user switched on, at an allowlisted host,
under the user's own credential where one is required, with the attempt recorded".
That is defensible, and each conjunct is checkable.

**Negative.** §2a is prose asserting properties of code, and nothing mechanically
ties the two. A change could widen (b) and leave §2a stale exactly as ADR-0044 did.
D3 is a review discipline, not a control — the honest classification, and the same
distinction `LEGAL.md` §6a/§6b draws between enforced and committed. Making it
enforceable would need something like a lint over the `*_url` constructors; that
does not exist and this ADR does not pretend it does.

**Negative.** Naming code locations in a NORMATIVE document means a refactor that
renames `optional_source_oa_url` makes §2a wrong. Accepted deliberately: a clause
citing nothing is unfalsifiable, which is the failure this ADR exists to fix, and
ADR-0047 D2 made the same trade for §6a.

## Alternatives rejected

- **Restore the Unpaywall-only ceiling** by reverting ADR-0044 and #445. Both are
  opt-in, sanctioned and useful; the problem was never the widening, only that it
  was undeclared.
- **Adopt #497's proposed wording.** False, as above.
- **Leave the invariant implicit** and rely on ADR review. That was the default for
  two releases, and it produced this issue.
