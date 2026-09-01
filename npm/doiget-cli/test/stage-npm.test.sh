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
WORKFLOW="$ROOT/.github/workflows/release-plz.yml"
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

for d in doiget-cli doiget-darwin-arm64 doiget-darwin-x64 doiget-linux-x64 doiget-win32-x64; do
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

if grep -q '"version": "9.9.9"' "$WORK/out/doiget-cli/package.json"; then
  check ok "wrapper version is 9.9.9"
else
  check no "wrapper version is 9.9.9"
fi

# The wrapper pins its platform packages to the exact same version; a drift
# here publishes a wrapper that resolves nothing.
if grep -q '"doiget-linux-x64": "9.9.9"' "$WORK/out/doiget-cli/package.json"; then
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
  if [ -f "$WORK/out/doiget-cli/bin/$f" ]; then
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

# Every path the publish step hands to `npm publish` must be one npm reads as
# a directory. `npm-stage/doiget-darwin-arm64` is two segments with no leading
# `./`, which is exactly npm's `owner/repo` shorthand for a GitHub dependency,
# so npm never looks at the directory. That is how the v0.8.11 npm publish
# died: `EALLOWGIT: Refusing to fetch "github:npm-stage/doiget-darwin-arm64"`,
# before any registry call, in a job that is `continue-on-error`.
#
# The specs are read out of the workflow and used VERBATIM, from a directory
# laid out the way the release job lays it out. Rewriting them to absolute
# paths first would destroy the only property under test: npm accepts an
# absolute path either way, and the shorthand collision is a fact about the
# literal spelling.
#
# `--dry-run` needs no credentials and, on a real directory, no registry
# round-trip either: it packs locally and reports what it would upload. A spec
# npm reads as a GitHub shorthand fails instead, which is the case being
# detected. `GIT_TERMINAL_PROMPT=0` keeps that failure from stopping to ask
# for credentials on a machine that has a terminal.
#
# Skipped when npm is absent, so this file stays runnable outside CI.
if command -v npm > /dev/null 2>&1; then
  export GIT_TERMINAL_PROMPT=0
  cp -r "$WORK/out" "$WORK/npm-stage"
  # A `npm-stage/doiget-*` glob is banned outright. It matches the WRAPPER --
  # `doiget-cli` starts with `doiget-` too -- so the wrapper is published
  # inside the loop and again on the explicit line after it. v0.8.12's release
  # job died on `You cannot publish over the previously published versions:
  # 0.8.12`, after everything had already shipped: red job, complete release.
  # It also sorted first, so the wrapper went out ahead of the packages its
  # optionalDependencies pin.
  #
  # This is checked as a shape, not as a duplicate count, because the glob is
  # the thing that is wrong. The same trap was spotted and excluded in
  # posture-lint's `find -name doiget-*` in the very PR that renamed the
  # wrapper, and missed here.
  if grep -qE 'npm-stage/doiget-\*' "$WORKFLOW"; then
    check no "the publish step does not glob npm-stage/doiget-*"
    grep -nE 'npm-stage/doiget-\*' "$WORKFLOW" | sed 's/^/  /'
  else
    check ok "the publish step does not glob npm-stage/doiget-*"
  fi

  # The platform list the loop actually iterates, from the same source it reads.
  #
  # `|| true` is load-bearing, not defensive noise. Under `set -euo pipefail` a
  # bare assignment takes the exit status of its command substitution, so a
  # `grep` that matches nothing kills the script HERE -- before the `check no`
  # written two lines down to report exactly that. The diagnostic was
  # unreachable in the one case it exists for, and CI would show a bare
  # non-zero exit with none of the ok/FAIL lines around it.
  looped="$(grep -oE '^doiget-[a-z0-9-]+:' "$ROOT/scripts/stage-npm.sh" | tr -d ':' | sort -u | sed 's#^#./npm-stage/#' || true)"
  direct="$(grep -oE 'npm publish [^ "$]+' "$WORKFLOW" | awk '{print $3}' || true)"
  if [ -z "$looped" ] || [ -z "$direct" ]; then
    check no "found the npm publish invocations in release-plz.yml"
  else
    check ok "found the npm publish invocations in release-plz.yml"
  fi

  all_specs="$(printf '%s
%s
' "$looped" "$direct" | sed '/^$/d' | sed 's#^\./##')"
  dupes="$(printf '%s
' "$all_specs" | sort | uniq -d)"
  if [ -n "$dupes" ]; then
    check no "no package is published twice"
    printf '  duplicated: %s
' "$dupes"
  else
    check ok "no package is published twice"
  fi

  for spec in $looped $direct; do
    # Expand the glob where the release job would expand it.
    entries="$(cd "$WORK" && printf '%s\n' $spec)"
    for d in $entries; do
      if ! ( cd "$WORK" && [ -d "$d" ] ); then
        check no "publish spec $d resolves to a staged directory"
        continue
      fi
      if ( cd "$WORK" && npm publish --dry-run "$d" ) > "$WORK/dry" 2>&1; then
        check ok "npm reads $d as a directory"
      else
        check no "npm reads $d as a directory"
        tail -4 "$WORK/dry"
      fi
    done
  done
else
  echo "skip  npm not on PATH; publish-spec check not run"
fi

exit "$fail"
