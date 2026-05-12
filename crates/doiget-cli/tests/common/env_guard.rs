//! RAII helper that scopes env-var mutations within a single test.
//!
//! On construction, snapshots the current value of each named env var
//! (or `None` if unset) and clears the variable. Tests then call
//! [`EnvGuard::set`] to seed test-specific values. On drop, every prior
//! value is restored — `Some(_)` is written back via `std::env::set_var`,
//! `None` is written back as a `remove_var`.
//!
//! ## Threading
//!
//! Env state is process-global. Tests holding this guard MUST also be
//! marked `#[serial]` (via the `serial_test` crate) so the (single-
//! threaded) `serial` group is the only writer at a time. Concurrent
//! tests outside the `serial` group will race with this guard's
//! mutations.
//!
//! ## History
//!
//! Slice 5 (PR #84 advisory item A5 refactor): four
//! `crates/doiget-cli/tests/*_e2e.rs` integration-test binaries each
//! defined a private `struct EnvGuard` with subtly different
//! capabilities (some snapshotted prior values, some only cleared).
//! Consolidated here on the snapshot-and-restore variant so a test that
//! runs after another test that exported a real `DOIGET_*` env var
//! cannot leak that variable into the test under inspection.

#![allow(dead_code)]

/// RAII guard that saves + clears the named env vars on construction
/// and restores them on drop.
pub struct EnvGuard {
    keys: Vec<&'static str>,
    prior: Vec<Option<std::ffi::OsString>>,
}

impl EnvGuard {
    /// Snapshot the current value of every name in `keys`, then clear
    /// each so the test starts from a clean slate. Pre-existing values
    /// are restored on drop.
    pub fn new(keys: &[&'static str]) -> Self {
        let prior: Vec<Option<std::ffi::OsString>> = keys.iter().map(std::env::var_os).collect();
        for k in keys {
            std::env::remove_var(k);
        }
        Self {
            keys: keys.to_vec(),
            prior,
        }
    }

    /// Set a (test-scoped) env var. The value is reverted to its
    /// pre-`new` state when this guard drops.
    pub fn set(&self, key: &str, val: &str) {
        std::env::set_var(key, val);
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (k, prior) in self.keys.iter().zip(self.prior.iter()) {
            match prior {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
    }
}
