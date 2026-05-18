# 0025 - Tag-driven release with a mandatory version gate and beta/stable lanes

- **Date:** 2026-05-17
- **Status:** Accepted — implemented by PR #166; amended 2026-05-18 (see Amendment) so the workflow filename stays `.github/workflows/release-plz.yml` (tag-driven pipeline contents + `scripts/release-version-gate.sh` + `cliff.toml`/`scripts/release-changelog.sh`; only `release-plz.toml` removed; `release-sign.yml`/`release-sbom.yml` demoted to `workflow_dispatch`-only and folded in). `0013`'s release portion is `Superseded by 0025` (its CI-baseline / posture-lint / SHA-pin / Dependabot decisions stand).
- **Supersedes:** 0013 (release portion only — the CI baseline / posture-lint / SHA-pin / Dependabot decisions of 0013 stand)
- **Source:** Maintainer release-workflow review, 2026-05-17 (release-plz `release-PR` model rejected after the v0.1.4 `#164` changelog defect)

## Context

ADR-0013 adopted **release-plz** in its *release-PR* mode: every push to `main`
opens/updates one perpetual "release PR" that bumps the workspace version and
appends to `CHANGELOG.md`; merging that PR mints per-crate tags
(`doiget-core-v*`, `doiget-cli-v*`, `doiget-mcp-v*`), creates GitHub Releases,
and `cargo publish`es to crates.io via OIDC. `release-sign.yml` and
`release-sbom.yml` already trigger on `push: tags: doiget-cli-v*`.

Two concrete failures of that model surfaced on 2026-05-17:

1. **Changelog defect (`#164`).** PRs `#159/#160/#161/#162/#163/#165` were
   merged as merge commits. release-plz's git-cliff changelog generation keyed
   off the merge-commit subjects (`Merge pull request #NNN …`), so the
   generated `## [0.1.4]` section captured only **one** `fix(core)` line plus a
   stray `Merge remote-tracking branch …` line, silently dropping the MCP
   spec-conformance, CLI exit-code-contract, credential-hygiene and docs work.
   crates.io versions are immutable; merging `#164` would have published an
   immutable, materially inaccurate public release record.

2. **Ergonomic mismatch.** The "release intent" is buried in *merging a
   long-lived PR* rather than in an explicit, reviewable act. The maintainer
   wants the obsidian-remote-ssh / Julia-Registrator+TagBot ergonomic where the
   **tag is the release**, tag↔publish↔GitHub-Release are tightly coupled, and
   there is no perpetual PR to babysit.

crates.io has **no dist-tag mechanism** (no npm-style `@beta`). The only
crates.io-native pre-release channel is a **SemVer pre-release identifier**
(`1.2.3-beta.1`), which `cargo` will not auto-select unless a consumer opts in
explicitly. Any beta/stable split must be expressed through SemVer, not a
registry-side channel.

The three crates share one version (`version.workspace = true`), so a single
workspace tag is sufficient; the per-crate `doiget-<crate>-v*` tag scheme that
release-plz imposed is unnecessary surface.

## Decision

Replace the release-plz release-PR model with a **tag-driven release
pipeline** whose only entry point is pushing an annotated, signed tag, gated by
a mandatory version-consistency job.

### D1 — Trigger: a single workspace tag is the release

```mermaid
flowchart LR
  subgraph OLD["OLD (ADR-0013, release-plz release-PR)"]
    A1[push to main] --> A2[release-plz updates<br/>perpetual release PR]
    A2 --> A3[maintainer merges PR]
    A3 --> A4[per-crate tags<br/>core-v*, cli-v*, mcp-v*]
    A4 --> A5[publish + GH release<br/>changelog from merge commits]
  end
  subgraph NEW["NEW (ADR-0025, tag-driven)"]
    B1[maintainer prepares release commit<br/>version bump + CHANGELOG] --> B2[git tag -s vX.Y.Z + push tag]
    B2 --> B3{{version gate}}
    B3 -- pass --> B4[publish core then cli/mcp]
    B3 -- fail --> BX[release aborted<br/>nothing published]
    B4 --> B5[sigstore sign + SBOM]
    B5 --> B6[GitHub Release<br/>body = CHANGELOG section]
  end
```

- The release workflow lives at `.github/workflows/release-plz.yml`,
  `on: push: tags: ['v*']`. **The filename `release-plz.yml` is retained
  deliberately** (see the Amendment below): the crates.io OIDC Trusted
  Publisher binding is `(repo, workflow filename)`-scoped, so keeping the name
  avoids re-registering it. The file no longer runs release-plz; its contents
  are this tag-driven pipeline.
- **One** workspace tag `vX.Y.Z` (or `vX.Y.Z-beta.N`). The per-crate
  `doiget-<crate>-v*` tag scheme is retired. `release-sign.yml` /
  `release-sbom.yml` are folded into the pipeline as `sign` / `sbom` jobs
  (their standalone files demoted to `workflow_dispatch`-only escape hatches).
- `release-plz.toml` is **removed** and the `release-plz` action is no longer
  invoked. Version bump + `CHANGELOG.md` editing become an explicit pre-tag
  step (script-assisted, see D4), not an auto-PR.
- Tags MUST be annotated and signed — **GPG or SSH** (`git tag -s` with
  `gpg.format` set to `openpgp` or `ssh`). G7 verifies the signature; SSH
  signatures are checked against the committed `.github/allowed_signers`
  (principal = tagger email). See the Amendment.

### D2 — Mandatory version gate (runs first; abort on any failure)

```mermaid
flowchart TD
  T[tag vX.Y.Z pushed] --> G0[parse tag to X.Y.Z + prerelease?]
  G0 --> G1{tag == workspace.package version?}
  G1 -- no --> F[FAIL: tag/manifest drift]
  G1 -- yes --> G2{Cargo.lock in sync<br/>cargo metadata --locked?}
  G2 -- no --> F
  G2 -- yes --> G3{X.Y.Z unpublished on crates.io<br/>for all 3 crates?}
  G3 -- no --> F
  G3 -- yes --> G4{semver strictly greater than<br/>last published in this lane?}
  G4 -- no --> F
  G4 -- yes --> G5{CHANGELOG.md has non-empty<br/>section for X.Y.Z?}
  G5 -- no --> F
  G5 -- yes --> G6{prerelease consistent?<br/>tag-beta iff manifest-beta iff lane}
  G6 -- no --> F
  G6 -- yes --> G7{tag signed + on allowed<br/>source branch for its lane?}
  G7 -- no --> F
  G7 -- yes --> P[gate PASS to publish]
```

The gate is the structural fix for the `#164` class of defect: **a release
cannot proceed unless `CHANGELOG.md` already contains a non-empty section for
exactly this version.** No publish step runs until the gate is green.

### D3 — beta / stable lanes via SemVer pre-release

| Tag | Lane | crates.io | GitHub Release | Allowed source |
| --- | --- | --- | --- | --- |
| `vX.Y.Z-beta.N` | **beta** | published as SemVer pre-release (not auto-selected by `cargo`) | `prerelease: true`, not "latest" | `next` only |
| `vX.Y.Z` | **stable** | normal publish | `prerelease: false`, "latest" | `main` only |

```mermaid
flowchart TD
  TAG[tag pushed] --> Q{matches vX.Y.Z-PRE.N ?}
  Q -- yes --> BETA[beta lane:<br/>manifest MUST carry -PRE.N<br/>GH prerelease=true<br/>monotonic within beta]
  Q -- no --> STABLE[stable lane:<br/>manifest MUST be clean X.Y.Z<br/>GH latest<br/>tag MUST be on main<br/>monotonic over last stable]
  BETA --> PUB[publish]
  STABLE --> PUB
```

Consumers opt into beta explicitly (`cargo add doiget-cli@=X.Y.Z-beta.N` or
`--pre`); there is intentionally no implicit `@beta` convenience because
crates.io has no dist-tags.

### D4 — CHANGELOG strategy

`CHANGELOG.md` stays a single hand-curated workspace file (as today). For each
release the maintainer runs a local helper that invokes **git-cliff** over the
range `<last-tag-in-lane>..HEAD` with a `cliff.toml` configured to traverse
**all** commits (not first-parent) so conventional commits behind merge commits
are not lost — the exact failure mode of `#164`. The generated section is
**reviewed and edited** before the release commit is made. The version gate
(D2-G5) then enforces that the section exists and is non-empty at tag time.
git-cliff replaces release-plz purely as a *local, pre-tag, reviewable*
changelog generator — never as an automated merge-time PR.

### D5 — Pipeline order & irreversibility handling

```mermaid
sequenceDiagram
  participant M as Maintainer
  participant GH as GitHub Actions
  participant CR as crates.io
  M->>M: bump version, git-cliff then edit CHANGELOG, commit
  M->>GH: git tag -s vX.Y.Z and git push --tags
  GH->>GH: version gate (D2) — abort-safe, nothing external yet
  GH->>CR: cargo publish doiget-core (--locked)
  GH->>CR: cargo publish doiget-cli, doiget-mcp (depend on core)
  GH->>GH: sigstore sign + SBOM for the binary
  GH->>GH: create GitHub Release (body = CHANGELOG X.Y.Z, prerelease per lane)
```

Irreversibility is contained by ordering: every reversible/abortable check
(the entire gate) happens **before** the first irreversible action
(`cargo publish`). crates.io publish is per-crate sequential in dependency
order (`core` → `cli`/`mcp`); if `cli`/`mcp` publish fails after `core`
succeeded, the recovery is a new patch tag, never a force-overwrite (impossible
on crates.io). The gate's "unpublished" check (D2-G3) makes a re-run idempotent
up to the first successful crate.

### D6 — Branch model: `main` + `next` (resolves O1 = option b)

Two long-lived branches. `next` is the integration + beta lane; `main` is the
stable lane. The version gate's source-branch check (D2-G7) is configured from
this model.

```mermaid
flowchart TD
  PR[feature / fix PRs] --> NX[next branch<br/>Cargo.toml = X.Y.Z-beta.N]
  NX -->|git tag -s vX.Y.Z-beta.N on next| BG{{version gate: beta lane}}
  BG -- pass --> BPUB[crates.io prerelease<br/>GH prerelease=true]
  NX -->|promote: bless next as stable| PROMO[strip -beta.N → X.Y.Z<br/>merge --no-ff next → main]
  PROMO --> MN[main branch<br/>Cargo.toml = X.Y.Z]
  MN -->|git tag -s vX.Y.Z on main| SG{{version gate: stable lane}}
  SG -- pass --> SPUB[crates.io stable<br/>GH latest]
  MN -->|stable hotfix| HF[patch on main → tag vX.Y.Z] --> BM[back-merge main → next<br/>so next never regresses]
  BM --> NX
```

Binding rules of the branch model:

1. **PR target.** All feature/fix PRs target `next`. `main` only ever receives
   commits via a `next → main` promotion merge or a stable hotfix.
2. **Version string per branch.** `next` always carries a pre-release
   identifier in `[workspace.package].version` (`X.Y.Z-beta.N`); `main` always
   carries a clean `X.Y.Z`. The gate (D2-G6/G7) rejects any tag whose
   prerelease-ness disagrees with its branch.
3. **Promotion.** Blessing `next` as stable = drop the `-beta.N` suffix in a
   release commit on `next`, then `git merge --no-ff next` into `main`, then
   tag `vX.Y.Z` on `main`. (`--no-ff` keeps an auditable promotion commit;
   it also means `main`/`next` history is a merge graph — git-cliff is
   configured for all-commits traversal per D4, so this does not reintroduce
   the `#164` loss.)
4. **Hotfix.** An urgent stable fix lands directly on `main` (patch tag
   `vX.Y.Z`), then `main` is **back-merged into `next`** immediately so the
   beta lane never regresses relative to stable.
5. **Branch protection.** `next` carries the *same* required checks as `main`
   (`test (ubuntu-latest)`, `test (windows-latest)`) so betas are gated as
   strictly as stable. "Require branches up to date before merging" applies to
   both (already in force on `main`).
6. **First cutover.** The implementing PR creates `next` from `main`'s
   post-implementation HEAD and sets `next`'s version to the next
   `X.Y.(Z+1)-beta.1`.

### D7 — Disposition of the in-flight release (`#164`)

`#164` was a release-plz PR that bumped `0.1.3 → 0.1.4` with a materially
inaccurate auto-generated CHANGELOG. Close `#164` and remove
`release-plz.{toml,yml}` as part of the implementing PR. The **first release
under ADR-0025 is `v0.2.0`**, not `0.1.4`: the work since `0.1.3`
(`#159/#160/#161/#162/#163/#165`) includes explicitly called-out breaking
changes to the CLI exit-code contract and the MCP tool spec, so the **minor**
bump signals that breaking surface under this project's 0.x semver policy
(strict semver for `doiget-core`; CLI/MCP breaks permitted within 0.x when
enumerated — CHANGELOG header). It is cut by a signed tag `v0.2.0` on `main`
with the correctly hand-curated CHANGELOG. No defective intermediate release is
published. (Whether `v0.2.0` first ships a `v0.2.0-beta.1` from `next` or goes
straight to stable `v0.2.0` on `main` is a rollout choice, not an architectural
one.)

## Consequences

**Positive.** The tag is the single, explicit, reviewable release act
(matches the maintainer's mental model). tag↔publish↔GitHub-Release are tightly
coupled. The `#164` changelog-defect class is structurally impossible (gate
D2-G5). No perpetual release PR. beta/stable is crates.io-native. Every
irreversible step is preceded by an abortable gate.

**Negative / cost.** Version bump + changelog move from "automated PR" to a
deliberate maintainer step (mitigated by the git-cliff helper). The maintainer
must remember the bump-commit-then-tag order (mitigated: the gate rejects
tag/manifest drift loudly instead of publishing a mismatched release). The
`main`+`next`
two-branch model adds a back-merge discipline (hotfix → back-merge to `next`)
and a promotion ritual (mitigated: the gate enforces lane/branch/semver
invariants mechanically, and the back-merge is a fixed checklist step).
git-cliff's all-commits traversal must be validated against the repo's
merge-commit history before first use.

**Governance.** This ADR is `Proposed`. On merge of the implementing PR:
flip this to `Accepted` (note the PR), flip `0013` `Status:` to
`Superseded by 0025` (release portion), add the `0025` row to
`DECISIONS/INDEX.md`, and update any NORMATIVE doc that references the
release process. To revise this decision, write a new ADR with
`Supersedes: 0025` per `CONTRIBUTING.md`.

## Amendment — 2026-05-18: retain the `release-plz.yml` filename

D1 originally specified a new file `.github/workflows/release.yml` with
`release-plz.yml` removed. PR #166 implemented it that way. This amendment
(separate follow-up PR) **renames the pipeline back to
`.github/workflows/release-plz.yml`** (only `release-plz.toml` stays removed).

**Why:** the crates.io OIDC Trusted Publisher for `doiget-core`/`-cli`/`-mcp`
is bound to `(repo, workflow filename)` and is **not** environment-scoped
(verified against the pre-#166 `release-plz.yml`: no `environment:` key).
Keeping the filename means the existing Trusted Publisher continues to
authorize the publish job with **zero crates.io reconfiguration**; renaming
would have required re-registering all three crates before the next publish.

**Scope of the amendment:** filename + internal references only. No change to
the decision, the version gate (D2), lanes (D3), changelog strategy (D4),
pipeline order (D5), branch model (D6), or `#164` disposition (D7). The
filename is a deliberate, documented misnomer (header note in the workflow).
This is recorded as an in-place amendment rather than a superseding ADR
because no decision changed — only an implementation detail was corrected.

**Signing mechanism (same amendment):** D1 originally said "GPG-signed". The
maintainer's environment has no GPG secret key but does have an SSH key
already trusted by GitHub for auth. Tag signing therefore accepts **GPG *or*
SSH** (`git tag -s` with `gpg.format=ssh`). G7 verifies SSH signatures against
the repo-committed `.github/allowed_signers` (public keys; principal = tagger
email). This does not weaken the property D1 protects — the tag is still
cryptographically signed by the maintainer and verified by the gate before any
publish; only the signature *format* is broadened. Adding/rotating a release
signer is a one-line reviewed change to `.github/allowed_signers`.

## Amendment — 2026-05-18 (2): publish order + partial-publish-recovery gate

The first real `v0.2.0` cut exposed two implementation bugs (the *design*
stands; D5 already anticipated partial-publish recovery — the code did not
implement it correctly):

1. **Publish order was wrong.** D5/the workflow published `doiget-core →
   doiget-cli → doiget-mcp`, assuming `cli`/`mcp` only depend on `core`. In
   fact **`doiget-cli` depends on BOTH `doiget-core` AND `doiget-mcp`**, so
   `cargo publish -p doiget-cli` failed (`failed to select a version for
   doiget-mcp`) after `core` had already published. The topological order is
   **`doiget-core → doiget-mcp → doiget-cli`** (cli last). Fixed in
   `.github/workflows/release-plz.yml`.

2. **G3/G4 blocked the D5 recovery re-run.** G3 hard-failed if *any* crate
   already published `$TAG_VERSION`. After the partial `v0.2.0`
   (`doiget-core@0.2.0` live, `cli`/`mcp` not), every re-run aborted at the
   gate, so D5's idempotent recovery in the publish step was unreachable —
   directly contradicting D5. G3/G4 are now partial-publish-aware: a crate
   already at `$TAG_VERSION` is a recovery skip (the publish step idempotently
   skips it); the gate FAILs only when **all** crates already publish the
   version (a complete re-release / forgotten bump); 1..n−1 published is a
   PASS-with-notice recovery. Fixed in `scripts/release-version-gate.sh`.

Recovery for the in-flight `v0.2.0`: with this PR merged, re-run the pipeline
for the existing tag (`gh workflow run release-plz.yml -f tag=v0.2.0`) — the
gate now passes as partial-recovery, `doiget-core@0.2.0` idempotently skips,
and `doiget-mcp@0.2.0` then `doiget-cli@0.2.0` publish. No new tag is minted;
`doiget-core@0.2.0` (already immutable on crates.io) is reused as-is.

## Amendment — 2026-05-18 (3): back-merge PR automation

§D6 rule 4 (anything landing on `main` → back-merge to `next` so the beta
lane never regresses) is automated by `.github/workflows/backmerge.yml`:
on every push to `main`, if `next` is behind `main`, it **opens** (or leaves
the existing) `main → next` PR. Scope is deliberately *open-only* — it does
**not** auto-merge: `next` is branch-protected, and the predictable
`Cargo.toml`/`Cargo.lock` version conflict (`next` carries `X.Y.Z-beta.N`,
`main` the clean `X.Y.Z`) is resolved by the human at merge time, **keeping
`next`'s `-beta.N`**. It authenticates with the `RELEASE_PLZ_TOKEN` PAT
(reused) because a `GITHUB_TOKEN`-opened PR would not trigger the required
CI and so could never merge into protected `next`. Auto-resolving the
version conflict in CI (a merge driver / normalize step) is a possible
future enhancement, intentionally out of scope here.
