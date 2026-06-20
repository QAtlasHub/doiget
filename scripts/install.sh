#!/bin/sh
# doiget installer — download the prebuilt, checksum-verified binary for this
# host from the latest (or a pinned) GitHub Release and install it. No Rust
# toolchain or compilation required.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/sotashimozono/doiget/main/scripts/install.sh | sh
#
# Environment overrides:
#   DOIGET_VERSION      version to install WITHOUT the leading 'v' (default: latest stable)
#   DOIGET_INSTALL_DIR  install directory (default: $HOME/.local/bin)
#
# POSIX sh on purpose (not the repo's bash house style): a script piped to `sh`
# must run under any POSIX shell. The published `.sha256` sidecar is verified
# before install; a mismatch aborts. cosign bundles are also published — see
# the README for optional keyless signature verification.
set -eu

REPO="sotashimozono/doiget"
VERSION="${DOIGET_VERSION:-latest}"
INSTALL_DIR="${DOIGET_INSTALL_DIR:-$HOME/.local/bin}"

err() { printf 'doiget-install: error: %s\n' "$1" >&2; exit 1; }
info() { printf 'doiget-install: %s\n' "$1"; }

# --- detect target -> release asset name ---------------------------------
os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
  Linux)
    case "$arch" in
      x86_64 | amd64) asset="doiget-linux-x86_64" ;;
      aarch64 | arm64) err "linux-aarch64 is not published yet — use 'cargo binstall doiget' or 'cargo install doiget' (target tracked in #247)" ;;
      *) err "unsupported Linux architecture: $arch" ;;
    esac
    ;;
  Darwin)
    case "$arch" in
      arm64 | aarch64) asset="doiget-macos-aarch64" ;;
      x86_64) asset="doiget-macos-x86_64" ;;
      *) err "unsupported macOS architecture: $arch" ;;
    esac
    ;;
  *) err "unsupported OS: $os — on Windows use scripts/install.ps1" ;;
esac

# --- resolve release URLs ------------------------------------------------
if [ "$VERSION" = "latest" ]; then
  base="https://github.com/$REPO/releases/latest/download"
else
  base="https://github.com/$REPO/releases/download/v$VERSION"
fi

# --- pick a downloader ---------------------------------------------------
if command -v curl >/dev/null 2>&1; then
  dl() { curl -fsSL "$1" -o "$2"; }
elif command -v wget >/dev/null 2>&1; then
  dl() { wget -qO "$2" "$1"; }
else
  err "need curl or wget to download"
fi

# --- portable sha256 of a file (first field = hex digest) ----------------
sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  elif command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 "$1" | awk '{print $NF}'
  else
    err "need sha256sum, shasum, or openssl to verify the download"
  fi
}

tmp="$(mktemp -d)"
# shellcheck disable=SC2064  # expand $tmp now so the trap removes this exact dir
trap "rm -rf '$tmp'" EXIT INT TERM

info "downloading $asset ($VERSION)"
dl "$base/$asset" "$tmp/$asset" || err "download failed: $base/$asset"
dl "$base/$asset.sha256" "$tmp/$asset.sha256" || err "checksum download failed: $base/$asset.sha256"

# The sidecar is `openssl dgst -sha256 -r` output: "<hex>  *<filename>".
expected="$(awk '{print $1}' "$tmp/$asset.sha256")"
actual="$(sha256_of "$tmp/$asset")"
[ -n "$expected" ] || err "empty expected checksum in $asset.sha256"
[ "$expected" = "$actual" ] || err "checksum mismatch: expected $expected, got $actual"
info "checksum OK ($actual)"

# --- install -------------------------------------------------------------
mkdir -p "$INSTALL_DIR"
mv "$tmp/$asset" "$INSTALL_DIR/doiget"
chmod +x "$INSTALL_DIR/doiget"
info "installed to $INSTALL_DIR/doiget"

case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) info "note: $INSTALL_DIR is not on your PATH — add: export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
esac

info "done — run: doiget --version"
