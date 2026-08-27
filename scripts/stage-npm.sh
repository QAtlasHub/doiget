#!/usr/bin/env bash
# stage-npm.sh — assemble the npm packages for a release (#511).
#
#   scripts/stage-npm.sh <version> <bindir> <outdir>
#
# <bindir> holds the release assets as downloaded from the GitHub Release
# (`doiget-linux-x86_64`, `doiget-macos-aarch64`, `doiget-macos-x86_64`,
# `doiget-windows-x86_64.exe`). <outdir> is created and filled with one
# directory per package, each ready for `npm publish`.
#
# Layout, and why: the wrapper `doiget` declares the four platform packages
# as `optionalDependencies`, so npm resolves exactly the one matching the
# host and skips the rest. There is deliberately NO postinstall download —
# see `npm/doiget/bin/doiget.js` for the four reasons.
#
# The asset names use x86_64/aarch64; npm's `cpu` field uses x64/arm64. That
# mapping lives HERE and in `npm/doiget/bin/doiget.js`, and the
# `npm platform packages cover every release binary` step in
# `.github/workflows/posture-lint.yml` fails if they drift.
#
# Dependency-light bash on purpose, matching scripts/build-mcpb.sh.

set -euo pipefail

VERSION="${1:?usage: stage-npm.sh <version> <bindir> <outdir>}"
BINDIR="${2:?usage: stage-npm.sh <version> <bindir> <outdir>}"
OUTDIR="${3:?usage: stage-npm.sh <version> <bindir> <outdir>}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
SRC="$REPO_ROOT/npm"

[ -d "$SRC" ] || { echo "::error::npm/ template directory not found at $SRC" >&2; exit 1; }

# Refuse to reuse a stale staging directory rather than deleting one the
# caller may not have meant to name.
[ -e "$OUTDIR" ] && { echo "::error::$OUTDIR already exists; remove it or pass a fresh path" >&2; exit 1; }
mkdir -p "$OUTDIR"

# pkg-name : release-asset : binary-name-inside-the-package
MAP="
doiget-darwin-arm64:doiget-macos-aarch64:doiget
doiget-darwin-x64:doiget-macos-x86_64:doiget
doiget-linux-x64:doiget-linux-x86_64:doiget
doiget-win32-x64:doiget-windows-x86_64.exe:doiget.exe
"

# `0.0.0` is the placeholder in every template manifest; the release stamps
# the real version so nothing in the tree is bumped by hand and drift is
# impossible. `mcpb/manifest.json` uses the same trick via jq.
stamp_version() {
  sed "s/\"0\.0\.0\"/\"$VERSION\"/g" "$1" > "$2"
}

echo "$MAP" | while IFS=: read -r pkg asset binname; do
  [ -n "$pkg" ] || continue
  [ -f "$BINDIR/$asset" ] || { echo "::error::missing release asset $BINDIR/$asset for $pkg" >&2; exit 1; }
  mkdir -p "$OUTDIR/$pkg/bin"
  stamp_version "$SRC/$pkg/package.json" "$OUTDIR/$pkg/package.json"
  cp "$BINDIR/$asset" "$OUTDIR/$pkg/bin/$binname"
  chmod +x "$OUTDIR/$pkg/bin/$binname"
  echo "staged $pkg  <- $asset"
done

mkdir -p "$OUTDIR/doiget/bin"
stamp_version "$SRC/doiget/package.json" "$OUTDIR/doiget/package.json"
# Every file under bin/, not just the entry point: the shim `require`s
# `platform.js`, and copying only `doiget.js` publishes a package that
# throws MODULE_NOT_FOUND on first run. Caught by
# `npm/doiget/test/stage-npm.test.sh`.
cp "$SRC"/doiget/bin/*.js "$OUTDIR/doiget/bin/"
cp "$REPO_ROOT/crates/doiget-cli/README.md" "$OUTDIR/doiget/README.md"
echo "staged doiget (wrapper)"

echo
echo "npm packages staged in $OUTDIR at version $VERSION"
