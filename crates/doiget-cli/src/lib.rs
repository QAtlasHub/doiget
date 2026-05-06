//! Library half of the `doiget-cli` crate.
//!
//! The binary entry point lives in `main.rs`; this `lib.rs` re-exports the
//! subcommand orchestrators (`commands::*`) so integration tests under
//! `tests/` can drive them in-process without spawning a child binary.
//! Production users invoke the binary directly via `doiget fetch ...` —
//! `commands::fetch::run` is the single async entry point for that path.
//!
//! # Stability
//!
//! `doiget-cli`'s public API is **not** semver-locked (per
//! `docs/PUBLIC_API.md` — only `doiget-core` is). The surface here exists
//! solely to support integration tests; downstream crates should depend on
//! `doiget-core` instead.

#![warn(missing_docs)]
#![forbid(unsafe_code)]

pub mod commands;
