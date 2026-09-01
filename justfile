# justfile — dev shortcuts that mirror what CI runs.
#
# Cargo invocations don't need a shell, so this file works on bash and
# PowerShell without a `set shell` directive. Toolchain is intentionally
# unpinned here; rust-toolchain.toml is the single source of truth.

# Default: list available recipes.
default:
    @just --list

# Format the workspace in place (local-only; CI runs fmt-check instead).
fmt:
    cargo fmt --all

# Mirrors CI `fmt` job (.github/workflows/ci.yml).
fmt-check:
    cargo fmt --all -- --check

# Mirrors CI `clippy` job. Feature surface (oa-only) matches the published
# build per ADR-0002; lint and test exercise an identical surface.
lint:
    cargo clippy --workspace --all-targets --no-default-features --features oa-only -- -D warnings

# Mirrors CI `test` job.
test:
    cargo test --workspace --all-targets --no-default-features --features oa-only

# Mirrors CI `test (slow)` — the `#[ignore]`d tests, which the job above skips.
# One of them obeys arXiv's published 3 s/request rate and takes ~10 minutes;
# that is the whole reason it is not in `test`.
test-slow:
    cargo test --workspace --all-targets --no-default-features --features oa-only -- --ignored

# Register THIS checkout's build as the MCP server for Claude Code, without
# touching `.mcp.json`.
#
# `.mcp.json` is the plugin's server declaration and pins the last published
# release, so opening this repo in Claude Code otherwise runs the RELEASE, not
# your working tree — you can edit `crates/doiget-mcp` all day and test the
# version you shipped last month. A local-scoped server wins over the
# project-scoped `.mcp.json`, so this shadows it for you alone and leaves the
# file (and everyone else's) unchanged.
#
# `just mcp-dev-off` removes it again.
mcp-dev:
    cargo build --no-default-features --features oa-only -p doiget-cli
    claude mcp remove doiget-dev --scope local || true
    claude mcp add doiget-dev --scope local -- cargo run --quiet --no-default-features --features oa-only -p doiget-cli -- serve

# Drop the local-scoped dev server registered by `just mcp-dev`.
mcp-dev-off:
    claude mcp remove doiget-dev --scope local

# Mirrors CI `msrv` job (build of the published surface).
build:
    cargo build --workspace --no-default-features --features oa-only

# Mirrors `cargo audit` job in .github/workflows/audit.yml.
audit:
    cargo audit

# Mirrors `cargo deny` job in .github/workflows/audit.yml.
deny:
    cargo deny check

# Mirrors CI `docs` job.
doc:
    cargo doc --workspace --no-deps

# Composite local pre-push gate: fmt-check + lint + test.
ci: fmt-check lint test
