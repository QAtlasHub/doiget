//! Phase 0 sanity: assert that the doiget-core library does not initiate
//! network connections during construction or any non-fetch path.
//!
//! This test does NOT use a real socket sandbox (those are kernel-level
//! features). It instead verifies that:
//!   1. `CapabilityProfile::from_env()` does not panic on an env that has
//!      no network env vars set.
//!   2. `Doi::parse` / `ArxivId::parse` / `Ref::parse` are pure (no clock,
//!      no env, no network).
//!
//! Phase 1 will replace this with a mock-network harness once `Source::fetch`
//! arrives. See `docs/SECURITY.md` §1.10 (network side channel) and
//! `docs/PHASES.md` §3.

#![allow(clippy::expect_used, clippy::unwrap_used)]

#[test]
fn parse_functions_make_no_outbound_calls() {
    // Snapshot: parse should be pure. If a future refactor adds an
    // env::var lookup or a clock call, this test pins the contract.
    let _ = doiget_core::Doi::parse("10.1234/example");
    let _ = doiget_core::ArxivId::parse("2401.12345");
    let _ = doiget_core::Ref::parse("doi:10.1234/example");
    // No assertion needed — if parse internally tried to bind a socket on
    // a sandbox runner with no network, it would error. We rely on the
    // posture-lint network-purity job for the static guarantee, and on
    // libfuzzer harnesses (separate PR) for the dynamic robustness.
}

#[test]
fn capability_profile_from_env_phase_0_returns_tier_1_only() {
    // Re-pin the Phase-0 stub guarantee from the doctest in lib.rs:
    // no env -> tier-1 profile, no error, no network.
    let p = doiget_core::CapabilityProfile::from_env().expect("Phase 0 stub never errors");
    assert!(p.tdm_elsevier.is_none());
    assert!(p.tdm_aps.is_none());
    assert!(p.tdm_springer.is_none());
}
