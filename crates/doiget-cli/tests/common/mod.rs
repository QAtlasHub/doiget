//! Shared helpers for the `doiget-cli` integration test binaries.
//!
//! Cargo treats `tests/common/mod.rs` as a non-test submodule — it is
//! compiled into each `tests/<name>_e2e.rs` integration test binary that
//! declares `mod common;`, but never compiled as its own test binary
//! (so it never produces a "no tests found" warning). See
//! <https://doc.rust-lang.org/book/ch11-03-test-organization.html#submodules-in-integration-tests>
//! for the canonical pattern.
//!
//! Per-`tests/`-binary `unused`-warnings: each integration test binary
//! is a separate crate, and each may use only a subset of this module's
//! exports. The `#[allow(dead_code)]` attribute on
//! [`env_guard::EnvGuard`] (and its members) keeps every binary's
//! `-D warnings` build green regardless of which helpers it exercises.

pub mod env_guard;
