#!/usr/bin/env bash
# Exercises G5 of the release version gate against crafted CHANGELOGs
# (ADR-0025 D2-G5 + Amendment 7).
#
# G5 is the #164 fix — "nothing ships without notes a human wrote" — and it had
# no test at all. Amendment 7 relaxes it on the BETA lane only, so the case that
# matters most here is the one asserting the relaxation does NOT leak to stable.
#
# Runs the real script, not a copy of its logic, inside a throwaway `git
# worktree` so the working tree is never touched. `--offline-skip-crates-io`
# skips G3/G4 only; G0-G2, G5 and G6 all run for real, which is why each case
# rewrites the manifest version and refreshes Cargo.lock: G1 demands
# tag == manifest and G2 demands the lock be in sync, and both run before G5.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
TMP="$(mktemp -d)"
WT="$TMP/wt"
CARGO_BIN="${CARGO:-cargo}"

cleanup() {
  git -C "$ROOT" worktree remove --force "$WT" > /dev/null 2>&1 || true
  rm -rf "$TMP"
}
trap cleanup EXIT

git -C "$ROOT" worktree add --detach "$WT" HEAD > /dev/null 2>&1
# The worktree carries HEAD, so it would run the LAST COMMITTED gate and
# silently pass over an uncommitted change to the very thing under test. Copy
# the live script in. It is still the real script -- only relocated, so its
# REPO_ROOT resolves to the fixture rather than to this repository.
cp "$HERE/release-version-gate.sh" "$WT/scripts/release-version-gate.sh"

fail=0
check() {
  if [ "$1" = "ok" ]; then echo "ok    $2"; else echo "FAIL  $2"; fail=1; fi
}

# Point the worktree's manifest at $1 and bring Cargo.lock along, so G1 and G2
# pass and G5 is actually reached.
set_version() {
  local v="$1" cur
  cur="$(grep -oE '^version       = "[^"]+"' "$WT/Cargo.toml" | head -1 | awk -F'"' '{print $2}')"
  sed -i "s/^version       = \"$cur\"/version       = \"$v\"/" "$WT/Cargo.toml"
  sed -i "s/version = \"$cur\" }/version = \"$v\" }/g" "$WT/Cargo.toml"
  ( cd "$WT" && "$CARGO_BIN" metadata --format-version 1 > /dev/null )
}

# $1 version, $2 tag, $3 CHANGELOG body. Prints the gate's output.
run_gate() {
  set_version "$1"
  printf '%s' "$3" > "$WT/CHANGELOG.md"
  ( cd "$WT" && bash scripts/release-version-gate.sh "$2" --offline-skip-crates-io 2>&1 ) || true
}

NOTES='# Changelog

## [Unreleased]

- **[core]** something a human wrote.

## [0.8.11] - 2026-08-27

- shipped.
'

EMPTY_UNRELEASED='# Changelog

## [Unreleased]

## [0.8.11] - 2026-08-27

- shipped.
'

EXPLICIT_BETA='# Changelog

## [Unreleased]

- accumulating.

## [9.9.9-beta.1] - 2026-08-27

- the maintainer wrote a per-beta section.

## [0.8.11] - 2026-08-27

- shipped.
'

NO_HEADINGS='# Changelog

## [0.8.11] - 2026-08-27

- shipped.
'

STABLE_SECTION='# Changelog

## [Unreleased]

## [9.9.9] - 2026-08-27

- the curated section for this release.
'

STABLE_ONLY_UNRELEASED='# Changelog

## [Unreleased]

- notes that belong to the NEXT release, not this one.

## [0.8.11] - 2026-08-27

- shipped.
'

# --- beta lane -------------------------------------------------------------

out="$(run_gate 9.9.9-beta.1 v9.9.9-beta.1 "$NOTES")"
if printf '%s' "$out" | grep -q 'PASS G5: CHANGELOG.md has a non-empty \[Unreleased\] section'; then
  check ok "beta: a non-empty [Unreleased] satisfies G5"
else
  check no "beta: a non-empty [Unreleased] satisfies G5"
  printf '%s\n' "$out" | tail -4
fi

out="$(run_gate 9.9.9-beta.1 v9.9.9-beta.1 "$EMPTY_UNRELEASED")"
if printf '%s' "$out" | grep -q "G5:.*\[Unreleased\]' is empty"; then
  check ok "beta: an EMPTY [Unreleased] still fails G5"
else
  check no "beta: an EMPTY [Unreleased] still fails G5"
  printf '%s\n' "$out" | tail -4
fi

out="$(run_gate 9.9.9-beta.1 v9.9.9-beta.1 "$EXPLICIT_BETA")"
if printf '%s' "$out" | grep -q 'PASS G5: CHANGELOG.md has a non-empty \[9.9.9-beta.1\] section'; then
  check ok "beta: an explicit per-beta section wins over [Unreleased]"
else
  check no "beta: an explicit per-beta section wins over [Unreleased]"
  printf '%s\n' "$out" | tail -4
fi

out="$(run_gate 9.9.9-beta.1 v9.9.9-beta.1 "$NO_HEADINGS")"
if printf '%s' "$out" | grep -q 'G5:.*neither'; then
  check ok "beta: neither heading fails G5"
else
  check no "beta: neither heading fails G5"
  printf '%s\n' "$out" | tail -4
fi

# --- stable lane: the relaxation must NOT reach here ------------------------

out="$(run_gate 9.9.9 v9.9.9 "$STABLE_SECTION")"
if printf '%s' "$out" | grep -q 'PASS G5: CHANGELOG.md has a non-empty \[9.9.9\] section'; then
  check ok "stable: the curated [X.Y.Z] section satisfies G5"
else
  check no "stable: the curated [X.Y.Z] section satisfies G5"
  printf '%s\n' "$out" | tail -4
fi

# The load-bearing one. If Amendment 7 ever leaks into the stable lane, a
# release ships carrying the previous release's notes and nothing of its own —
# which is #164, the defect G5 exists to prevent.
out="$(run_gate 9.9.9 v9.9.9 "$STABLE_ONLY_UNRELEASED")"
if printf '%s' "$out" | grep -q "G5: CHANGELOG.md has no '## \[9.9.9\]' section"; then
  check ok "stable: [Unreleased] does NOT satisfy G5"
else
  check no "stable: [Unreleased] does NOT satisfy G5"
  printf '%s\n' "$out" | tail -4
fi

exit "$fail"
