#!/usr/bin/env bash
# The cargo-binstall overrides in crates/doiget-cli/Cargo.toml name release
# assets by hand (#501). A renamed or dropped release artifact would make
# `cargo binstall doiget-cli` 404 with nothing in CI noticing, so this pins
# the two lists to each other: every override names an artifact the release
# matrix builds, and every artifact the matrix builds has an override.
set -euo pipefail
cd "$(dirname "$0")/.."

release="$(sed -n 's/^ *artifact: *\(doiget-[A-Za-z0-9_.-]*\).*/\1/p' .github/workflows/release-plz.yml | sort -u)"
binstall="$(sed -n 's#^pkg-url = ".*/\(doiget-[A-Za-z0-9_.-]*\)"#\1#p' crates/doiget-cli/Cargo.toml | sed 's/\.exe$//' | sort -u)"

[ -n "$release" ] || { echo "no artifact: lines found in release-plz.yml" >&2; exit 1; }
[ -n "$binstall" ] || { echo "no binstall pkg-url lines found in crates/doiget-cli/Cargo.toml" >&2; exit 1; }

if [ "$release" != "$binstall" ]; then
  echo "binstall overrides and the release matrix disagree:" >&2
  diff <(echo "$release") <(echo "$binstall") | sed 's/^</  release only: /; s/^>/  binstall only: /' | grep 'only' >&2
  exit 1
fi
echo "binstall assets match the release matrix: $(echo "$release" | tr '\n' ' ')"
