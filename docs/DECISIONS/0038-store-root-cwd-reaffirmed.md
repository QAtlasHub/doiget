# 0038 - Default store root stays `./papers` (cwd); 0036 reaffirmed against #406

- **Date:** 2026-08-22
- **Status:** Accepted
- **Supersedes:** - (reaffirms [0036](0036-default-store-cwd.md) unchanged)
- **Source:** #406, dogfood 2026-08-22

## Context

[ADR-0036](0036-default-store-cwd.md) made the default store root `./papers`
under the current working directory, so that fetched artifacts are visible where
an agent or human is actually working. Its Consequences section already recorded
the cost:

> A `papers/` directory now appears in whatever directory doiget is first run
> from. This is intentional (visibility) …

#406 reported that exact symptom from a real session, plus a sharper one: a
*denied* fetch, run inside an unrelated git worktree, left
`<worktree>/papers/.metadata/…` behind. The issue proposed defaulting to an XDG
data directory instead.

Investigating it turned up two things that were **not** ADR-0036 working as
designed, and one that was:

1. `doiget_health` — annotated `read_only_hint = true` — answered "is the store
   writable?" by calling `create_dir_all` on it. A *health check* created the
   tree, in a directory that is indeterminate for a daemon. Fixed in 0.8.7.
2. Our own test suite leaked `crates/doiget-mcp/papers/` into the repo, and
   `papers/` was not in `.gitignore`. Fixed in 0.8.7.
3. The metadata-only record written when the PDF leg is denied. That is
   [#145](https://github.com/QAtlasHub/doiget/issues/145) working as intended:
   the metadata *did* resolve, it is useful, and the CLI names its path.

## Decision

**Keep the cwd default. ADR-0036 stands unamended.**

The evidence in #406 is real but it is evidence for (1) and (2), which are now
fixed without moving the default. What remains — `papers/` appearing where you
run doiget — is the accepted consequence 0036 wrote down in advance, and it is
the property that closed the #344 agent-invisibility failure mode.

Reversing it would re-open #344: artifacts landing in `~/.local/share` where
"neither the agent nor the human sees it". The two dogfooding reports point in
opposite directions, and 0036's is the one about the tool's primary user.

Users who want a central library set `DOIGET_STORE_ROOT` or `[store] root`; 0.8.7
makes `doiget config doctor` print the resolved path and say that it is
cwd-relative, so the default is now self-describing rather than surprising.

## Consequences

**Positive.**

- No breaking change to where 0.8.x writes.
- The two genuine defects are fixed; the remaining behaviour is documented and
  discoverable from `config doctor`.

**Negative / accepted.**

- Unchanged from 0036: a user who fetches from many directories accrues several
  small stores unless they set `DOIGET_STORE_ROOT`.
- A denied fetch still writes a metadata-only record. Anyone who wants that
  changed should reopen it against #145's reasoning, not this ADR.

**If this is revisited**, the superseding ADR needs an answer to #344 that does
not rely on `--link`, since that was already available when #344 was filed.
