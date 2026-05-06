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
