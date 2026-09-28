#!/usr/bin/env bash
# install.test.sh — offline test of scripts/install.sh (#594).
#
# A fake `curl` on PATH serves release assets from a fixture directory, and the
# "binary" is a shell script that answers `--version`. Installing 0.8.9 and
# then 0.9.0 over it must (1) say what it replaced and (2) leave a manifest
# naming the version it actually installed.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64|Linux-amd64) asset=doiget-linux-x86_64 ;;
  Darwin-arm64|Darwin-aarch64) asset=doiget-macos-aarch64 ;;
  Darwin-x86_64) asset=doiget-macos-x86_64 ;;
  *) echo "skip: no published asset for $(uname -s)-$(uname -m)"; exit 0 ;;
esac

mkdir -p "$work/bin" "$work/fixtures" "$work/install"
cat > "$work/bin/curl" <<'CURL'
#!/bin/sh
# fake curl: curl -fsSL <url> -o <out>  ->  copy fixtures/<basename of url>
url=""; out=""
while [ $# -gt 0 ]; do
  case "$1" in -o) out="$2"; shift 2 ;; -*) shift ;; *) url="$1"; shift ;; esac
done
cp "$FIXTURES/$(basename "$url")" "$out"
CURL
chmod +x "$work/bin/curl"

fixture() { # version -> write a fake binary + its .sha256 sidecar
  printf '#!/bin/sh\necho "doiget %s"\n' "$1" > "$work/fixtures/$asset"
  if command -v sha256sum >/dev/null 2>&1; then
    sum="$(sha256sum "$work/fixtures/$asset" | awk '{print $1}')"
  else
    sum="$(shasum -a 256 "$work/fixtures/$asset" | awk '{print $1}')"
  fi
  printf '%s *%s\n' "$sum" "$asset" > "$work/fixtures/$asset.sha256"
}

run() { PATH="$work/bin:$PATH" FIXTURES="$work/fixtures" DOIGET_INSTALL_DIR="$work/install" sh "$here/install.sh"; }

fixture 0.8.9
first="$(run)"
fixture 0.9.0
second="$(run)"

fail() { echo "FAIL: $1"; echo "--- first:"; echo "$first"; echo "--- second:"; echo "$second"; exit 1; }
echo "$first" | grep -q "installed doiget 0.8.9" || fail "first install did not name its version"
echo "$second" | grep -q "replacing doiget 0.8.9 with 0.9.0" || fail "upgrade did not say what it replaced"
grep -q '"installer": "install.sh"' "$work/install/doiget.install.json" || fail "manifest installer"
grep -q '"version": "0.9.0"' "$work/install/doiget.install.json" || fail "manifest version"
grep -q "\"asset\": \"$asset\"" "$work/install/doiget.install.json" || fail "manifest asset"
third="$(run)"
echo "$third" | grep -q "reinstalling doiget 0.9.0 (same version)" || fail "same-version re-run"
echo "install.test.sh: ok"
