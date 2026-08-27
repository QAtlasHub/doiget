#!/usr/bin/env bash
# One-time bootstrap of the five npm packages (#511).
#
# WHY THIS EXISTS
#
# npm Trusted Publishing cannot perform a package's FIRST publish. The trusted
# publisher setting lives under a package's Settings, and there is no Settings
# page for a package that does not exist — so the release workflow's pure-OIDC
# job, which carries no NPM_TOKEN by design, can never create these packages.
# v0.8.11 released with that job red for a different reason (a path spec npm
# read as a GitHub shorthand); had that been fixed, it would have failed here
# instead. The order was simply inverted: OIDC has to be configured before it
# can be used.
#
# The way out is the documented one: publish a placeholder with a token once,
# configure the trusted publisher against the real package, then never use a
# token again.
#
# WHAT IT PUBLISHES
#
# The five templates under `npm/` verbatim, at their checked-in `0.0.0`. The
# four platform packages carry no `bin` field and no binary, so a 0.0.0 of them
# is inert. The wrapper's `optionalDependencies` pin `0.0.0`, which resolves to
# those inert packages.
#
# Under `--tag placeholder`, NOT `latest`. `npm publish` only moves `latest`
# when the tag is `latest`, so after this runs `npm install doiget` fails with
# "No matching version" rather than installing a wrapper with no binary in it.
# That is the honest state until a real release: this repo has only ever pushed
# stable tags (38 tags, zero betas), and the release job publishes betas under
# `beta`, so `latest` first appears at the next STABLE release.
#
# WHAT IT DOES NOT DO
#
# No `--provenance`: provenance needs a CI OIDC token, which is exactly what
# this script exists to make possible. The placeholders are unprovenanced;
# every real version is provenanced.
#
# Usage:
#   scripts/bootstrap-npm.sh            # dry run: show what would happen
#   scripts/bootstrap-npm.sh --publish  # actually publish
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="$REPO_ROOT/npm"
PLACEHOLDER_TAG="placeholder"

# Read the package list from the staging script rather than restating it: a
# fifth platform added there must not be silently skipped here.
MAP_SOURCE="$REPO_ROOT/scripts/stage-npm.sh"
PACKAGES="$(grep -oE '^doiget-[a-z0-9-]+:' "$MAP_SOURCE" | tr -d ':' | sort -u || true)"
if [ -z "$PACKAGES" ]; then
  echo "error: could not read the package list from $MAP_SOURCE" >&2
  exit 1
fi
# The wrapper goes last: its optionalDependencies pin the platform packages by
# exact version, so publishing it first leaves a window in which
# `npm install doiget` resolves nothing to run.
PACKAGES="$PACKAGES
doiget"

PUBLISH=0
case "${1:-}" in
  --publish) PUBLISH=1 ;;
  ""|--dry-run) ;;
  *) echo "usage: $0 [--publish]" >&2; exit 2 ;;
esac

echo "packages:"
for p in $PACKAGES; do
  [ -f "$SRC/$p/package.json" ] || { echo "error: $SRC/$p/package.json is missing" >&2; exit 1; }
  # Read with grep, not node: `node -p require(...)` is handed an MSYS path
  # on Windows and cannot resolve it, and reading one field does not justify
  # a runtime dependency here.
  v="$(grep -oE '"version"[[:space:]]*:[[:space:]]*"[^"]+"' "$SRC/$p/package.json" | head -1 | awk -F'"' '{print $4}')"
  echo "  $p@$v"
  if [ "$v" != "0.0.0" ]; then
    echo "error: $p is at $v, not the 0.0.0 placeholder. This script publishes the" >&2
    echo "       templates verbatim; a stamped version here means it is running" >&2
    echo "       against a staged tree rather than the repository." >&2
    exit 1
  fi
done
echo

# Refuse to re-run against packages that already exist. Bootstrapping is
# once-only, and a second run would publish nothing useful while looking like
# it had worked.
existing=0
for p in $PACKAGES; do
  code="$(curl -s -o /dev/null -w '%{http_code}' "https://registry.npmjs.org/$p")"
  if [ "$code" = "200" ]; then
    echo "already on the registry: $p"
    existing=$((existing + 1))
  elif [ "$code" != "404" ]; then
    echo "error: unexpected HTTP $code for $p — check the registry is reachable" >&2
    exit 1
  fi
done
if [ "$existing" -gt 0 ]; then
  echo
  echo "$existing of these already exist. Bootstrapping is once-only; configure the"
  echo "trusted publisher on them instead (fields below) rather than re-running."
  if [ "$PUBLISH" = "1" ]; then
    exit 1
  fi
fi

if [ "$PUBLISH" = "0" ]; then
  echo
  echo "DRY RUN. Nothing was published. Re-run with --publish to do it."
  echo "Before that: npm login, 2FA enabled, and a granular access token with"
  echo "write access to packages."
else
  who="$(npm whoami 2>/dev/null || true)"
  if [ -z "$who" ]; then
    echo "error: not logged in to npm. Run 'npm login' first." >&2
    exit 1
  fi
  echo "publishing as: $who"
  echo
  for p in $PACKAGES; do
    echo "--- $p"
    # `./` is load-bearing: a bare `npm/doiget-linux-x64` matches npm's
    # `owner/repo` GitHub shorthand and is never read as a directory. That is
    # the bug that took down the v0.8.11 npm job.
    npm publish "./npm/$p" --access public --tag "$PLACEHOLDER_TAG"
  done
  echo
  echo "marking the placeholders so nobody installs one by accident:"
  for p in $PACKAGES; do
    npm deprecate "$p@0.0.0" \
      "placeholder published to bootstrap npm trusted publishing; use a real release" || true
  done
fi

cat <<EOF

Next, on npmjs.com, for EACH of these packages:

  Settings -> Trusted Publisher -> GitHub Actions

    Organization or user   QAtlasHub
    Repository             doiget
    Workflow filename      release-plz.yml
    Allowed actions        npm publish
    Environment            (leave empty)

$(for p in $PACKAGES; do echo "  https://www.npmjs.com/package/$p/access"; done)

Then revoke the token used here — the release workflow authenticates over OIDC
and needs no stored credential.

Note: 'latest' is deliberately unset. It is first written by the next STABLE
release; betas publish under the 'beta' dist-tag. Until then 'npm install
doiget' fails cleanly rather than installing an empty wrapper.
EOF
