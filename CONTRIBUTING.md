# Contributing to doiget

Thank you for your interest in contributing. doiget is a deliberately small-scope project,
and this document describes both **how** to contribute and **what changes are out of scope**.

## Local dev setup

doiget targets **Rust stable**. The active toolchain is pinned via
`rust-toolchain.toml` (`channel = "stable"`); the declared MSRV is **1.86**
(see [`docs/PUBLIC_API.md`](docs/PUBLIC_API.md) §7).

```sh
# 1. Install rustup if you don't have it.
#    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
#    Or:  winget install Rustlang.Rustup    (Windows)
#    Or:  brew install rustup                (macOS via Homebrew)

# 2. Clone and build.
git clone https://github.com/QAtlasHub/doiget.git
cd doiget
cargo build                                # default features = oa-only
cargo build --no-default-features          # sanity: minimal features

# 3. Run the local checks CI runs (in this order; fix as you go).
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --no-default-features --features oa-only -- -D warnings
cargo test  --workspace --all-targets --no-default-features --features oa-only

# 4. Optional but recommended: install the audit tooling once.
cargo install cargo-deny
cargo install cargo-audit
cargo deny  check --workspace --no-default-features --features oa-only
cargo audit
```

**Phase 0 expected behavior:** `cargo build` succeeds, `doiget --help` prints the
subcommand list, and any `doiget <subcommand>` returns a `Phase 0 stub` error.
`cargo test` runs the four smoke tests in
`crates/doiget-core/src/lib.rs::tests`.

### Per-developer settings

**Do not put them in the repository.** `.cargo/config.toml` is tracked and pins
the reproducibility settings ([`docs/SECURITY.md`](docs/SECURITY.md) §1.9); it is
not a place for your machine's preferences. There is no
`.cargo/config.local.toml` — cargo reads only `config.toml` from a `.cargo`
directory, and the `include = [...]` that would make a sibling file real is a
hard error before any cargo command runs when the file is missing, which it
would be in every fresh clone and on every CI runner (#521).

Two mechanisms work and need no change to this repository:

**`$CARGO_HOME/config.toml`** (`~/.cargo/config.toml`) — cargo merges it into
the config hierarchy automatically for every project. Per cargo's documented
precedence it sits *below* the repository's `.cargo/config.toml`, so the pinned
`[net]` / `[term]` / reproducibility values still win where the two overlap.
That is the right way round: your defaults apply to everything the repo does
not deliberately fix.

**`CARGO_TARGET_DIR`** (or `--target-dir`) for build output. Worth being
deliberate about:

```sh
# per-checkout, and it goes away with the checkout
cargo build                       # -> ./target

# shared across checkouts — pick a path you will actually look at
export CARGO_TARGET_DIR="$HOME/.cache/cargo-target/doiget"
```

A shared target directory grows without bound and nothing prunes it. On
2026-08-26 two of them — set ad hoc to fixed paths under `%TEMP%` — held
**115 GB and 81 GB** on one machine and took it to 95% full; deleting them
recovered 195 GB. If you share one, put it somewhere you will notice, and
`cargo clean --target-dir <path>` occasionally.

## Before you open a PR

1. Read [docs/SCOPE.md](docs/SCOPE.md) for the **Permanent non-goals** list. PRs that move
   doiget toward a non-goal will be closed without merge regardless of code quality.
2. Read the relevant ADR(s) in [docs/DECISIONS/](docs/DECISIONS/) for the area you're
   touching. Decisions are normative; deviation requires a new ADR.
3. Check that an issue or discussion exists. For non-trivial changes, please open a
   [Discussion](https://github.com/QAtlasHub/doiget/discussions) first.

## Scope-reopening meta-rule

A `Permanent non-goal` listed in [docs/SCOPE.md](docs/SCOPE.md) cannot be reversed by a PR
description, an issue comment, or an inline rationale. To re-open scope on an item, file a
new GitHub Discussion titled `[scope-reopening] <topic>` and obtain explicit maintainer
approval **before** writing any code. PRs that effectively reverse a non-goal without this
process will be closed.

## Doc rules

doiget's docs are split into **NORMATIVE** and **INFORMATIVE** classes (see
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the index). The header of each doc states
its class.

- **NORMATIVE** docs define binding contracts. Changes to NORMATIVE content require a
  matching ADR PR.
- **INFORMATIVE** docs explain context and rationale. Changes are merge-on-review.

When writing or editing docs:

- Use ATX headings (`#`, `##`, …) only — no Setext (`====`).
- Use **relative links only** for repo-internal references; reserve absolute URLs for
  external resources.
- Place images under `docs/_images/` and reference them with relative paths.
- Keep lines under ~100 chars where it does not harm readability.
- Localization: English is canonical for all NORMATIVE docs. A `docs/ja/` may exist as
  informative supplement (Phase 6+).

## Code rules

### Layout

The Cargo workspace structure is locked by ADR-0008 (see
[docs/DECISIONS/](docs/DECISIONS/)). New crates require an ADR.

### Style

- `cargo fmt` (rustfmt with default settings) is enforced by CI.
- `cargo clippy --workspace --all-targets -- -D warnings` is enforced by CI.
- `unsafe_code` is forbidden workspace-wide.
- `print_stdout` is denied in `doiget-mcp` (MCP stdio safety; see ADR-0001).
- Public items in `doiget-core` require doc comments (`#![warn(missing_docs)]`).

### Tests

A PR is expected to include tests for the code it touches. Test layers (see ADR-0008):

- **Unit:** `#[cfg(test)] mod tests` colocated with the code under test.
- **Integration:** `tests/*.rs` at the workspace root.
- **Golden:** `tests/fixtures/golden/*` with the expected serialized output (BibTeX, CSL,
  TOML normalization, MCP tool response shapes).
- **Property:** `proptest` for the `safekey` algorithm and TOML round-trip.

Coverage target: **80%+ on `doiget-core`** (measured via `cargo-llvm-cov`).

### Forbidden imports

The following crates are denied workspace-wide via `deny.toml`:

- HTTP server frameworks (`axum`, `actix-web`, `warp`, `tide`, `hyper` server) — MCP HTTP
  transport is a permanent non-goal (ADR-0001).
- Telemetry SDKs (`sentry`, `posthog`, `google_analytics`) — telemetry is a permanent
  non-goal (ADR-0015).
- Self-update crates (`self_update`, etc.) — self-update is a permanent non-goal.
- `openssl` family — TLS is via `rustls` only.

## Commit and PR conventions

- **Branch from `next`, not `main`** (ADR-0025 §D6). `next` is the integration
  + beta lane; `main` is the stable lane and advances only via a `next → main`
  promotion (stable fixes also flow through `next`; direct-to-`main` hotfix is
  retired by ADR-0033). Branch name format: `<type>/<short-slug>`,
  e.g. `fix/safekey-collision`, `feat/citation-graph`,
  `docs/clarify-store-spec`.
- Commit messages follow Conventional Commits: `feat:`, `fix:`, `docs:`, `chore:`,
  `refactor:`, `test:`, `ci:`, `perf:`.
- Sign your commits. Both `main` and `next` are branch-protected with the same
  required checks. Release **tags** MUST be signed — **GPG or SSH** (`git tag
  -s` with `gpg.format` set accordingly); the release version gate verifies the
  signature against `.github/allowed_signers` (ADR-0025 §D2-G7 / Amendment).
- Each PR has a single, well-scoped purpose. Multi-purpose PRs will be asked to split.
- The PR description must reference any related ADR, Discussion, or issue.

### Version bumps (enforced — ADR-0033)

Every PR must advance `[workspace.package].version`; the **blocking
`version-bump` check** (`scripts/version-bump-gate.sh`) verifies it. There are
**no label-based exceptions**.

- **PR → `next`:** bump `beta.N` by **exactly +1** (e.g. `0.8.0-beta.3 →
  0.8.0-beta.4`). To raise the cycle's base (a fix-only cycle that grows into a
  feature/breaking release), set it to a valid **+1 single-component step over
  the current stable** (`origin/main`) — `patch+1`, `minor+1`, or `major+1`,
  lower components zeroed — and reset the counter to `-beta.1`. You cannot skip
  or hold the version, and the base must stay a single step over stable so
  `next` is always promotable (after a promotion the first PR MUST retarget).
- **PR → `main`:** promotion only. The PR head **must be `next`** — there is no
  direct-to-`main` path (stable fixes also go through `next`; ADR-0033 retires
  ADR-0025 §D6 rule 4). The version must be a **clean `X.Y.Z`** that is exactly
  **+1 major/minor/patch** over `origin/main`, never a skip.
- The only exempt PR is the automated **`main → next` back-merge** (it keeps
  `next`'s `-beta.N`); it is recognised by branch shape, not a label.

This is separate from the advisory `version-check` job (release-readiness
visibility) and the tag-time release gate (ADR-0025 §D2).

## Release process

Releases are **tag-driven**; [ADR-0025](docs/DECISIONS/0025-tag-driven-release.md)
is the binding spec and full runbook. Summary:

- **Lanes.** A clean SemVer tag `vX.Y.Z` = **stable**, cut from `main`.
  `vX.Y.Z-beta.N` = **beta**, cut from `next`. crates.io has no dist-tags, so
  the SemVer pre-release identifier *is* the channel.
- **Cutting a release** (maintainer only): bump `[workspace.package].version`,
  curate the `## [X.Y.Z]` `CHANGELOG.md` section (helper:
  `scripts/release-changelog.sh`), commit, then push **one signed tag**
  (`git tag -s vX.Y.Z … && git push origin vX.Y.Z`). The
  `.github/workflows/release-plz.yml` pipeline (kept at that filename only to
  preserve the crates.io Trusted Publisher binding — release-plz itself was
  removed) runs a mandatory **version gate**, then publishes
  `doiget-core → doiget-mcp → doiget-cli` to crates.io via OIDC,
  sigstore-signs the binaries, emits an SBOM, and opens the GitHub Release.
- **The gate fails closed** (ADR-0025 §D2): tag↔manifest mismatch, a
  missing/empty `## [X.Y.Z]` CHANGELOG section, a non-monotonic version,
  prerelease/lane inconsistency, or an unsigned tag aborts the release
  *before* anything is published. A partial publish is recovered by re-running
  the **same** tag on a fixed pipeline (already-published crates idempotently
  skip) — never by force-overwrite; crates.io is immutable.
- A pipeline bug fix only takes effect for a re-run if the tag is re-pointed
  to the fixed commit (the pipeline runs the workflow/scripts *as of the
  tagged tree*). Do not reintroduce a perpetual "release PR".

### npm: the one-time bootstrap

The `publish to npm` job authenticates over OIDC and holds no `NPM_TOKEN`.
That is the right end state and it cannot bootstrap itself: **npm Trusted
Publishing cannot perform a package's first publish**, because the trusted
publisher is configured under a package's Settings and an unpublished package
has none. v0.8.11 shipped with that job red and the five packages still
absent from the registry.

Once, by a maintainer with an npm account:

1. `scripts/bootstrap-npm.sh` — dry run. It reads the package list from
   `scripts/stage-npm.sh`, checks each template is still at the `0.0.0`
   placeholder, and refuses to proceed if any package already exists.
2. `npm login`, with 2FA enabled and the GitHub account linked — npm's
   documented fallback when a 2FA device and its recovery codes are both
   lost.
3. `scripts/bootstrap-npm.sh --publish` — publishes the five templates
   verbatim under the `placeholder` dist-tag, then deprecates them.
4. On npmjs.com, for **each** of the five: Settings → Trusted Publisher →
   GitHub Actions, org `QAtlasHub`, repo `doiget`, workflow
   `release-plz.yml`, allowed action `npm publish`, environment empty.
5. Revoke the token.

`latest` is deliberately left unset. `npm publish` only moves `latest` when
the tag is `latest`, so until a real release `npm install doiget` fails with
"No matching version" — a clean error rather than an install of a wrapper
with no binary in it.

## ADR workflow

When proposing a binding decision:

1. Create `docs/DECISIONS/NNNN-<slug>.md` using the existing ADRs as a template
   (Context / Decision / Consequences / Status).
2. Append the entry to `docs/DECISIONS/INDEX.md`.
3. Reference the ADR number in the PR title (e.g. `feat: implement ADR-0007 safekey`).

Do not edit accepted ADRs in place. To revise a decision, write a new ADR that supersedes
the old one (the old ADR's `Status:` becomes `Superseded by NNNN`).

### ADR acceptance closes the source Discussion

An ADR flips from `Proposed` to `Accepted` when its decision is merged (the
implementing slice/PR is recorded in the ADR `Status:` line and in
`docs/DECISIONS/INDEX.md`). When that happens, the **source GitHub Discussion**
(the `Source Discussion` column in `INDEX.md`) is **locked** and a closing
comment links the superseding ADR, e.g.:

> Resolved by [ADR-0010](docs/DECISIONS/0010-citation-graph-hard-cap.md)
> (Accepted, Slice 14). Binding decision now lives in the ADR; this thread is
> locked for history.

This keeps the binding decision in the source tree (per the ADR rationale —
Discussions may be deleted) and prevents a resolved Discussion from looking
like an open question. Locking is a deliberate human action via the GitHub UI
/ `gh`; it is **not** automated by CI.

> **Follow-up (noted, not done here):** the 19 design Discussions whose ADRs
> were reconciled to `Accepted` in issue #150 still need this lock + closing
> comment back-filled. That is tracked separately and is out of scope for the
> docs-reconciliation PR that introduced this rule.

## Posture lint

A CI workflow (`.github/workflows/posture-lint.yml`) scans the repository for:

- Forbidden marketing terms ("bypass", "circumvent", "free papers", "Sci-Hub alternative").
- Imports of forbidden HTTP server / telemetry / self-update crates.
- Author-controlled telemetry endpoints in source.

PRs that introduce any of these will fail CI. If you believe your PR has been
false-flagged, please add a justification in the PR description and the maintainer will
review.

## Security and responsible disclosure

If your PR touches authentication, credential handling, log integrity, or any source
adapter that handles publisher API keys, please CC the security threat model
([docs/SECURITY.md](docs/SECURITY.md)) and walk through the relevant threat surfaces in
the PR description.

For vulnerability reports, **do not file a public issue**. See [CONTACT.md](CONTACT.md).

## Code of Conduct

By participating in this project, you agree to abide by the
[Contributor Covenant](https://www.contributor-covenant.org/) v2.1. Reports of
unacceptable behavior may be sent to the maintainer at the contact email above.

## Contributor License Agreement

doiget's public releases are, and will remain, available under an OSI-approved open
source license — today the MIT License (see [LICENSE](LICENSE)). That promise binds the
maintainer, not you, and it is written here so it is checkable.

The grant below is broader than "MIT in, MIT out". It exists so the project can change
its outbound license later — a copyleft core with separately licensed connectors, say —
without having to find and re-ask every past contributor. The alternatives that were
considered and rejected are recorded in
[ADR-0051](docs/DECISIONS/0051-contributor-license-agreement.md).

### 1. Copyright grant

You grant the maintainer (Sota Shimozono) and recipients of software distributed by the
project a perpetual, worldwide, non-exclusive, no-charge, royalty-free, irrevocable
copyright license to reproduce, prepare derivative works of, publicly display, publicly
perform, sublicense, and distribute your contribution and such derivative works, **under
any license terms, including terms that differ from those in effect when you
contributed**.

You keep your copyright. This is a license, not an assignment, and it is non-exclusive:
your contribution remains yours to use anywhere else, under any terms you like.

### 2. Patent grant

You grant the maintainer and recipients of software distributed by the project a
perpetual, worldwide, non-exclusive, no-charge, royalty-free, irrevocable (except as
stated below) patent license to make, have made, use, offer to sell, sell, import, and
otherwise transfer your contribution. This license covers only those patent claims
licensable by you that are necessarily infringed by your contribution alone, or by the
combination of your contribution with the project.

If any entity institutes patent litigation alleging that your contribution, or the
project it was contributed to, constitutes direct or contributory patent infringement,
every patent license granted to that entity under this section terminates as of the date
the litigation is filed.

### 3. What you certify

By signing off (§4) you certify that:

1. The contribution is your original work, or you have the right to submit it under this
   agreement — including, where the work is not entirely yours, that its license is
   compatible with this grant and that you have identified that license and its origin
   in the contribution itself.
2. You are legally entitled to grant the licenses above. If your employer holds rights
   to intellectual property you create, you have permission to contribute on their
   behalf, or your employer has waived those rights.
3. You know of no third-party claim, patent, or license that your contribution
   infringes.

Tooling does not change this. A patch drafted with AI assistance is still submitted
under your certification, and item 1 is yours to satisfy.

If any of this later turns out to be untrue, tell the maintainer (see
[CONTACT.md](CONTACT.md)) rather than leaving it in the tree.

### 4. How you agree: commit sign-off

Put a `Signed-off-by` trailer on every commit in your PR, with your real name and an
email address you control:

```bash
git commit -s -m "fix(store): ..."
#   -s writes:  Signed-off-by: Your Name <you@example.com>
#               taken from your git user.name / user.email

git rebase --signoff origin/next   # sign off commits you already made
```

A sign-off means: *"I agree to the Contributor License Agreement in this file, as it
read when I contributed."*

The trailer is deliberately the whole mechanism. It lives in `git log`, so the record is
auditable from a clone alone, and no signature service has to outlive the project.
[`dco.yml`](.github/workflows/dco.yml) checks it on every PR; commits authored by bots
and merge commits created by GitHub are exempt.

### 5. Scope

This section governs contributions to this repository. It does not change the terms
under which you received doiget, and it is not retroactive — contributions merged before
it was added are licensed under the MIT License alone, per the text it replaces.

**This is not legal advice, and the maintainer is not a lawyer.** If your employer needs
a countersigned agreement rather than a commit trailer, contact the maintainer before
opening a PR.
