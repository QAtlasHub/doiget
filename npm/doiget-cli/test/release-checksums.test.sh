#!/usr/bin/env bash
# Runs the npm-publish checksum-verification loop out of release-plz.yml
# against a synthetic download directory.
#
# That loop is the fix for the defect this cluster started from: the download
# step used `-p 'doiget-*.sha256'`, which also matched the SBOM's and the
# .mcpb's checksums, whose payloads are never downloaded — so `sha256sum -c`
# failed on every release, inside a job that is `continue-on-error`. It had
# no test at all: the one surface where a silent failure had already happened
# was verified by prose.
#
# The loop is extracted from the workflow rather than copied, so this test
# cannot drift into agreeing with a stale duplicate of it.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"
WORKFLOW="$ROOT/.github/workflows/release-plz.yml"
WORK="$(mktemp -d)"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

fail=0
check() {
  if [ "$1" = "ok" ]; then echo "ok    $2"; else echo "FAIL  $2"; fail=1; fi
}

# Pull the `for f in *.sha256` block out of the workflow and strip its YAML
# indentation. If the block is renamed or removed, this fails loudly rather
# than silently testing nothing.
awk '/^ *verified=0$/{on=1} on{print} /expected 4/{tail=1} tail && /^ *fi$/{exit}' "$WORKFLOW" | sed 's/^ \{0,10\}//' > "$WORK/loop.sh"
if [ ! -s "$WORK/loop.sh" ] || ! grep -q 'expected 4' "$WORK/loop.sh"; then
  echo "FAIL  could not extract the checksum loop from $WORKFLOW"
  exit 1
fi

# The four platform binaries a release actually publishes.
ASSETS="doiget-linux-x86_64 doiget-macos-aarch64 doiget-macos-x86_64 doiget-windows-x86_64.exe"

seed() {
  local dir="$1"
  mkdir -p "$dir"
  for a in $ASSETS; do
    printf 'fake-binary-%s' "$a" > "$dir/$a"
    ( cd "$dir" && sha256sum "$a" > "$a.sha256" )
  done
}

run_loop() {
  ( cd "$1" && bash "$WORK/loop.sh" > "$1/out" 2>&1; echo $? )
}

# 1. A clean release verifies all four and exits 0.
seed "$WORK/clean"
rc="$(run_loop "$WORK/clean")"
if [ "$rc" = "0" ]; then
  check ok "a complete release verifies"
else
  check no "a complete release verifies"
  cat "$WORK/clean/out"
fi

# 2. A checksum whose payload was never downloaded must fail. This is the
#    exact shape of the original bug: the SBOM's .sha256 arrived, its
#    payload did not.
seed "$WORK/orphan"
sed 's/doiget-linux-x86_64/doiget-sbom.spdx.json/' "$WORK/orphan/doiget-linux-x86_64.sha256" \
  > "$WORK/orphan/doiget-sbom.spdx.json.sha256"
rc="$(run_loop "$WORK/orphan")"
if [ "$rc" != "0" ]; then
  check ok "an orphaned checksum fails the job"
else
  check no "an orphaned checksum fails the job"
  cat "$WORK/orphan/out"
fi

# 3. A release missing one platform binary must fail on the count rather
#    than pass quietly having verified three. That is what `-ne 4` is for.
seed "$WORK/short"
rm -f "$WORK/short/doiget-macos-aarch64" "$WORK/short/doiget-macos-aarch64.sha256"
rc="$(run_loop "$WORK/short")"
if [ "$rc" != "0" ]; then
  check ok "a missing platform binary fails the count"
else
  check no "a missing platform binary fails the count"
  cat "$WORK/short/out"
fi
if grep -q 'expected 4' "$WORK/short/out"; then
  check ok "the count failure says what was expected"
else
  check no "the count failure says what was expected"
  cat "$WORK/short/out"
fi

# 4. A tampered binary must fail verification.
seed "$WORK/corrupt"
printf 'tampered' > "$WORK/corrupt/doiget-linux-x86_64"
rc="$(run_loop "$WORK/corrupt")"
if [ "$rc" != "0" ]; then
  check ok "a corrupted binary fails verification"
else
  check no "a corrupted binary fails verification"
  cat "$WORK/corrupt/out"
fi

exit "$fail"
