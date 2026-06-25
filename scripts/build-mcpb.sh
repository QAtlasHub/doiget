#!/usr/bin/env bash
# build-mcpb.sh — assemble the doiget Claude Desktop Extension (.mcpb).
#
# A `.mcpb` is a zip of `manifest.json` + `server/<per-platform binaries>` that
# Claude Desktop one-click-installs and launches over stdio (`doiget serve`).
# This is a SEPARATE distribution channel from the MCP Registry (`server.json`):
# the Desktop Extensions directory (Settings > Extensions in Claude Desktop).
#
# Usage:
#   scripts/build-mcpb.sh <version> <bindir> [out.mcpb]
#
# <bindir> must contain the release binaries under their GitHub Release asset
# names:
#   doiget-linux-x86_64  doiget-macos-aarch64  doiget-macos-x86_64  doiget-windows-x86_64.exe
#
# Requirements: the `mcpb` CLI (`npm install -g @anthropic-ai/mcpb`), `jq`, and
# `lipo` for the universal macOS binary — run on macOS (the release CI
# `desktop-extension` job does). Without `lipo` the bundle ships arm64-only macOS.
set -euo pipefail

VERSION="${1:?usage: build-mcpb.sh <version> <bindir> [out.mcpb]}"
BINDIR="${2:?usage: build-mcpb.sh <version> <bindir> [out.mcpb]}"
OUT="${3:-doiget-${VERSION}.mcpb}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
STAGE="$(mktemp -d)/doiget"
mkdir -p "$STAGE/server"

# manifest.json with the version pinned to this release.
jq --arg v "$VERSION" '.version = $v' "$REPO_ROOT/mcpb/manifest.json" > "$STAGE/manifest.json"

# macOS: fuse the two arch binaries into ONE universal binary, because the
# .mcpb platform override is OS-level (darwin), not arch-level — a single
# `darwin` entry must serve both Apple Silicon and Intel. `lipo` is macOS-only.
if command -v lipo >/dev/null 2>&1; then
  lipo -create -output "$STAGE/server/doiget-darwin" \
    "$BINDIR/doiget-macos-aarch64" "$BINDIR/doiget-macos-x86_64"
else
  echo "::warning::lipo not found — bundling the arm64 macOS binary only (Intel Macs unsupported in this build)" >&2
  cp "$BINDIR/doiget-macos-aarch64" "$STAGE/server/doiget-darwin"
fi
chmod +x "$STAGE/server/doiget-darwin"

cp "$BINDIR/doiget-linux-x86_64"       "$STAGE/server/doiget-linux"
chmod +x "$STAGE/server/doiget-linux"
cp "$BINDIR/doiget-windows-x86_64.exe" "$STAGE/server/doiget-windows.exe"

# Optional icon (mcpb/icon.png), if present.
[ -f "$REPO_ROOT/mcpb/icon.png" ] && cp "$REPO_ROOT/mcpb/icon.png" "$STAGE/icon.png"

# Pack: `mcpb pack <dir> <out>` zips the staging dir into the .mcpb.
mcpb pack "$STAGE" "$OUT"
echo "built $OUT"
