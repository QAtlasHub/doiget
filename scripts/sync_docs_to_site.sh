#!/usr/bin/env bash
# sync_docs_to_site.sh — copy curated docs/* sources into site/content/* with
# TOML front-matter injected so Zola can render them.
#
# Sync strategy: site/content/* is git-managed so the docs/ -> site/
# projection is auditable in PR diffs. Run this script locally (or in CI)
# after editing docs/*.md, then commit BOTH the source (docs/) and the
# projection (site/content/).
#
# The script is intentionally a thin bash helper — no Python / Node / jq —
# so it Just Works from any contributor's shell on Linux, macOS, and Git
# Bash on Windows. Each docs/*.md page maps to ONE site/content/<layer>/<slug>.md
# file. The layer mapping is the MAPPINGS table below.
#
# Files in docs/DECISIONS/ are NOT projected by this script — ADRs are a
# self-contained corpus and contributors should follow the GitHub link on
# the contribute/ section landing page rather than have ADRs duplicated as
# Zola pages.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

DOCS_DIR="$REPO_ROOT/docs"
SITE_CONTENT="$REPO_ROOT/site/content"

if [ ! -d "$DOCS_DIR" ]; then
  echo "error: docs directory not found at $DOCS_DIR" >&2
  exit 1
fi

if [ ! -d "$SITE_CONTENT" ]; then
  echo "error: site/content directory not found at $SITE_CONTENT" >&2
  echo "       run this script after the Zola scaffold has landed." >&2
  exit 1
fi

# Mapping table: <docs-file>:<site-relative-output-path>:<title>:<weight>
# Weights start at 100 to leave room for hand-written stubs at 10-90.
MAPPINGS=(
  "ARCHITECTURE.md:contribute/architecture.md:Architecture:100"
  "CAPABILITY.md:developer/capability.md:Capability profile:110"
  "CACHE.md:developer/cache.md:Cache:120"
  "CONFIG.md:developer/config.md:Configuration:130"
  "ERRORS.md:developer/errors.md:Error codes:140"
  "LEGAL.md:use/legal.md:Legal posture:140"
  "MCP_TOOLS.md:developer/mcp-tools.md:MCP tools:100"
  "MIGRATION.md:use/migration.md:BiblioFetch.jl migration:130"
  "PHASES.md:contribute/phases.md:Phase plan:110"
  "PROVENANCE_LOG.md:developer/provenance-log.md:Provenance log:150"
  "PUBLIC_API.md:developer/public-api.md:Public API:160"
  "REDIRECT_ALLOWLIST.md:developer/redirect-allowlist.md:Redirect allowlist:170"
  "SAFEKEY.md:developer/safekey.md:Safekey algorithm:180"
  "SCOPE.md:use/scope.md:Scope (what doiget does and does not do):130"
  "SECURITY.md:developer/security.md:Security:190"
  "SOURCES.md:developer/sources.md:Sources matrix:200"
  "STORE.md:developer/store.md:Store contract:210"
)

projected=0
skipped=0

for mapping in "${MAPPINGS[@]}"; do
  src_name="${mapping%%:*}"
  rest="${mapping#*:}"
  out_rel="${rest%%:*}"
  rest="${rest#*:}"
  title="${rest%%:*}"
  weight="${rest#*:}"

  src="$DOCS_DIR/$src_name"
  out="$SITE_CONTENT/$out_rel"

  if [ ! -f "$src" ]; then
    echo "skip: $src_name not present in docs/ (pre-Phase resource)" >&2
    skipped=$((skipped + 1))
    continue
  fi

  mkdir -p "$(dirname "$out")"

  # First non-empty non-directive prose line becomes the description
  # (truncated to 200 chars). Skip H1/H2 headings and blockquote markers.
  description=$(awk '
    /^[[:space:]]*$/ { next }
    /^# / { next }
    /^## / { next }
    /^> / { next }
    { print; exit }
  ' "$src" | head -c 200 | tr -d '\r' | sed 's/"/\\"/g')

  {
    printf '+++\n'
    printf 'title = "%s"\n' "$title"
    printf 'description = "%s"\n' "$description"
    printf 'weight = %s\n' "$weight"
    printf '+++\n'
    printf '\n'
    cat "$src"
  } > "$out"

  echo "wrote: site/content/$out_rel  (title='$title', weight=$weight)"
  projected=$((projected + 1))
done

echo
echo "projected $projected docs/*.md page(s) to site/content/*."
if [ "$skipped" -gt 0 ]; then
  echo "skipped $skipped mapping(s) due to missing docs/ source."
fi
