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
# Under `--tag placeholder`. This does NOT keep `latest` unset -- an earlier
# version of this comment claimed it did, and the registry disagrees: npm sets
# `latest` on a package's FIRST publish regardless of `--tag`. Measured after
# the real run:
#
#   doiget-linux-x64  dist-tags: {'placeholder': '0.0.0', 'latest': '0.0.0'}
#
# So a placeholder IS what `npm install <pkg>` resolves to until a real release
# moves `latest`. That is why the deprecation below is not cosmetic: it is the
# only warning a user gets. It is also why the wrapper matters most -- nobody
# installs a platform package directly, but `npm install doiget` would have
# landed on an empty 0.0.0.
#
# WHAT IT DOES NOT DO
#
# No `--provenance`: provenance needs a CI OIDC token, which is exactly what
# this script exists to make possible. The placeholders are unprovenanced;
# every real version is provenanced.
#
# Usage:
#   scripts/bootstrap-npm.sh            # dry run: show what would happen
#   scripts/bootstrap-npm.sh --publish  # publish; type the OTP when asked
#   OTP=123456 scripts/bootstrap-npm.sh --publish   # non-interactive
#
# Re-running after a partial run is safe and expected: packages already on the
# registry are skipped and the rest are published. npm asks for a fresh
# one-time password per publish, so stopping half way through is the normal
# case rather than the exceptional one.
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
doiget-cli"

# Everything below uses paths relative to the repository root, so stand there.
# See the note on the `npm publish` call for why relative rather than absolute.
cd "$REPO_ROOT"

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

# Work out what is left to do. Publishing five packages is five separate
# registry calls, and npm demands a fresh one-time password for each when the
# account has 2FA on — so a run stopping half way through is the NORMAL case,
# not the exceptional one. An earlier version refused to start whenever any
# package already existed, which turned one mistyped OTP into "the remaining
# packages can never be published by this script". Skip what is done, do what
# is left.
TODO=""
existing=0
for p in $PACKAGES; do
  code="$(curl -s -o /dev/null -w '%{http_code}' "https://registry.npmjs.org/$p")"
  case "$code" in
    200)
      echo "already on the registry, skipping: $p"
      existing=$((existing + 1))
      ;;
    404)
      TODO="$TODO
$p"
      ;;
    *)
      echo "error: unexpected HTTP $code for $p — check the registry is reachable" >&2
      exit 1
      ;;
  esac
done
TODO="$(printf '%s' "$TODO" | sed '/^$/d')"
if [ -z "$TODO" ]; then
  echo
  echo "All $existing packages are published. Nothing left to bootstrap —"
  echo "configure the trusted publisher on them (fields below)."
fi

if [ "$PUBLISH" = "0" ]; then
  echo
  echo "DRY RUN. Nothing was published. Re-run with --publish to do it."
  echo
  echo "npm requires a second factor for every publish. Two ways:"
  echo "  * run this in an INTERACTIVE terminal and type the OTP when npm asks"
  echo "    (once per package, so five times, plus the deprecations);"
  echo "  * or set OTP=123456 for a single code that covers the whole run, if"
  echo "    your authenticator's window is long enough."
  echo "A granular access token with 'bypass 2FA' also works and is what npm's"
  echo "own 403 suggests, but npm is restricting those for direct publishing —"
  echo "and the point of this bootstrap is to stop needing tokens at all."
elif [ -n "$TODO" ]; then
  who="$(npm whoami 2>/dev/null || true)"
  if [ -z "$who" ]; then
    echo "error: not logged in to npm. Run 'npm login' first." >&2
    exit 1
  fi
  echo
  echo "publishing as: $who"
  # npm requires a second factor to publish. With 2FA disabled there is no
  # code to ask for, so npm does not prompt -- it just returns
  #   403 ... Two-factor authentication or granular access token with bypass
  #   2fa enabled is required to publish packages
  # which reads like a permissions problem rather than "your account is not
  # set up yet". Say so before spending an OTP prompt that will never appear.
  if npm profile get 2>/dev/null | grep -qi "two-factor auth: disabled"; then
    echo >&2
    echo "error: this npm account has two-factor auth disabled, and npm requires" >&2
    echo "       a second factor to publish. Nothing here can work until it is on." >&2
    echo >&2
    echo "  1. npmjs.com -> Account -> Two-Factor Authentication (authenticator" >&2
    echo "     app). Save the recovery codes." >&2
    echo "  2. While there, link your GitHub account: it is npm's documented" >&2
    echo "     fallback if the 2FA device AND the recovery codes are both lost." >&2
    echo "  3. npm logout && npm login -- the current session token predates" >&2
    echo "     2FA and is not second-factor verified." >&2
    echo "  4. Re-run this script." >&2
    exit 1
  fi
  # An OTP is accepted for a short window, so one code can cover several
  # calls. Passing it explicitly also lets this run without a TTY.
  OTP_ARGS=""
  if [ -n "${OTP:-}" ]; then
    OTP_ARGS="--otp=$OTP"
  fi
  echo
  for p in $TODO; do
    echo "--- $p"
    # RELATIVE, from $REPO_ROOT, and both halves matter.
    #
    # `./` is load-bearing: a bare `npm/doiget-linux-x64` matches npm's
    # `owner/repo` GitHub shorthand and is never read as a directory. That is
    # the bug that took down the v0.8.11 npm job.
    #
    # Relative is load-bearing too. An absolute `$SRC` is a POSIX path under
    # WSL or Git Bash, and `npm` on PATH may well be the WINDOWS npm, which
    # reads `/mnt/c/...` as a relative path and opens `C:\mnt\c\...`. A
    # relative path sidesteps the whole question: WSL translates the working
    # directory when it launches a Windows binary, so both agree on where
    # "here" is. cwd-independence comes from the `cd` above, not from
    # spelling the path out.
    # shellcheck disable=SC2086
    npm publish "./npm/$p" --access public --tag "$PLACEHOLDER_TAG" $OTP_ARGS
  done
fi

# Deprecate every 0.0.0 on the registry, not only the ones this run published.
# A run that stops half way leaves the earlier packages undeprecated, and since
# npm points `latest` at them they are exactly what `npm install` resolves to --
# the notice is the only warning anyone gets, so it must not depend on the run
# reaching the end.
if [ "$PUBLISH" = "1" ]; then
  echo
  echo "marking placeholders so nobody installs one by accident:"
  for p in $PACKAGES; do
    code="$(curl -s -o /dev/null -w '%{http_code}' "https://registry.npmjs.org/$p")"
    [ "$code" = "200" ] || continue
    if curl -s "https://registry.npmjs.org/$p" | grep -q '"deprecated"'; then
      echo "  already deprecated: $p@0.0.0"
      continue
    fi
    # shellcheck disable=SC2086
    npm deprecate "$p@0.0.0" \
      "placeholder published to bootstrap npm trusted publishing; use a real release" \
      $OTP_ARGS || echo "  (deprecate failed for $p — re-run this script to retry)"
  done
fi

cat <<EOF

Next, on npmjs.com, for EACH of these packages:

  the package's ACCESS page (below) -> Trusted Publisher -> GitHub Actions

    Organization or user   QAtlasHub
    Repository             doiget
    Workflow filename      release-plz.yml     (filename only, no path)
    Environment name       (leave empty)
    Allowed actions        npm publish

$(for p in $PACKAGES; do echo "  https://www.npmjs.com/package/$p/access"; done)

Then revoke the token used here — the release workflow authenticates over OIDC
and needs no stored credential.

Note: npm sets 'latest' to 0.0.0 on a first publish whatever --tag says, so
until a real release these placeholders ARE what 'npm install <pkg>' resolves
to. The deprecation notice is the only thing warning anyone. The next release
moves 'latest' to a real version.
EOF
