# 0049 - An unparsable ref is misuse (exit 2), and one function decides it

- **Date:** 2026-08-26
- **Status:** Accepted
- **Supersedes:** -
- **Source:** #492 (`fetch` exits 1, `graph` exits 2, `ERRORS.md` §4 says 2), found while doing #477

## Context

`docs/ERRORS.md` §4 is `Status: NORMATIVE` and reads:

| Exit | Meaning |
|---|---|
| `1` | At least one fetch failed. |
| `2` | Misuse (bad arguments, missing config). |

Given `doiget <cmd> "not a doi"`, one binary answered two ways:

- `commands/graph.rs` exited **2**, with a comment citing §4 and asserting it
  was *"consistent with `fetch`'s INVALID_REF path"*.
- `commands/fetch.rs` exited **1**, because `cli_exit_code` has no
  `InvalidRef` arm and the value fell through `_ => 1`.
- The eight commands #477 converted (`info`, `link`, `cite`, `text`, `tag`,
  `bib`, `csl`, `source`) exited 1 with `fetch`, by design — that conversion
  changed no exit code.

So `graph`'s comment was false, and `fetch`'s 1 was not a decision: it was the
catch-all. Against the table, nothing was fetched, so "at least one fetch
failed" does not describe the run.

`crates/doiget-cli/tests/fetch_error_mapping_e2e.rs` pinned it:

```rust
fn fetch_invalid_ref_emits_cargo_style_error_and_exit_1() { … .code(1) … }
```

#492 declined to change this in passing, and was right to: someone wrote that
name and that assertion deliberately for #119, so either it encodes a decision
§4 does not capture, or §4 is right and the test froze the catch-all. The two
readings imply opposite changes.

Reading #119 settles it. Its subject is the `error[INVALID_REF]:` *line* —
replacing an opaque anyhow dump with the cargo-style message. The exit code
came along as whatever `cli_exit_code` already returned. There is no argument
in it for 1 over 2.

## Decision

**D1 — `ErrorCode::InvalidRef` maps to exit 2.** An unparsable ref is a bad
argument. §4's 1 is reserved for a run in which a fetch was attempted and
failed.

**D2 — One function decides, and the call sites stop choosing.**
`fetch::cli_exit_code` is the single mapping; `graph` no longer hard-codes its
own 2. #477 unified the *message* through `render_ref_parse_error` and left the
exit code per-call-site, which is precisely where the disagreement lived.

**D3 — The rule is asserted across every ref-taking command, not just one.**
`every_ref_taking_command_exits_2_for_an_invalid_ref` runs the same table as
the message test, so a new subcommand is covered by both or by neither. A
single-command assertion is what allowed this to persist: `fetch`'s exit code
was pinned, and nothing compared it to its siblings.

**D4 — §4 says which class an unparsable *value* is in.** The table listed
"bad arguments" without settling whether that meant argv shape (what `clap`
rejects) or a value that fails validation. It now says both are misuse, so the
next reader does not have to re-derive it from two disagreeing call sites.

## Consequences

**Breaking for scripts keying on 1.** A caller that branched on `$? == 1` to
mean "invalid ref" now sees 2. That is the point — 1 could equally have meant a
network failure, a `NO_OA_AVAILABLE`, or any other code under the catch-all, so
the previous behaviour did not distinguish what such a script believed it did.
Recorded in `CHANGELOG.md` under **Changed**. Not in `docs/MIGRATION.md`,
which covers BiblioFetch.jl and machine moves rather than CLI contract
changes.

**The rejected alternative.** #492's option 2 — keep `fetch` at 1 and move
`graph` to 1, adding a sentence to §4 saying "misuse" means argv shape only —
has the smaller blast radius and was not chosen. It would make `INVALID_REF`
the one closed error code whose exit value is the *unclassified* one, and it
would leave `Ambiguous` (already 2, for a value that fails to select one
entity) sitting next to `InvalidRef` at 1, for a value that fails to parse.

**No new code.** The mapping already existed and already had a home; this adds
one arm and deletes one hard-coded literal.
