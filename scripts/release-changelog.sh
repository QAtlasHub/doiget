#!/usr/bin/env bash
# release-changelog.sh — LOCAL, pre-tag changelog draft generator (ADR-0025 D4).
#
# Runs git-cliff over `<last-tag-in-lane>..HEAD` with cliff.toml and prints the
# generated section to STDOUT for the maintainer to REVIEW + EDIT before making
# the release commit. It NEVER writes CHANGELOG.md and NEVER commits — git-cliff
# replaces release-plz purely as a local, reviewable changelog drafter, not as
# an automated merge-time PR (the #164 failure mode).
#
# Usage:
#   scripts/release-changelog.sh [<since-ref>]
#
# <since-ref> defaults to the most recent reachable release tag. The repo's
# legacy per-crate tags are `doiget-core-v*` (release-plz scheme, being
# retired); the new scheme is a single `v*` workspace tag (ADR-0025 D1). We
# auto-detect: prefer the latest `v*` tag, else fall back to the latest
# `doiget-core-v*`, so this works for both the first ADR-0025 release (no `v*`
# tag yet) and every release after.
#
# House style mirrors scripts/sync_docs_to_site.sh / release-version-gate.sh:
# thin dependency-light bash (no Python / Node / jq). Requires `git-cliff` on
# PATH (https://git-cliff.org); the maintainer installs it locally — it is NOT
# a CI dependency (no automated changelog generation under ADR-0025).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

cd "$REPO_ROOT"

if ! command -v git-cliff >/dev/null 2>&1; then
  echo "error: git-cliff not found on PATH." >&2
  echo "       install it (cargo install git-cliff, or see https://git-cliff.org)" >&2
  echo "       — it is a LOCAL maintainer tool, intentionally NOT a CI dependency." >&2
  exit 1
fi

if [ ! -f "$REPO_ROOT/cliff.toml" ]; then
  echo "error: cliff.toml not found at repo root" >&2
  exit 1
fi

SINCE="${1:-}"
if [ -z "$SINCE" ]; then
  # Prefer the newest ADR-0025 workspace tag `vX.Y.Z[-PRE]`; if none exists yet
  # (first release under 0025), fall back to the newest legacy per-crate
  # `doiget-core-v*` tag (the release-plz scheme being retired).
  SINCE="$(git tag --list 'v[0-9]*' --sort=-version:refname | head -n1 || true)"
  if [ -z "$SINCE" ]; then
    SINCE="$(git tag --list 'doiget-core-v*' --sort=-version:refname | head -n1 || true)"
  fi
  if [ -z "$SINCE" ]; then
    echo "error: could not auto-detect a last release tag; pass <since-ref> explicitly" >&2
    exit 1
  fi
fi

echo "# git-cliff draft for range ${SINCE}..HEAD" >&2
echo "# REVIEW + EDIT this before pasting it into CHANGELOG.md; do NOT commit verbatim." >&2
echo "# (ADR-0025 D4: generated section is reviewed/edited, then the version gate" >&2
echo "#  D2-G5 enforces a non-empty CHANGELOG section exists at tag time.)" >&2
echo >&2

# --config: explicit cliff.toml. The range arg makes git-cliff walk ALL
# commits in <since>..HEAD (cliff.toml sets first_parent = false — the exact
# #164 root cause: first-parent traversal hid conventional commits behind
# merge commits). Output goes to stdout (no --output / no --prepend) so the
# maintainer copy-edits manually.
exec git-cliff --config "$REPO_ROOT/cliff.toml" "${SINCE}..HEAD"
