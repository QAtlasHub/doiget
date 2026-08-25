# 0043 - The machine-readable surfaces carry the trace and the remediation

- **Date:** 2026-08-24
- **Status:** Accepted
- **Supersedes:** - (extends [0023](0023-denial-context.md))
- **Source:** #459, and the half of #445 that #448 left

## Context

Three issues this cycle made a blocked fetch actionable: #443 added the `= help:` block
naming the registrable-domain widening, #448 attached the #438 resolution trace to
blocked outcomes, and #449 made the content leg fall through. All three landed as **CLI
text**. The MCP tool envelope and the `batch --json` record got none of it.

Driving the real MCP server against the DOI #407 was written about shows what that costs.
`doiget_fetch_paper` on `10.1109/TSP.2018.2812747` returns:

```
pdf: { status: "blocked",
       code: "NETWORK_ERROR",
       message: "redirect target strathprints.strath.ac.uk not in allowlist for source oa-publisher",
       denial_context: { reason: "redirect_not_in_allowlist",
                         attempted: "strathprints.strath.ac.uk",
                         expected: [ 24 host patterns ] } }
```

An agent sees a refusal, the host, and 24 patterns that are not it. There is no
`attempts`, and nothing says what to change.

The same call, after adding **one line** of config:

```
ok: true    pdf: { status: "fetched" }    size_bytes: 880081
```

`strathprints.strath.ac.uk` is a university repository, `trust_academic_repos` exists for
exactly that class, and `user_extension::academic_repo_hosts()` already knows the host.
The information needed to get from the first output to the second was in the codebase and
never reached the caller.

An agent that cannot recover from a recoverable failure reports "this paper is not
available". That is false, and it is false in a way the user cannot see.

## Decision

**Both surfaces carry both fields, additively, computed once in `doiget-core`.**

`docs/ERRORS.md` §3.2 (`remediation`) and §3.3 (`attempts`) are the wire contract.
Neither is required; §3.1's existing rule — consumers MUST tolerate presence and absence —
extends to them unchanged, so no current consumer breaks.

Four sub-decisions worth recording, because each had a cheaper alternative:

**Computed in core, not per surface.** `widening_suggestions` moves out of `doiget-cli`
into `doiget_core::remediation`, and the CLI's `= help:` block now renders the same list
the MCP envelope serialises. The cheaper option was to copy it. #454 is the recent lesson
about two surfaces each keeping their own copy of a rule: the Tier-3 allowlists had a
guard asserting the list was right and no caller handing it to anyone, for three releases.

**`kind` is a closed enum, not a pasteable string.** `additional_host` trusts one
publisher; `trust_flag` trusts a curated class (ADR-0028). Collapsing them into one
"here is what to paste" would hide a policy decision that an agent is making on the
user's behalf.

**`attempts.outcome` is a token, not the rendered sentence.** `AttemptOutcome::render`
is prose and has already been reworded twice (#413, #438); a consumer keying off it would
have broken both times. `wire()` is the stable half, `detail` carries the specifics —
which env var, which prefix — so a consumer never has to parse them back out of a
sentence.

**`attempts` is absent when there is no trace, never `[]`.** "We have no trace" and "the
trace ran and found nothing" are different, and #413 exists precisely because they used
to be the same observable. Emitting `[]` for the first would reintroduce that.

## Alternatives rejected

**(a) Leave it; the CLI has the information.** Rejected because MCP is not a lesser
surface. §3's persona table has always promised the agent a *structured* rendering of
what the human is told — this is that promise going unkept, not a feature request.

**(b) Fold the hints into `error.message`.** Cheapest, and it works for a human tailing
a log. It makes an agent parse English to find a config key, which is the failure mode
`denial_context` was introduced to end (ADR-0023). Doing it again one field over would be
strange.

**(c) Derive `Serialize` on `SourceAttempt` / `AttemptOutcome`.** Fewer lines. Both are
`#[non_exhaustive]` public API, so deriving makes every future variant a wire change by
default rather than by decision. `attempts_to_value` keeps the wire shape a thing someone
chose.

## Consequences

**Positive.**

- An agent can recover from an allowlist denial without a human, and can tell "we asked
  and it had nothing" from "we never asked" — the #413 distinction, finally available to
  the caller that most needs it.
- One implementation of the widening rule, so the CLI and MCP cannot drift.
- The `--mode json` half of #445 is closed, for all three surfaces rather than the one it
  asked about.

**Negative / accepted.**

- Two more closed enums to version. Both are small and both are already the kind of thing
  ADR-0023 versions.
- `remediation` is advisory. It suggests widening the trusted surface, and a caller that
  applies every suggestion without thought will trust more than it needed to. The `note`
  field exists to make the trade visible, and the most-specific-first ordering means the
  narrowest fix is read first. Not a substitute for the user's judgement, and
  `docs/SECURITY.md`'s posture on the allowlist is unchanged: doiget suggests, the user
  edits the file.
- The trust flag is offered only when a curated pattern already matches, so it can never
  invent a new class — but the curated lists are static data and will age.

**Revisit** if a third machine-readable surface appears, or if `remediation` grows a kind
that is not a config edit.
