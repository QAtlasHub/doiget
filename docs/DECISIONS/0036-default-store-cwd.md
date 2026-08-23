# 0036 - Default store root is `./papers` (current working directory)

- **Date:** 2026-06-24
- **Status:** Accepted (implemented by `feat/344-default-store-cwd`; promoted in 0.8.0, #352)
- **Supersedes:** -  (amends [0004](0004-bibliofetch-coexistence.md): the *co-location* default only — the shared on-disk *format* contract in [`STORE.md`](../STORE.md) is unchanged)
- **Source:** #344 problem 1 / dogfood 2026-06-24

## Context

ADR-0004 set the default store root to `~/papers` so that doiget and
BiblioFetch.jl co-locate their stores out of the box (a single shared
library under the home directory).

Dogfooding doiget as an LLM-agent tool surfaced a recurring failure mode
(#344, problem 1): an agent runs `doiget fetch …`, the PDF lands in
`~/papers` (`%USERPROFILE%\papers`), and **neither the agent nor the human
sees it** — the working directory where they are actually operating shows
nothing. The artifact is "successfully fetched" yet invisible at the place
work happens. `fetch --link` (ADR-0035) mitigates this by surfacing a link
into an explicit `--dir`, but the *default* still hides the primary store
far from the cwd, so the happy path stays surprising.

The root location is **not** part of the shared store contract: STORE.md
specifies the layout *under* a root (`<root>/<safekey>.pdf` +
`<root>/.metadata/…`), the safekey algorithm, the lock protocol, and the
atomic-write sequence. Where the root *is* has always been per-tool
configuration ([`CONFIG.md`](../CONFIG.md) §2). So the default can change
without touching the bytes-on-disk contract.

## Decision

The built-in default store root becomes **`./papers`** — `papers/`
directly under the current working directory — for both the CLI and the
MCP server (`resolve_store_root` in `doiget-cli/src/commands/mod.rs` and
`doiget-mcp/src/lib.rs`; `ResolvedConfig::from_env` now reuses the CLI
resolver, so `config show` / `doctor` never drift from the writer).

Resolution order is otherwise unchanged: `DOIGET_STORE_ROOT` env >
`--store-root` flag > config file > this default. A user who wants a
single central library sets `DOIGET_STORE_ROOT=~/papers` (or `store.root`
in `config.toml`), which also restores BiblioFetch.jl co-location.

### Amendment 1 (2026-08-23, #441) — the config rung now exists

The order above described an implementation that did not exist. Until #441
`resolve_store_root` read `DOIGET_STORE_ROOT` and fell straight through to
the cwd default; `[store] root` was parsed by nothing, while `docs/CONFIG.md`
§3 listed it, `doiget config init` wrote it into its template and
`doiget config doctor` recommended it. A user could follow every piece of
the tool's own advice and have the setting silently ignored — worst of all
when they tested from the directory they had configured, where the ignored
value and the cwd default coincide.

Two clarifications to the sentence above, now that it is implemented:

- **The flag and the env var share one rung.** `--store-root` is applied by
  writing `DOIGET_STORE_ROOT`, so the flag wins over an inherited env value
  (the usual CLI convention) rather than losing to it. The original wording
  put the env var first; no build ever behaved that way.
- **A leading `~` is expanded for the config value only.** The env var is
  expanded by the shell before doiget sees it; a config file has no shell,
  so `~/papers` written there would otherwise become a literal `~`
  directory.

`doiget config doctor` now prints which rung answered, so a setting that is
present but not honoured can no longer look like one that worked.

This **amends ADR-0004**: doiget and BiblioFetch.jl no longer co-locate
their stores *by default*. The shared on-disk *format* (STORE.md) — the
actual coexistence contract — is unchanged; pointing both tools at the
same root still yields a fully shared, round-trip-compatible store.

## Consequences

**Positive.**

- Fetched artifacts are visible where work happens; the agent failure mode
  in #344 is closed at the default, not only via `--link`.
- The default is self-explanatory: `ls papers/` in the project directory.
- No contract change to STORE.md; the safekey / lock / atomic-write specs
  and the cross-tool round-trip CI test are untouched.

**Negative / accepted.**

- **BiblioFetch.jl co-location breaks by default.** A user running both
  tools who relied on the implicit `~/papers` shared library must now set
  `DOIGET_STORE_ROOT=~/papers` explicitly. The maintainer accepts this
  (does not use BiblioFetch.jl); a tracking note is filed on the
  BiblioFetch side if warranted.
- A `papers/` directory now appears in whatever directory doiget is first
  run from. This is intentional (visibility) but means a user fetching
  from many directories accrues several small stores unless they set
  `DOIGET_STORE_ROOT`. Documented in CONFIG.md / STORE.md.
- Relative default: the store "moves" with the cwd. Acceptable because the
  override is a one-liner and the default optimizes for the common
  per-project agent workflow.

**Tests.** Integration / e2e tests already set `DOIGET_STORE_ROOT` to a
`tempfile::TempDir`, so they are unaffected by the default change; the
`config` unit test asserting the unset default now expects `./papers`.

To revise this decision, write a new ADR with `Supersedes: 0036`.
