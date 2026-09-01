# 0055 - A failure says what to do about it, in three states

- **Date:** 2026-08-30
- **Status:** Accepted
- **Supersedes:** -
- **Complements:** [0014](0014-docs-class-system.md) — the ADR `docs/ERRORS.md` §2/§3 changes require; [0023](0023-denial-context-structured.md) and [0043](0043-machine-readable-diagnostics.md), which built the *content* of a fix but only on the success envelope
- **Source:** #506 (the MCP envelope says what happened but not what to do next)

## Context

`docs/ERRORS.md` §2 has carried per-code retry guidance since Phase 0, and it is
good guidance: `INVALID_REF` → *"No (user must correct input)"*, `NOT_IMPLEMENTED`
→ *"do not retry"*. This is not missing thinking. It is missing **plumbing**:

```
grep -rniE 'retryable|do_not_retry|permanent|transient' crates/doiget-mcp  → 0
grep -rniE 'retry' crates/doiget-mcp/src/*.rs (tool descriptions)          → 0
```

The failure envelope was `{ok:false, error:{code, message, denial_context?}}`, so
an agent's only signal was **the name of the code** — and several names point the
wrong way. `NO_OA_AVAILABLE` is the most common failure there is, and both its
name and its ERRORS.md row (*"Try later, or enable opt-in source"*) read to a
machine as *wait*, when it is nearly always *configure*. That invites an
unbounded retry loop over something that will not change on its own.

## Decision

**1. `error.disposition`, with three states.**

| value | meaning |
|---|---|
| `terminal` | the answer will not change. Do not retry, do not wait. |
| `retry_after` | it may change on its own. Retry with backoff. |
| `needs_config` | it will not change by itself, but a named change makes it. Surface it; do not loop. |

Two states cannot express the third, and the third is the one that matters most
here. Facing it an agent should neither loop nor give up silently.

`terminal` also covers failures a caller can act on by issuing a *different*
request — `INVALID_REF`, `AMBIGUOUS`, `TEXT_UNAVAILABLE`. **This** call is
settled, which is what a disposition is about.

`STORE_ERROR` and `LOG_ERROR` are `needs_config`. A machine cannot name the fix
for a full disk, but it must not loop on one either, and "surface this to a
human" is exactly what the state means.

**2. One source of truth.** `ErrorCode::disposition()` is an exhaustive `match`
with no wildcard, so a new code must decide. `docs/ERRORS.md` §2 gains a
Disposition column, and `errors_md_disposition_column_matches_the_code` parses
the shipped document and asserts every row against the function. #506 asked for
the table to be generated from the code or asserted against it, because
otherwise the doc and the wire drift — the #493 pattern. Generating it would
have cost the per-code prose, which is the useful part; asserting keeps both.

The test also guards its own parser: it asserts it saw exactly 15 rows, because
a parser that silently matches nothing passes every time.

**3. One builder.** Every failure envelope goes through `error_object`. A field
present on some failures and absent on others is worse than no field: it teaches
the reader to fall back to guessing from the code's name, which is the habit
this exists to replace.

**4. Stated where an agent that never read ERRORS.md still meets it** — the MCP
server `instructions`, delivered on `initialize` to every client. One place
rather than twenty-two tool descriptions, and it names the specific trap:
`NO_OA_AVAILABLE` is `needs_config`, not something to wait out.

## Consequences

- The wire gains a field. Additive: `{ok:false}` consumers that ignore it are
  unaffected, and `code` / `message` / `denial_context` are unchanged.
- `docs/ERRORS.md` §2 gains a column and §3's envelope shape gains the field.
  That is the NORMATIVE change ADR-0014 requires this ADR for. No code's
  *meaning* or recoverability guidance changed — only that the guidance is now
  machine-readable.
- A new `ErrorCode` will not compile until its disposition is decided, and will
  not pass tests until `docs/ERRORS.md` records the same answer.

## Not in scope

- **`error.retry_after_ms`.** #506 asks for it and it is not here. `Retry-After`
  is parsed today (`http::parse_retry_after`) but consumed **inside** the retry
  loop and discarded; by the time an error surfaces, the retries are exhausted
  and no honest number remains. Emitting a plausible default would be a number
  the caller could not distinguish from a measured one. Plumbing the header out
  of the loop is its own change.
- **`remediation` on `ok:false`.** ADR-0043's channel keys on `DenialContext`,
  which is present on only some failures; carrying it for those alone would
  reproduce the sometimes-present problem this ADR rejects for `disposition`.
- **`rate_limit_budget` on live responses.** Independent of the retry contract.

#506 stays open for those three.
