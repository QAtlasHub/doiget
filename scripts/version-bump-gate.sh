#!/usr/bin/env bash
# version-bump-gate.sh — PR-time version-bump gate (ADR-0033).
#
# This is the THIRD, distinct version-enforcement layer in the project:
#   1. scripts/release-version-gate.sh  — TAG time (ADR-0025 D2, G0–G7): is THIS
#      tag releasable? (manifest/lock/CHANGELOG/crates.io/signature/lane).
#   2. .github/workflows/version-check.yml — ADVISORY (ADR-0025 Amendment 6):
#      "would tagging the current version release?" — non-blocking visibility.
#   3. THIS — PR time (ADR-0033): did THIS PR *advance* the version correctly,
#      per the lane cadence? Designed to be a BLOCKING required check.
#
# Why a separate layer: the tag gate runs only on a pushed tag, and version-check
# is advisory and compares against crates.io (which has no published betas), so
# nothing ever forced a PR to bump beyond the *previous PR's* version. That gap
# let `next` roll 0.7.1-beta.0 → 0.7.2-beta.0 → 0.7.2-beta.1 with no rule, and
# left `next` carrying a base (0.7.2) that is a +2 *skip* over the 0.7.0 stable —
# not even promotable under a single-step rule. ADR-0033 closes the gap.
#
# Rules enforced (no labels, no exceptions — ADR-0033):
#   * PR → next : version is X.Y.Z-beta.N AND strictly greater than origin/next.
#                 - same base  : N == origin/next.N + 1   (strict +1 cadence).
#                 - base moved : a "retarget" — the new base MUST be a valid +1
#                                single-component step over the current stable
#                                and the counter MUST reset to beta.1.
#                 - in BOTH cases the base must be a +1 single-step over
#                   origin/main (keeps `next` always promotable; forces a
#                   retarget after every promotion).
#   * PR → main : promotion only — head branch MUST be `next` (same repo), the
#                 version MUST be a clean X.Y.Z, and exactly a +1 single-
#                 component step (major|minor|patch) over origin/main. No skips.
#   * The ONLY exempt PR is the automated main → next back-merge (it keeps
#     next's -beta.N): recognised by branch SHAPE (head == main, same repo),
#     never by a label.
#
# House style mirrors scripts/release-version-gate.sh: dependency-light bash
# (no jq / python / toml tooling), one actionable `::error::` per failure, abort
# on the first failure. Inputs come from the workflow as env vars.
#
# Inputs (env):
#   BASE_REF    target branch of the PR (next|main)             [required]
#   HEAD_REF    PR source branch name                           [required for main / backmerge]
#   CROSS_REPO  "true" when the PR head repo != base repo        [default: false]
# Reads:
#   ./Cargo.toml                 the PROPOSED [workspace.package].version (PR head)
#   origin/next, origin/main     current lane versions (the workflow fetches them)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
CARGO_TOML="$REPO_ROOT/Cargo.toml"

fail() { echo "::error::$*" >&2; exit 1; }
pass() { echo "PASS $*"; }

BASE_REF="${BASE_REF:-}"
HEAD_REF="${HEAD_REF:-}"
CROSS_REPO="${CROSS_REPO:-false}"
[ -n "$BASE_REF" ] || fail "version-bump: BASE_REF (PR target branch) not set"

# ---------------------------------------------------------------------------
# Helpers.
# ---------------------------------------------------------------------------

# First `version = "..."` inside [workspace.package] (awk; no toml tooling —
# identical extraction to release-version-gate.sh G1). Reads stdin.
read_wp_version() {
  awk '
    /^\[workspace\.package\]/ { in_wp = 1; next }
    /^\[/ { in_wp = 0 }
    in_wp && /^[[:space:]]*version[[:space:]]*=/ {
      line = $0; sub(/^[^"]*"/, "", line); sub(/".*$/, "", line); print line; exit
    }'
}

# Version at a git ref (empty if the ref or file is absent — caller checks).
git_ref_version() {
  local out=""
  out="$(git -C "$REPO_ROOT" show "$1:Cargo.toml" 2>/dev/null || true)"
  printf '%s' "$out" | read_wp_version
}

# Strict parse — the ONLY forms doiget uses: X.Y.Z or X.Y.Z-beta.N. Sets the
# globals _MAJ _MIN _PAT _ISPRE (0/1) _PREN (beta number; -1 when stable).
# Returns 1 on any other shape (e.g. -rc.N, +build, missing component).
parse_version() {
  local v="$1"
  if [[ "$v" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]]; then
    _MAJ="${BASH_REMATCH[1]}"; _MIN="${BASH_REMATCH[2]}"; _PAT="${BASH_REMATCH[3]}"
    _ISPRE=0; _PREN=-1
  elif [[ "$v" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)-beta\.([0-9]+)$ ]]; then
    _MAJ="${BASH_REMATCH[1]}"; _MIN="${BASH_REMATCH[2]}"; _PAT="${BASH_REMATCH[3]}"
    _ISPRE=1; _PREN="${BASH_REMATCH[4]}"
  else
    return 1
  fi
}

# Compare X.Y.Z cores: echo 0 if a>b, 1 if a==b, 2 if a<b.
core_cmp3() {
  if [ "$1" -gt "$4" ]; then echo 0; return; fi
  if [ "$1" -lt "$4" ]; then echo 2; return; fi
  if [ "$2" -gt "$5" ]; then echo 0; return; fi
  if [ "$2" -lt "$5" ]; then echo 2; return; fi
  if [ "$3" -gt "$6" ]; then echo 0; return; fi
  if [ "$3" -lt "$6" ]; then echo 2; return; fi
  echo 1
}

# Is candidate X.Y.Z exactly one +1 step (with zero-resets) over stable A.B.C?
# args: cMAJ cMIN cPAT sMAJ sMIN sPAT  → return 0 (yes) / 1 (no)
is_single_step() {
  local cM=$1 cm=$2 cp=$3 sM=$4 sm=$5 sp=$6
  [ "$cM" -eq "$sM" ] && [ "$cm" -eq "$sm" ] && [ "$cp" -eq $((sp + 1)) ] && return 0   # patch+1
  [ "$cM" -eq "$sM" ] && [ "$cm" -eq $((sm + 1)) ] && [ "$cp" -eq 0 ] && return 0       # minor+1
  [ "$cM" -eq $((sM + 1)) ] && [ "$cm" -eq 0 ] && [ "$cp" -eq 0 ] && return 0           # major+1
  return 1
}

# Human-readable list of the allowed steps over a stable core (for error text).
allowed_steps() { echo "$1.$2.$(($3 + 1)) (patch), $1.$(($2 + 1)).0 (minor), $(($1 + 1)).0.0 (major)"; }

# ---------------------------------------------------------------------------
# Read the proposed (PR head) version.
# ---------------------------------------------------------------------------
[ -f "$CARGO_TOML" ] || fail "version-bump: $CARGO_TOML not found"
HEAD_V="$(read_wp_version < "$CARGO_TOML")"
[ -n "$HEAD_V" ] || fail "version-bump: could not read [workspace.package].version from Cargo.toml"
parse_version "$HEAD_V" \
  || fail "version-bump: head version '$HEAD_V' is not X.Y.Z or X.Y.Z-beta.N — doiget uses only the 'beta' pre-release identifier (ADR-0033 / ADR-0025 D3)"
hMAJ=$_MAJ; hMIN=$_MIN; hPAT=$_PAT; hISPRE=$_ISPRE; hPREN=$_PREN

echo "version-bump gate (ADR-0033): PR head version='$HEAD_V', base='$BASE_REF', head-branch='${HEAD_REF:-?}', cross-repo='$CROSS_REPO'"

case "$BASE_REF" in
  next)
    # -- structural carve-out: the automated main → next back-merge keeps next's
    #    -beta.N (ADR-0025 D6 / Amendment 3). It is NOT a feature PR; recognised
    #    by branch shape (head == canonical main, same repo), never a label. --
    if [ "$HEAD_REF" = "main" ] && [ "$CROSS_REPO" != "true" ]; then
      pass "main → next sync (back-merge) — version bump intentionally not required (ADR-0025 D6 / ADR-0033)"
      exit 0
    fi

    [ "$hISPRE" = "1" ] \
      || fail "version-bump: a PR to 'next' must carry a beta version X.Y.Z-beta.N (the beta lane); got '$HEAD_V' (ADR-0025 D6.2)"

    NEXT_V="$(git_ref_version origin/next)"
    [ -n "$NEXT_V" ] || fail "version-bump: could not read Cargo.toml version at origin/next — the workflow must fetch it (fetch-depth: 0 + refs/heads/next)"
    MAIN_V="$(git_ref_version origin/main)"
    [ -n "$MAIN_V" ] || fail "version-bump: could not read Cargo.toml version at origin/main — the workflow must fetch it (refs/heads/main)"
    parse_version "$NEXT_V" || fail "version-bump: origin/next version '$NEXT_V' is malformed"
    nMAJ=$_MAJ; nMIN=$_MIN; nPAT=$_PAT; nPREN=$_PREN
    parse_version "$MAIN_V" || fail "version-bump: origin/main version '$MAIN_V' is malformed (main must be a clean stable X.Y.Z)"
    sMAJ=$_MAJ; sMIN=$_MIN; sPAT=$_PAT

    # The base (X.Y.Z) must always be a +1 single-step over the current stable,
    # so `next` is ALWAYS promotable and a retarget is forced after a promotion.
    if ! is_single_step "$hMAJ" "$hMIN" "$hPAT" "$sMAJ" "$sMIN" "$sPAT"; then
      fail "version-bump: next base $hMAJ.$hMIN.$hPAT is not a +1 single-component step over the current stable $MAIN_V (origin/main). Allowed: $(allowed_steps "$sMAJ" "$sMIN" "$sPAT"). After a promotion you MUST retarget next before landing more betas. (ADR-0033)"
    fi

    ccmp="$(core_cmp3 "$hMAJ" "$hMIN" "$hPAT" "$nMAJ" "$nMIN" "$nPAT")"
    if [ "$ccmp" = "2" ]; then
      fail "version-bump: head base $hMAJ.$hMIN.$hPAT REGRESSES below origin/next $nMAJ.$nMIN.$nPAT — next must only move forward (ADR-0033)"
    elif [ "$ccmp" = "1" ]; then
      # Same base → strict beta.N + 1.
      [ "$hPREN" -eq $((nPREN + 1)) ] \
        || fail "version-bump: a PR to 'next' must bump beta by EXACTLY +1 — expected $hMAJ.$hMIN.$hPAT-beta.$((nPREN + 1)) (origin/next is $NEXT_V), got $HEAD_V (ADR-0033 strict cadence)"
      pass "version-bump: $HEAD_V is strict beta+1 over origin/next ($NEXT_V); base is a valid step over stable $MAIN_V"
    else
      # Base moved UP → retarget. Counter resets to beta.1.
      [ "$hPREN" -eq 1 ] \
        || fail "version-bump: a base retarget ($nMAJ.$nMIN.$nPAT → $hMAJ.$hMIN.$hPAT) must reset the counter to -beta.1; got -beta.$hPREN (ADR-0033)"
      pass "version-bump: retarget $nMAJ.$nMIN.$nPAT → $hMAJ.$hMIN.$hPAT-beta.1 (valid +1 step over stable $MAIN_V); cadence reset"
    fi
    ;;

  main)
    # Promotion only. main NEVER takes a direct PR — stable fixes also route
    # through next + promotion (ADR-0033 retires ADR-0025 D6 rule 4's direct
    # hotfix). No labels, no exceptions.
    if [ "$CROSS_REPO" = "true" ]; then
      fail "version-bump: 'main' accepts promotions only from THIS repo's 'next' branch — a fork PR to main is rejected (ADR-0033)"
    fi
    if [ "$HEAD_REF" != "next" ]; then
      fail "version-bump: 'main' accepts PRs ONLY from 'next' (got head '$HEAD_REF'). There is no direct-to-main path — stable hotfixes also go via next + promotion (ADR-0033 retires ADR-0025 D6 rule 4)."
    fi
    if [ "$hISPRE" != "0" ]; then
      fail "version-bump: a promotion to 'main' (stable) must carry a CLEAN X.Y.Z — strip the -beta.N before opening the next → main PR; got '$HEAD_V' (ADR-0025 D6.2)"
    fi
    MAIN_V="$(git_ref_version origin/main)"
    [ -n "$MAIN_V" ] || fail "version-bump: could not read Cargo.toml version at origin/main — the workflow must fetch it (fetch-depth: 0 + refs/heads/main)"
    parse_version "$MAIN_V" || fail "version-bump: origin/main version '$MAIN_V' is malformed (main must be a clean stable X.Y.Z)"
    sMAJ=$_MAJ; sMIN=$_MIN; sPAT=$_PAT
    if ! is_single_step "$hMAJ" "$hMIN" "$hPAT" "$sMAJ" "$sMIN" "$sPAT"; then
      fail "version-bump: promotion must bump the stable line by EXACTLY +1 (major|minor|patch) over origin/main ($MAIN_V) — NO skips. Allowed: $(allowed_steps "$sMAJ" "$sMIN" "$sPAT"). Got $HEAD_V. (ADR-0033)"
    fi
    pass "version-bump: promotion $MAIN_V → $HEAD_V is a valid +1 single-component step (ADR-0033)"
    ;;

  *)
    # Not a release lane — the workflow only triggers on next|main, so this is a
    # defensive no-op rather than a failure.
    echo "version-bump: base '$BASE_REF' is not a release lane (next|main) — gate not applicable; PASS"
    ;;
esac

echo
echo "version-bump gate PASSED ($HEAD_V → base '$BASE_REF')"
