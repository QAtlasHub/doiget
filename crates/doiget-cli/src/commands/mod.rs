//! `doiget` subcommand implementations.
//!
//! Each module under this namespace owns a single subcommand from
//! `crates/doiget-cli/src/main.rs`. Phase 1 ships the read-only
//! introspection commands first; fetcher-bound commands follow.

pub mod config;
