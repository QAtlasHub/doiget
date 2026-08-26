# 0050 - credentials.toml carries the key; the agreement stays in the environment

- **Date:** 2026-08-26
- **Status:** Accepted
- **Supersedes:** -
- **Complements:** [0047](0047-legal-claims-are-read-off-the-code.md) — same audit; that ADR corrected the claim, this one closes the gap the claim was about
- **Source:** #509 (a credentials file documented in full and read by nothing)

## Context

`docs/CONFIG.md` is `Status: NORMATIVE`. Its §6 gave a complete schema for
`~/.config/doiget/credentials.toml` — four `[tdm.<publisher>]` tables, an
`api_key` and an `agreed` per table, a precedence rule against the environment,
and a `0600` permission warning "at startup".

Nothing read the file. The TDM resolver in `crates/doiget-core/src/lib.rs`
called `std::env::var` and stopped there, `credentials.toml` appeared in exactly
one non-prose place (a doc comment about the config *directory*), and the
permission warning did not exist either — the only `permissions()` calls in the
tree were a store write and a writability probe.

The cost is specific. Follow the NORMATIVE document, write an Elsevier key into
that file with `agreed = true`, and doiget silently ignores it and reports the
source unavailable for want of a key. `config doctor` could not help: it points
at "docs/CONFIG.md §6", the section describing the file that does nothing.

Same shape as #476, #441, #442, #454 and #458 — a documented mechanism with no
implementation behind it. This one landed on credentials, in a NORMATIVE doc,
with a security claim attached.

#509 offered two ways out, and they are not equivalent: implement it, or delete
the documentation.

## Decision

**D1 — Implement the file, for the key.** A long-lived API key is genuinely
better off in a file than in the environment: it survives a shell restart, it is
not visible in `ps`, and it does not leak into the environment of every
subprocess. `crates/doiget-core/src/credentials.rs` reads
`[tdm.<publisher>] api_key`, and `DOIGET_KEY_<PUBLISHER>` sits one rung above it
— the same env-then-file order `store_root` (#441) and `contact_email` (#504)
use, and the order `CONFIG.md` §1 has always claimed.

**D2 — The agreement does not follow the key into the file.**
`DOIGET_AGREE_TDM_<PUBLISHER>=1` stays environment-only.

`LEGAL.md` §6a.2 lists the per-publisher agreement as an *enforced control*, and
part of what makes it meaningful is that it is an act taken in the session that
runs the fetch. A boolean written once into a config file and forgotten is a
weaker consent for the same word. #509 flagged this and asked for it to be an
explicit decision rather than a side effect; this is that decision.

The rules in `CAPABILITY.md` §2 are unchanged by the new rung, which is the
point: a key from the file still needs the agreement, so `KeyButNotAgreed` now
also fires for a file-supplied key with no agreement variable.

**D3 — An `agreed` key in the file is reported, not discarded.** It is parsed
solely so doiget can warn that it grants nothing and name the variable that
does. Silently ignoring a field this document specified is the defect being
closed; doing it again in the fix would be worse than not fixing it.

**D4 — The `0600` warning exists, and is POSIX-only.** Group- or
world-accessible modes emit a `tracing::warn!` naming the mode and the `chmod`.
Off POSIX it is a no-op: Windows ACLs are not a mode, and advice phrased in
`chmod` terms would be advice the user cannot follow. `CONFIG.md` §6 says
"SHOULD ... doiget warns" rather than "MUST", because a warning is what is
enforced.

## Consequences

**Positive.**

- The NORMATIVE document is true again. `LEGAL.md` §6a.1 can state the file as a
  credential source without #494's correction attached.
- The security claim is real: the permission check is code, not a sentence.
- The opt-in is not diluted. §6a.2 still names an act, not a stored boolean.

**Negative.**

- A new file-parsing surface in the credential path. Bounded: absent,
  unreadable and malformed all yield "no keys" with a `warn`, so one bad line
  cannot take a fetch down, and there is no accessor for `agreed` at all — the
  type cannot carry it, so no future caller can be tempted to consult one.
- Configuration is now in two files. `config doctor` reports which rung answered
  for the addresses (#504); it does not yet do so for keys, and deliberately
  will not print a key.

**Testing note, learned here.** Adding a file rung made the existing
`CapabilityProfile::from_env` tests depend on the developer's real
`~/.config/doiget/credentials.toml` — green in CI and red on the one machine
that has TDM configured. They now run under an isolated config home. A rung that
reads a file outside the test's control is not a detail of the test harness; it
is a property of the change.
