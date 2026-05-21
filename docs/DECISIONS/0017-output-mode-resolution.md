# 0017 - Output mode resolution (flag > env > implicit > TTY > quiet)

- **Date:** 2026-05-05
- **Status:** Accepted — fully implemented (#144, 0.2.1-beta.7). The resolution ladder (`--mode` > `--json`/`--quiet` > `DOIGET_MODE` > subcommand-implicit > TTY) is wired through `crates/doiget-cli/src/commands/output.rs::resolve` and threaded into every command's `run(..)`; the load-bearing MCP / stdout-purity invariant (Slice 9 + Slice 1) remains in force, now backed by the `serve → forced Mcp` override in `commands::main::run_dispatch`. Per-mode command behaviour (Quiet stdout suppression, Json bodies for human-table commands, ERRORS.md §3 batch JSONL) is staged as follow-up issues #203 / #204 / #205. Amendment 1 (2026-05-21) distinguishes **explicit** vs **implicit** Quiet and adds an artifact-command classification so commands whose output is the product (`bib`/`csl`/`capabilities`/`audit-log --verify --mode json`) honor only explicit Quiet, fixing the LLM cold-boot deadlock reported in #219 / #220.
- **Supersedes:** -
- **Source:** Discussion #14

## Context

This is a stub ADR. The full Context, Decision, and Consequences sections will be
expanded during Phase 0 from the corresponding GitHub Discussion review threads.
The summary below is sufficient for ADR INDEX.md and for cross-references in the
NORMATIVE specs to resolve.

## Decision

Output mode resolution order: --mode CLI flag > DOIGET_MODE env > subcommand-implicit (e.g., serve forces mcp) > TTY detection > quiet (default). MCP mode strictly forbids stdout writes outside JSON-RPC frames; tracing-subscriber writer is redirected to stderr globally.

## Consequences

See the source Discussion for the analyzed positives and negatives. The binding
result is captured in the Decision above and is referenced from the relevant
NORMATIVE doc(s) under docs/.

To revise this decision, write a new ADR with Status: Accepted and Supersedes: 0017,
and update this file's Status to Superseded by NNNN per CONTRIBUTING.md.

## Amendment — 2026-05-21 (1): bifurcate Quiet into explicit vs implicit; classify commands as artifact vs informational

**Diagnosis (LLM cold-boot deadlock, #219 / #220).** ADR-0017 collapses
two distinct signals into a single `OutputMode::Quiet`:

1. **Explicit Quiet** — the user *asks* for silence:
   `--quiet`, `-q`, `--mode quiet`, `DOIGET_MODE=quiet`.
2. **Implicit Quiet** — the resolver *infers* silence from a display
   heuristic: stdout is not a TTY and no other signal was given.
   The intent here is "don't dump ANSI into a pipe," not "be silent."

These two signals collapse to the same enum value but have different
authorities. Conflating them silently breaks artifact-output commands
in agent / pipe contexts:

```text
$ doiget capabilities | jq .    # non-TTY -> implicit Quiet
                                # capabilities suppresses stdout
                                # exit code 0, empty output
                                # LLM cannot discover anything.
```

The chicken-and-egg: the very command an LLM would call to discover
how to make `doiget` talk (`capabilities`) is the one silenced by the
TTY heuristic. `bib` / `csl` exhibit the same class of failure — both
are artifact-producing commands and both go silent when piped.

**Decision.** Distinguish explicit Quiet from implicit Quiet at the
resolution boundary, and classify every command as either *artifact*
(output IS the product) or *informational* (output is a status report,
silenceable on display heuristic). The classification dictates which
Quiet a command honors:

```text
                       | informational   | artifact
                       |  cmd  silenced? |  cmd  silenced?
-----------------------+-----------------+------------------
explicit  Quiet        |       yes       |       yes
  (--quiet / -q /      |                 |
   DOIGET_MODE=quiet / |                 |
   --mode quiet)       |                 |
-----------------------+-----------------+------------------
implicit  Quiet        |       yes       |       NO
  (non-TTY default)    |                 |
```

### Implementation shape (illustrative; binding contract is the table above)

1. **Resolver surface.** Promote the return type of
   `commands::output::resolve(..)` from `OutputMode` to a small
   `ResolvedOutput` value carrying the `mode` plus an
   explicit-quiet bit:

   ```rust
   pub struct ResolvedOutput {
       pub mode: OutputMode,           // unchanged (4 variants)
       pub quiet_was_explicit: bool,   // true iff user asked for it
   }
   ```

   The wire surface (`DOIGET_MODE` string values, the `modes` array
   in `capabilities` JSON, the `--mode` clap values) is **unchanged**;
   the bit lives only in the in-memory resolved value.

2. **Per-command honoring.** Each command receives `ResolvedOutput`
   instead of bare `OutputMode`:

   ```rust
   // Informational commands (audit-log Human, list-recent, search,
   // info, config show/path, provenance migrate, fetch/batch status):
   if out.mode == OutputMode::Quiet { return suppress(); }

   // Artifact commands (bib, csl, capabilities,
   // audit-log --verify --mode json):
   if out.mode == OutputMode::Quiet && out.quiet_was_explicit {
       return suppress();
   }
   ```

3. **Classification table.** A single source of truth in
   `commands::output` (e.g. a `is_artifact_command(name: &str) -> bool`
   helper) lists artifact commands; the lib-level parity test
   asserting every `Cli` enum variant has a `SubcommandMeta` entry
   (#214) is extended to also assert the classification matches the
   command's documented behavior.

4. **`audit-log --verify`** is the boundary case: **informational in
   Human mode, artifact in Json mode**. The split is natural since
   the `--mode json` request is itself an explicit signal; the
   command checks the mode it resolved into rather than its name.

### Binding properties

1. **`OutputMode` enum shape is unchanged** — 4 variants, same serde,
   same `capabilities` `modes` array. No wire-format break.
2. **`DOIGET_MODE=quiet` semantics are unchanged** — still suppresses
   *every* command including artifact ones (it is explicit).
3. **TTY detection semantics are unchanged** — still produces
   `OutputMode::Quiet` for non-TTY when no other signal is given,
   but now carries `quiet_was_explicit = false`.
4. **`serve → forced Mcp`** override (Slice 9 / Slice 1 stdout-purity
   invariant) is unchanged and orthogonal to this bit.
5. **`--mode quiet` is explicit** — passing the flag, even though it
   is the *same mode* the TTY heuristic might pick, signals intent
   and silences artifact commands.

### Test plan

- Unit (`output.rs`): `resolve(..)` returns `quiet_was_explicit = true`
  for each of `{--quiet, -q, --mode quiet, DOIGET_MODE=quiet}` and
  `false` for non-TTY default.
- E2E (`capabilities_e2e.rs`): `doiget capabilities` (non-TTY, no
  flags) emits the full JSON inventory — closes #219. The existing
  `--quiet capabilities` test (#203 follow-up) keeps the empty-stdout
  assertion to lock the explicit-quiet path.
- E2E (`output_mode_e2e.rs`): `bib` / `csl` emit their artifact on
  non-TTY default; `bib --quiet` / `csl --quiet` emit empty.
- E2E (audit-log boundary): `audit-log --verify --mode json` emits
  the JSON report regardless of TTY; `audit-log --verify` (Human)
  is still silenced on non-TTY.

### Migration

The implementation slice is a CLI-internal refactor; consumers
(scripts, agents, `capabilities` JSON consumers) see a behavior
*restoration* for artifact commands in pipes — no API change.
The 0.3.0 non-TTY-default callout in CHANGELOG remains in force
for informational commands.
