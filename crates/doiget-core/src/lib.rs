//! # doiget-core
//!
//! Core library for [doiget](https://github.com/sotashimozono/doiget): an Open Access
//! first paper-fetcher with strict capability gating, fail-closed provenance logging,
//! and a BiblioFetch.jl-compatible store layout.
//!
//! Phase 0 ships only this skeleton. Real implementations land in Phase 1.
//! See `docs/PUBLIC_API.md` for the semver-locked surface and `docs/ARCHITECTURE.md`
//! for the high-level design.

#![warn(missing_docs)]
#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

/// Crate version. Used by `doiget-cli --version` and `doiget_health`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// TOML schema version this build writes. See `docs/STORE.md` §3.
pub const SCHEMA_VERSION: &str = "1.0";

/// Hard-coded rate limit. See `docs/LEGAL.md` §6 safeguard 8.
pub const MAX_CONCURRENT_FETCHES: u32 = 5;

/// Hard-coded rate limit. See `docs/LEGAL.md` §6 safeguard 8.
pub const MAX_FETCHES_PER_SECOND: f32 = 5.0;

/// Maximum batch size for `doiget batch` and `doiget_batch_fetch`.
pub const MCP_BATCH_MAX_SIZE: usize = 100;

/// Maximum DOI suffix length accepted at validation. See `docs/SECURITY.md` §1.1.
pub const DOI_SUFFIX_MAX_LEN: usize = 256;

// ---------------------------------------------------------------------------
// Ref
// ---------------------------------------------------------------------------

/// A reference to a paper, either by DOI or arXiv id.
///
/// See `docs/SECURITY.md` §1.1 for input-validation rules.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", tag = "kind", content = "id")]
pub enum Ref {
    /// A DOI (e.g., `10.1234/example`).
    Doi(Doi),
    /// An arXiv id (e.g., `2401.12345`).
    Arxiv(ArxivId),
}

/// A validated DOI string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Doi(String);

/// A validated arXiv id string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ArxivId(String);

impl Doi {
    /// Returns the DOI as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl ArxivId {
    /// Returns the arXiv id as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// ---------------------------------------------------------------------------
// Safekey
// ---------------------------------------------------------------------------

/// A filesystem-safe key derived deterministically from a `Ref`.
///
/// See `docs/SAFEKEY.md` for the full algorithm and reference test vectors.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Safekey(String);

impl Safekey {
    /// Returns the safekey as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// ---------------------------------------------------------------------------
// ErrorCode
// ---------------------------------------------------------------------------

/// The closed set of error codes doiget surfaces.
///
/// See `docs/ERRORS.md` for the persona × code matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    /// DOI / arXiv id failed validation.
    InvalidRef,
    /// Tier 1 sources reported no OA URL.
    NoOaAvailable,
    /// Internal rate cap or upstream 429.
    RateLimited,
    /// Transport / DNS / TLS failure.
    NetworkError,
    /// Filesystem write failed.
    StoreError,
    /// Provenance log write failed; the fetch was aborted.
    LogError,
    /// Source not granted by the runtime `CapabilityProfile`.
    CapabilityDenied,
    /// Per-request timeout exceeded.
    FetchTimeout,
    /// Store entry's `schema_version` is ahead of this build.
    SchemaTooNew,
    /// Could not acquire `flock` within 5 s.
    LockTimeout,
    /// Bug — please open an issue.
    InternalError,
}

// ---------------------------------------------------------------------------
// CapabilityProfile (placeholder; full impl in Phase 1)
// ---------------------------------------------------------------------------

/// Marker for the always-on Open Access tier. See `docs/CAPABILITY.md`.
#[derive(Debug, Clone, Copy)]
pub struct AlwaysOn;

/// Which Tier 2 metadata sources are enabled this session. See `docs/CAPABILITY.md`.
#[derive(Debug, Clone, Default)]
pub struct MetadataAccess {
    /// Phase 4+; enabled by `DOIGET_ENABLE_OPENALEX`.
    pub openalex: bool,
    /// Phase 4+; enabled by `DOIGET_ENABLE_S2`.
    pub semantic_scholar: bool,
    /// Phase 4+; enabled by `DOIGET_ENABLE_DOAJ`.
    pub doaj: bool,
}

/// Process-wide rate limits. Hard-coded; not configurable.
///
/// See `docs/LEGAL.md` §6 safeguard 8.
#[derive(Debug, Clone, Copy)]
pub struct RateLimits {
    /// Maximum concurrent fetches.
    pub max_concurrent_fetches: u32,
    /// Maximum fetches per second.
    pub max_fetches_per_second: f32,
    /// Per-source backoff between consecutive requests, in milliseconds.
    pub per_source_backoff_ms: u64,
}

impl RateLimits {
    /// The single, hard-coded set of rate limits. There is no other constructor.
    pub const HARD_CODED: Self = Self {
        max_concurrent_fetches: MAX_CONCURRENT_FETCHES,
        max_fetches_per_second: MAX_FETCHES_PER_SECOND,
        per_source_backoff_ms: 200,
    };
}

/// A successful TDM grant — the user has both agreed to the publisher's TDM ToS
/// (via env var) and provided an API key. See `docs/CAPABILITY.md` §3.
#[derive(Debug, Clone)]
pub struct TdmGrant {
    /// Which env var the user used to acknowledge the publisher's ToS.
    pub agree_env_var: String,
    /// When the agreement env var was first observed at startup.
    pub agreed_at: chrono::DateTime<chrono::Utc>,
    // Note: `api_key: secrecy::Secret<String>` is added when the relevant `tdm-*`
    // feature is enabled. Phase 0 does not compile any TDM source, so the field
    // type is private to the future per-feature impl.
}

/// Runtime gate for which sources may be invoked. See `docs/CAPABILITY.md`.
#[derive(Debug, Clone)]
pub struct CapabilityProfile {
    /// Tier 1 OA sources are always permitted.
    pub oa: AlwaysOn,
    /// Tier 2 metadata access (Phase 4+).
    pub metadata: MetadataAccess,
    /// Tier 3 grants are populated only when both env var and feature compile-in are set.
    pub tdm_elsevier: Option<TdmGrant>,
    /// Tier 3 grants are populated only when both env var and feature compile-in are set.
    pub tdm_aps: Option<TdmGrant>,
    /// Tier 3 grants are populated only when both env var and feature compile-in are set.
    pub tdm_springer: Option<TdmGrant>,
    /// Hard-coded rate limits for this process.
    pub rate_limits: RateLimits,
}

/// Errors that can arise during `CapabilityProfile::from_env`.
#[derive(Debug, thiserror::Error)]
pub enum CapabilityError {
    /// User set the agree env var but provided no key. See `docs/CAPABILITY.md` §2.
    #[error("env {agree_var} is set but {key_var} is missing")]
    AgreedButNoKey {
        /// The agreement env var the user set.
        agree_var: String,
        /// The key env var that should accompany it.
        key_var: String,
    },
    /// Key env var is set but user has not agreed. See `docs/CAPABILITY.md` §2.
    #[error("key for {agree_var} is present but {agree_var} is not set to '1'")]
    KeyButNotAgreed {
        /// The agreement env var the user must set to `1` before the key takes effect.
        agree_var: String,
    },
}

impl CapabilityProfile {
    /// Read the runtime profile from environment variables.
    ///
    /// Phase 0 returns a default profile (Tier 1 only) for skeleton purposes.
    /// Phase 1 will implement full env resolution per `docs/CAPABILITY.md`.
    pub fn from_env() -> Result<Self, CapabilityError> {
        Ok(Self {
            oa: AlwaysOn,
            metadata: MetadataAccess::default(),
            tdm_elsevier: None,
            tdm_aps: None,
            tdm_springer: None,
            rate_limits: RateLimits::HARD_CODED,
        })
    }
}
