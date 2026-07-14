#!/usr/bin/env bash
# release-version-gate.sh — the mandatory, abort-safe pre-publish gate for the
# tag-driven release pipeline (ADR-0025 D2). It runs FIRST in release-plz.yml; if
# any check fails NOTHING external (cargo publish / GitHub Release) runs, so
# every irreversible step is preceded by this entirely reversible gate.
#
# Usage:
#   scripts/release-version-gate.sh vX.Y.Z [--offline-skip-crates-io]
#   GITHUB_REF_NAME=vX.Y.Z scripts/release-version-gate.sh
#
# The tag is taken from $1 if given, else $GITHUB_REF_NAME (the ref a
# `push: tags` workflow sets). Checks G0–G7 per ADR-0025 D2; each is a clear
# PASS/FAIL with an actionable `::error::` annotation, and the gate aborts on
# the FIRST failure (no cascading noise). G0–G2, G5, G6 need no network and run
# locally; G3/G4 query crates.io and G7 inspects git refs, both guarded behind
# availability so a local dry-run still exercises the structural checks.
#
# `--offline-skip-crates-io` skips ONLY G3/G4 (the crates.io reachability
# checks) for a local dry-run. It MUST NOT be passed in CI — release-plz.yml calls
# this script with no skip flag so the network checks always run for a real
# release.
#
# House style mirrors scripts/sync_docs_to_site.sh: a thin, dependency-light
# bash helper (no Python / Node / jq) that Just Works from Linux, macOS, and
# Git Bash on Windows. `curl` is the only non-coreutils external (present on
# all three CI runners and in Git Bash).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

CARGO_TOML="$REPO_ROOT/Cargo.toml"
CHANGELOG="$REPO_ROOT/CHANGELOG.md"
CRATES=(doiget-core doiget-cli doiget-mcp)

SKIP_CRATES_IO=0
TAG_ARG=""
for arg in "$@"; do
  case "$arg" in
    --offline-skip-crates-io) SKIP_CRATES_IO=1 ;;
    -*) echo "::error::unknown flag '$arg' (expected --offline-skip-crates-io)" >&2; exit 2 ;;
    *) TAG_ARG="$arg" ;;
  esac
done

TAG="${TAG_ARG:-${GITHUB_REF_NAME:-}}"

fail() {
  # One actionable line, GitHub-annotated, then abort immediately.
  echo "::error::$*" >&2
  exit 1
}
pass() { echo "PASS $*"; }

# ---------------------------------------------------------------------------
# G0 — parse the tag to X.Y.Z (+ optional pre-release identifier).
# Accepts `vX.Y.Z` (stable) and `vX.Y.Z-PRE.N` (beta lane, ADR-0025 D3).
# ---------------------------------------------------------------------------
if [ -z "$TAG" ]; then
  fail "G0: no tag given (pass it as \$1 or set \$GITHUB_REF_NAME)"
fi

# Strict SemVer-ish: digits.digits.digits, optional `-beta.N` style pre-release.
if [[ "$TAG" =~ ^v([0-9]+)\.([0-9]+)\.([0-9]+)(-([0-9A-Za-z.-]+))?$ ]]; then
  MAJOR="${BASH_REMATCH[1]}"
  MINOR="${BASH_REMATCH[2]}"
  PATCH="${BASH_REMATCH[3]}"
  PRERELEASE="${BASH_REMATCH[5]}"   # empty for a stable tag
else
  fail "G0: tag '$TAG' is not vX.Y.Z or vX.Y.Z-PRE.N (SemVer)"
fi
XYZ="${MAJOR}.${MINOR}.${PATCH}"
if [ -n "$PRERELEASE" ]; then
  TAG_VERSION="${XYZ}-${PRERELEASE}"
  LANE="beta"
else
  TAG_VERSION="$XYZ"
  LANE="stable"
fi
pass "G0: tag '$TAG' -> version '$TAG_VERSION' (lane: $LANE)"

# ---------------------------------------------------------------------------
# G1 — tag version == [workspace.package] version in Cargo.toml.
# ---------------------------------------------------------------------------
if [ ! -f "$CARGO_TOML" ]; then
  fail "G1: $CARGO_TOML not found"
fi
# First `version = "..."` after the `[workspace.package]` header. awk so we do
# not need toml tooling (house style: no jq / extra deps).
MANIFEST_VERSION="$(awk '
  /^\[workspace\.package\]/ { in_wp = 1; next }
  /^\[/ { in_wp = 0 }
  in_wp && /^[[:space:]]*version[[:space:]]*=/ {
    line = $0
    sub(/^[^"]*"/, "", line)
    sub(/".*$/, "", line)
    print line
    exit
  }
' "$CARGO_TOML")"
if [ -z "$MANIFEST_VERSION" ]; then
  fail "G1: could not read [workspace.package] version from Cargo.toml"
fi
if [ "$MANIFEST_VERSION" != "$TAG_VERSION" ]; then
  fail "G1: tag/manifest drift — tag is '$TAG_VERSION' but [workspace.package] version is '$MANIFEST_VERSION'. Bump Cargo.toml (and Cargo.lock) to match the tag before tagging."
fi
pass "G1: tag version == manifest version ($TAG_VERSION)"

# ---------------------------------------------------------------------------
# G2 — Cargo.lock is in sync (cargo metadata --locked succeeds).
# ---------------------------------------------------------------------------
CARGO_BIN="${CARGO:-cargo}"
if ! command -v "$CARGO_BIN" >/dev/null 2>&1; then
  fail "G2: '$CARGO_BIN' not on PATH (set \$CARGO to the cargo binary)"
fi
if ! "$CARGO_BIN" metadata --locked --format-version 1 >/dev/null 2>&1; then
  fail "G2: 'cargo metadata --locked' failed — Cargo.lock is out of sync with Cargo.toml. Run 'cargo metadata' (or 'cargo update -p doiget-core -p doiget-cli -p doiget-mcp --precise $TAG_VERSION') and commit Cargo.lock."
fi
pass "G2: Cargo.lock in sync (cargo metadata --locked OK)"

# ---------------------------------------------------------------------------
# G5 — CHANGELOG.md has a non-empty `## [X.Y.Z]` section.
# THE #164-class structural fix: strict — the section must EXIST and have at
# least one non-blank content line before the next `## ` heading. (Run before
# the network checks so a local dry-run surfaces this without a network.)
# ---------------------------------------------------------------------------
if [ ! -f "$CHANGELOG" ]; then
  fail "G5: $CHANGELOG not found"
fi
# Match `## [X.Y.Z]` or `## [X.Y.Z](...)` exactly (TAG_VERSION may include the
# -PRE suffix; that is the section the maintainer must have written).
CL_SECTION="$(awk -v ver="$TAG_VERSION" '
  BEGIN { in_sec = 0; content = 0 }
  {
    # Heading lines like: ## [0.1.4] - 2026-05-18  OR  ## [0.1.4](url) - ...
    if ($0 ~ /^## \[/) {
      if (in_sec == 1) { exit }                 # reached the next section
      hv = $0
      sub(/^## \[/, "", hv)
      sub(/\].*$/, "", hv)
      if (hv == ver) { in_sec = 1; next }
    }
    if (in_sec == 1) {
      line = $0
      gsub(/[[:space:]]/, "", line)
      if (length(line) > 0) { content = 1 }
    }
  }
  END { print in_sec "|" content }
' "$CHANGELOG")"
CL_FOUND="${CL_SECTION%%|*}"
CL_HASCONTENT="${CL_SECTION##*|}"
if [ "$CL_FOUND" != "1" ]; then
  fail "G5: CHANGELOG.md has no '## [$TAG_VERSION]' section. ADR-0025 D2-G5: a release cannot proceed unless CHANGELOG.md already contains a curated section for exactly this version (this is the #164-class fix). Add the section, review it, then re-tag."
fi
if [ "$CL_HASCONTENT" != "1" ]; then
  fail "G5: CHANGELOG.md '## [$TAG_VERSION]' section is empty (no non-blank content line before the next '## '). Curate the section before tagging."
fi
pass "G5: CHANGELOG.md has a non-empty [$TAG_VERSION] section"

# ---------------------------------------------------------------------------
# G6 — prerelease consistency: tag has `-PRE` ⇔ manifest has `-PRE` ⇔ lane.
# (G1 already proved tag==manifest string equality, so a mismatch here would
# be caught at G1; we still assert lane/prerelease explicitly per ADR D6.2 so
# the failure message names the lane invariant.)
# ---------------------------------------------------------------------------
MANIFEST_HAS_PRE=0
case "$MANIFEST_VERSION" in *-*) MANIFEST_HAS_PRE=1 ;; esac
TAG_HAS_PRE=0
[ -n "$PRERELEASE" ] && TAG_HAS_PRE=1
if [ "$TAG_HAS_PRE" != "$MANIFEST_HAS_PRE" ]; then
  fail "G6: prerelease mismatch — tag pre-release=$TAG_HAS_PRE but manifest pre-release=$MANIFEST_HAS_PRE. beta lane requires tag AND manifest to carry -PRE.N; stable lane requires both clean (ADR-0025 D6.2)."
fi
if [ "$LANE" = "beta" ] && [ "$TAG_HAS_PRE" != "1" ]; then
  fail "G6: lane is beta but tag has no pre-release identifier"
fi
if [ "$LANE" = "stable" ] && [ "$TAG_HAS_PRE" != "0" ]; then
  fail "G6: lane is stable but tag carries a pre-release identifier"
fi
pass "G6: prerelease consistent (tag/manifest/lane all '$LANE')"

# ---------------------------------------------------------------------------
# G3 — X.Y.Z not already published on crates.io for ALL of the 3 crates.
# G4 — version is strictly SemVer-greater than the latest published in lane.
# Both query crates.io; skipped only with --offline-skip-crates-io (local
# dry-run). They ALWAYS run in CI (release-plz.yml passes no skip flag).
# ---------------------------------------------------------------------------
# SemVer compare: returns 0 if $1 > $2, 1 if $1 == $2, 2 if $1 < $2.
# Pre-release ordering per SemVer §11: a pre-release has LOWER precedence than
# the associated normal version, and pre-release ids compare field-by-field.
semver_cmp() {
  local a="$1" b="$2"
  local an="${a%%-*}" bn="${b%%-*}"
  local ap="" bp=""
  case "$a" in *-*) ap="${a#*-}" ;; esac
  case "$b" in *-*) bp="${b#*-}" ;; esac
  local IFS=.
  # shellcheck disable=SC2206
  local av=($an) bv=($bn) i
  for i in 0 1 2; do
    local x="${av[$i]:-0}" y="${bv[$i]:-0}"
    if [ "$x" -gt "$y" ]; then echo 0; return; fi
    if [ "$x" -lt "$y" ]; then echo 2; return; fi
  done
  # Equal numeric core — disambiguate by pre-release.
  if [ -z "$ap" ] && [ -z "$bp" ]; then echo 1; return; fi
  if [ -z "$ap" ]; then echo 0; return; fi   # a is normal > b pre-release
  if [ -z "$bp" ]; then echo 2; return; fi   # a pre-release < b normal
  # shellcheck disable=SC2206
  local ai=($ap) bi=($bp) n=${#ai[@]} m=${#bi[@]} max=$n
  [ "$m" -gt "$max" ] && max=$m
  for ((i=0; i<max; i++)); do
    local p="${ai[$i]:-}" q="${bi[$i]:-}"
    if [ -z "$p" ]; then echo 2; return; fi   # a has fewer fields -> lower
    if [ -z "$q" ]; then echo 0; return; fi
    if [[ "$p" =~ ^[0-9]+$ && "$q" =~ ^[0-9]+$ ]]; then
      if [ "$p" -gt "$q" ]; then echo 0; return; fi
      if [ "$p" -lt "$q" ]; then echo 2; return; fi
    else
      if [[ "$p" > "$q" ]]; then echo 0; return; fi
      if [[ "$p" < "$q" ]]; then echo 2; return; fi
    fi
  done
  echo 1
}

if [ "$SKIP_CRATES_IO" -eq 1 ]; then
  echo "SKIP G3/G4: --offline-skip-crates-io set (local dry-run; MUST NOT be used in CI)"
else
  if ! command -v curl >/dev/null 2>&1; then
    fail "G3: curl not available and --offline-skip-crates-io not set — cannot verify crates.io publication state"
  fi
  # ADR-0025 D2-G3/G4 with D5 partial-publish-recovery semantics.
  # Per crate: if THIS crate already publishes the exact $TAG_VERSION, that is
  # the idempotent-recovery case (crates.io is immutable; the publish step
  # skips it) — record it and skip G4 for that crate. Otherwise enforce G4
  # (strictly SemVer-greater than every published version). AFTER the loop:
  #   - all crates already at $TAG_VERSION  -> FAIL (complete re-release / a
  #     forgotten version bump; nothing to do, crates.io is immutable);
  #   - some (1..n-1) already at it          -> PASS as partial-publish
  #     recovery (the publish step completes the not-yet-published siblings);
  #   - none                                 -> normal PASS.
  published_at_tag=0
  for crate in "${CRATES[@]}"; do
    # crates.io v1 API: 200 + a `versions` array. We grep the raw JSON for the
    # exact "num":"X.Y.Z" pair (no jq, house style). A 404 => crate never
    # published yet (treated as "not present").
    body="$(curl -fsSL --max-time 30 \
      -H 'User-Agent: doiget-release-version-gate (https://github.com/QAtlasHub/doiget)' \
      "https://crates.io/api/v1/crates/${crate}" 2>/dev/null || true)"
    if [ -z "$body" ]; then
      echo "note: crates.io returned no body for '$crate' (likely never published yet) — new publish, G3/G4 OK for this crate"
      continue
    fi
    if printf '%s' "$body" | grep -Eq "\"num\"[[:space:]]*:[[:space:]]*\"${TAG_VERSION//./\\.}\""; then
      published_at_tag=$((published_at_tag + 1))
      echo "::notice::G3/G4: $crate $TAG_VERSION already on crates.io (immutable) — partial-publish recovery; the publish step idempotently skips it (ADR-0025 D5)"
      continue
    fi
    # Not yet published for this crate — G4: TAG_VERSION must be strictly
    # greater than every published num. semver_cmp orders a pre-release below
    # its matching normal version, so a plain strict-greater check is correct
    # for both lanes (ADR-0025 D2-G4 "monotonic within lane").
    while IFS= read -r pub; do
      [ -z "$pub" ] && continue
      cmp="$(semver_cmp "$TAG_VERSION" "$pub")"
      if [ "$cmp" != "0" ]; then
        if [ "$cmp" = "1" ]; then
          fail "G4: $crate $TAG_VERSION equals an already-published version — not strictly greater (ADR-0025 D2-G4)."
        fi
        fail "G4: $crate $TAG_VERSION is NOT strictly SemVer-greater than published $pub (ADR-0025 D2-G4: monotonic within lane)."
      fi
    done < <(printf '%s' "$body" | grep -Eo '"num"[[:space:]]*:[[:space:]]*"[^"]+"' | sed -E 's/.*"([^"]+)"$/\1/')
    pass "G3/G4: $crate — $TAG_VERSION unpublished and strictly greater than all published"
  done
  if [ "$published_at_tag" -eq "${#CRATES[@]}" ]; then
    fail "G3: ALL ${#CRATES[@]} crates already publish $TAG_VERSION — this version is fully released; nothing to do. Recovery from a *partial* publish is allowed, but a *complete* re-release is not (crates.io is immutable). Did you forget to bump the version?"
  elif [ "$published_at_tag" -gt 0 ]; then
    pass "G3/G4: PARTIAL-PUBLISH RECOVERY — $published_at_tag/${#CRATES[@]} crate(s) already at $TAG_VERSION; the publish step idempotently skips them and completes the rest (ADR-0025 D5)"
  else
    pass "G3: $TAG_VERSION unpublished on crates.io for all ${#CRATES[@]} crates"
    pass "G4: $TAG_VERSION strictly SemVer-greater than latest published in lane"
  fi
fi

# ---------------------------------------------------------------------------
# G7 — tag is annotated + signed AND reachable from the allowed lane branch.
# beta  ⇒ tag reachable from `next`; stable ⇒ reachable from `main`
# (ADR-0025 D6). Skipped only when not in a git work-tree with the tag object
# present (e.g. a pure manifest dry-run); the message says so explicitly.
# ---------------------------------------------------------------------------
if ! command -v git >/dev/null 2>&1 || ! git -C "$REPO_ROOT" rev-parse --git-dir >/dev/null 2>&1; then
  echo "SKIP G7: not inside a git work-tree (manifest-only dry-run) — CI always runs this"
elif ! git -C "$REPO_ROOT" rev-parse -q --verify "refs/tags/$TAG" >/dev/null 2>&1; then
  echo "SKIP G7: tag '$TAG' object not present locally (pre-tag dry-run) — CI runs on the pushed tag so this always executes there"
else
  OBJ_TYPE="$(git -C "$REPO_ROOT" cat-file -t "$TAG" 2>/dev/null || echo "")"
  if [ "$OBJ_TYPE" != "tag" ]; then
    fail "G7: tag '$TAG' is a lightweight tag (object type '$OBJ_TYPE'), not an annotated tag. ADR-0025 D1 requires 'git tag -s' (annotated + signed)."
  fi
  # ADR-0025 D1: GPG- OR SSH-signed. `git verify-tag` auto-detects the
  # signature type; SSH signatures additionally need an allowed-signers file.
  # Passing it via `-c` is harmless for GPG-signed tags (ignored) and enables
  # SSH verification against the committed `.github/allowed_signers` (whose
  # principal is the tagger email).
  ALLOWED_SIGNERS="$REPO_ROOT/.github/allowed_signers"
  if ! git -C "$REPO_ROOT" -c gpg.ssh.allowedSignersFile="$ALLOWED_SIGNERS" \
        verify-tag "$TAG" >/dev/null 2>&1; then
    fail "G7: tag '$TAG' is not signature-verifiable ('git verify-tag' failed). ADR-0025 D1 requires a GPG- or SSH-signed tag; an SSH signer must be listed in .github/allowed_signers with a principal matching the tagger email."
  fi
  if [ "$LANE" = "beta" ]; then
    LANE_BRANCH="next"
  else
    LANE_BRANCH="main"
  fi
  REMOTE_REF="refs/remotes/origin/$LANE_BRANCH"
  if git -C "$REPO_ROOT" rev-parse -q --verify "$REMOTE_REF" >/dev/null 2>&1; then
    CHECK_REF="$REMOTE_REF"
  elif git -C "$REPO_ROOT" rev-parse -q --verify "refs/heads/$LANE_BRANCH" >/dev/null 2>&1; then
    CHECK_REF="refs/heads/$LANE_BRANCH"
  else
    fail "G7: lane branch '$LANE_BRANCH' not found locally (need origin/$LANE_BRANCH or $LANE_BRANCH). CI checks this on a full-history checkout."
  fi
  TAG_COMMIT="$(git -C "$REPO_ROOT" rev-list -n1 "$TAG")"
  if ! git -C "$REPO_ROOT" merge-base --is-ancestor "$TAG_COMMIT" "$CHECK_REF"; then
    fail "G7: tag '$TAG' (lane '$LANE') is NOT reachable from '$LANE_BRANCH'. ADR-0025 D6: beta tags must be on 'next', stable tags on 'main'."
  fi
  pass "G7: tag '$TAG' annotated+signed and reachable from '$LANE_BRANCH'"
fi

echo
echo "version gate PASSED for $TAG (lane: $LANE)"
