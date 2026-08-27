#!/usr/bin/env bash
# Runs scripts/stage-npm.sh against fixture binaries and checks the layout it
# produces (#511 follow-up).
#
# The script had no executable test at all — only a posture-lint grep
# comparing name lists across three files. A grep cannot catch a sed that
# stops stamping the version, a cp that drops the shim, or a missing-asset
# guard that has stopped guarding. Those would surface only at release time,
# inside a job that is `continue-on-error`.
#
# Everything happens inside a mktemp directory; the repository is read-only
# to this test.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"
WORK="$(mktemp -d)"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

BIN="$WORK/bin"
mkdir -p "$BIN"
for a in doiget-linux-x86_64 doiget-macos-aarch64 doiget-macos-x86_64 doiget-windows-x86_64.exe; do
  printf 'fake-binary-%s' "$a" > "$BIN/$a"
done

if ! bash "$ROOT/scripts/stage-npm.sh" 9.9.9 "$BIN" "$WORK/out" > "$WORK/log" 2>&1; then
  echo "FAIL  stage-npm.sh exited non-zero"
  cat "$WORK/log"
  exit 1
fi

fail=0
check() {
  if [ "$1" = "ok" ]; then
    echo "ok    $2"
  else
    echo "FAIL  $2"
    fail=1
  fi
}

for d in doiget doiget-darwin-arm64 doiget-darwin-x64 doiget-linux-x64 doiget-win32-x64; do
  if [ -d "$WORK/out/$d" ]; then check ok "staged $d"; else check no "staged $d"; fi
done

# Every manifest carries the stamped version, never the 0.0.0 placeholder.
for f in "$WORK/out"/*/package.json; do
  name="$(basename "$(dirname "$f")")"
  if grep -q '"0\.0\.0"' "$f"; then
    check no "version stamped in $name"
  else
    check ok "version stamped in $name"
  fi
done

if grep -q '"version": "9.9.9"' "$WORK/out/doiget/package.json"; then
  check ok "wrapper version is 9.9.9"
else
  check no "wrapper version is 9.9.9"
fi

# The wrapper pins its platform packages to the exact same version; a drift
# here publishes a wrapper that resolves nothing.
if grep -q '"doiget-linux-x64": "9.9.9"' "$WORK/out/doiget/package.json"; then
  check ok "optionalDependencies pinned to the same version"
else
  check no "optionalDependencies pinned to the same version"
fi

# Windows keeps the .exe suffix; the others must not.
if [ -f "$WORK/out/doiget-win32-x64/bin/doiget.exe" ]; then
  check ok "win32 binary is doiget.exe"
else
  check no "win32 binary is doiget.exe"
fi
if [ -f "$WORK/out/doiget-linux-x64/bin/doiget" ]; then
  check ok "linux binary is doiget"
else
  check no "linux binary is doiget"
fi

# The shim and the table it requires must both ship, or `npx doiget` throws
# MODULE_NOT_FOUND on first run.
for f in doiget.js platform.js; do
  if [ -f "$WORK/out/doiget/bin/$f" ]; then
    check ok "wrapper ships bin/$f"
  else
    check no "wrapper ships bin/$f"
  fi
done

# A missing release asset must fail loudly rather than stage a package with
# no binary in it.
#
# The exit code alone proves nothing: `set -euo pipefail` makes the `cp` after
# the guard fail too, so deleting the guard outright still exits non-zero —
# with a raw `cp: cannot stat` instead of the `::error::` annotation, and,
# since `mkdir`/`stamp_version` run before that `cp`, with a half-staged
# package left on disk. So assert on the annotation AND on the absence of the
# wreckage, never on the exit code alone.
rm -f "$BIN/doiget-linux-x86_64"
if bash "$ROOT/scripts/stage-npm.sh" 9.9.9 "$BIN" "$WORK/out2" > "$WORK/log2" 2>&1; then
  check no "a missing release asset fails the staging script"
else
  check ok "a missing release asset fails the staging script"
fi
if grep -q '::error::missing release asset' "$WORK/log2"; then
  check ok "the failure names the missing asset"
else
  check no "the failure names the missing asset"
  cat "$WORK/log2"
fi
if [ -e "$WORK/out2/doiget-linux-x64" ]; then
  check no "no half-staged package is left behind"
  find "$WORK/out2/doiget-linux-x64" -print
else
  check ok "no half-staged package is left behind"
fi

exit "$fail"
