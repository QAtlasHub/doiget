#!/usr/bin/env bash
# Offline tests for scripts/update-homebrew-formula.sh (#501).
#
# The generator is what stands between "the formula is right" and "somebody
# remembered to edit it", so the properties that would silently ship a broken
# install are asserted here rather than discovered by a `brew install`.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GEN="$ROOT/scripts/update-homebrew-formula.sh"
TMP="$(mktemp -d)"
# Named files only, never a recursive delete of a variable path.
cleanup() { rm -f "$TMP/doiget.rb" "$TMP/doiget-tag.rb" "$TMP/bad.rb" "$TMP/regen.rb"; rmdir "$TMP" 2>/dev/null || true; }
trap cleanup EXIT

A=$(printf 'a%.0s' $(seq 64))
B=$(printf 'b%.0s' $(seq 64))
C=$(printf 'c%.0s' $(seq 64))

fail=0
check() {
  if [ "$2" != "$3" ]; then
    echo "FAIL: $1: expected '$3', got '$2'"; fail=1
  else
    echo "ok: $1"
  fi
}

# ---------------------------------------------------------------- generation
OUT="$TMP/doiget.rb"
DOIGET_FORMULA_OUT="$OUT" bash "$GEN" 1.2.3 "$A" "$B" "$C" >/dev/null

check "version is set" "$(grep -c '^  version "1.2.3"$' "$OUT")" "1"
check "all three platform urls" "$(grep -c 'releases/download/v1.2.3/doiget-' "$OUT")" "3"
check "all three checksums" "$(grep -c '^      sha256 "' "$OUT")" "3"
check "arm checksum placed under on_arm" \
  "$(awk '/on_arm do/{f=1} f&&/sha256/{print substr($2,2,3); exit}' "$OUT")" "aaa"

# A leading `v` is how the TAG is written. Accepting it silently and emitting
# `version "v1.2.3"` would produce a formula that installs but compares wrong.
OUT2="$TMP/doiget-tag.rb"
DOIGET_FORMULA_OUT="$OUT2" bash "$GEN" v1.2.3 "$A" "$B" "$C" >/dev/null
check "a leading v is normalised" "$(cmp -s "$OUT" "$OUT2" && echo same || echo differs)" "same"

# The `#{...}` in the `test do` block is Ruby interpolation and must survive
# the shell heredoc intact. It leaked out backslash-escaped once.
check "no leaked backslash before ruby interpolation" \
  "$(grep -c '[\]#{' "$OUT")" "0"
check "ruby interpolation present" "$(grep -c 'doiget #{version}' "$OUT")" "1"

# ------------------------------------------------------------- sha validation
# A truncated or missing checksum must stop the generator, not produce a
# formula that fails at `brew install` on someone else's machine.
for bad in "" "notahex" "$(printf 'a%.0s' $(seq 63))" "$A$A"; do
  if DOIGET_FORMULA_OUT="$TMP/bad.rb" bash "$GEN" 1.2.3 "$bad" "$B" "$C" >/dev/null 2>&1; then
    echo "FAIL: generator accepted a bad sha256: '$bad'"; fail=1
  fi
done
echo "ok: bad checksums are refused"

# ------------------------------------------------ the shipped formula is fresh
# The formula in the tree must be something the generator would produce, so a
# hand-edit is caught rather than merged.
STABLE="$(grep -oE '^  version "[^"]+"' "$ROOT/Formula/doiget.rb" | grep -oE '[0-9][^"]*')"
SHIPPED_SHAS=$(grep -oE '^      sha256 "[0-9a-f]{64}"' "$ROOT/Formula/doiget.rb" | grep -oE '[0-9a-f]{64}')
# shellcheck disable=SC2086
DOIGET_FORMULA_OUT="$TMP/regen.rb" bash "$GEN" "$STABLE" $SHIPPED_SHAS >/dev/null
check "Formula/doiget.rb is generator output, not hand-edited" \
  "$(cmp -s "$ROOT/Formula/doiget.rb" "$TMP/regen.rb" && echo same || echo differs)" "same"

if [ "$fail" -ne 0 ]; then
  echo "update-homebrew-formula tests FAILED"; exit 1
fi
echo "update-homebrew-formula tests passed"
