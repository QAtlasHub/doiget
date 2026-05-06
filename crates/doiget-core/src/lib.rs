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
use sha2::Digest;

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

/// Maximum queued MCP requests beyond `MAX_CONCURRENT_FETCHES`. Excess returns
/// `ErrorCode::RateLimited`. See `docs/SECURITY.md` §1.4 / `docs/MCP_TOOLS.md`.
pub const MCP_QUEUE_DEPTH_MAX: usize = 100;

/// MCP server stdin-EOF graceful-shutdown deadline, in seconds. See ADR-0001
/// and `docs/MCP_TOOLS.md` §8.
pub const MCP_STDIN_EOF_SHUTDOWN_SEC: u64 = 5;

/// Maximum DOI suffix length accepted at validation. See `docs/SECURITY.md` §1.1.
pub const DOI_SUFFIX_MAX_LEN: usize = 256;

/// Maximum PDF body size accepted by the fetcher, in bytes. See
/// `docs/SECURITY.md` §1.2 (Oversized PDF).
pub const PDF_MAX_BYTES: u64 = 100_000_000;

/// Time-to-live for entries in `~/.cache/doiget/resolver/`. See
/// `docs/CACHE.md` §3.
pub const RESOLVER_CACHE_TTL_DAYS: u32 = 7;

/// Time-to-live for entries in `~/.cache/doiget/citations/`. See
/// `docs/CACHE.md` §3.
pub const CITATION_CACHE_TTL_DAYS: u32 = 30;

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
///
/// Construct via `Doi::parse(s)` (Phase 1+). The inner field is intentionally
/// `pub(crate)` to forbid bypass construction; tests inside `doiget-core` may
/// still use `Doi(s)` for fixture purposes.
///
/// Wire format: bare string (`#[serde(transparent)]`), e.g. `"10.1234/example"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Doi(pub(crate) String);

/// A validated arXiv id string.
///
/// Construct via `ArxivId::parse(s)` (Phase 1+). Inner field is `pub(crate)`.
///
/// Wire format: bare string (`#[serde(transparent)]`), e.g. `"2401.12345"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ArxivId(pub(crate) String);

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
/// Construct via `Ref::safekey()` (Phase 1+); inner field is `pub(crate)`.
///
/// Wire format: bare string (`#[serde(transparent)]`), e.g. `"doi_10.1234_example"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Safekey(pub(crate) String);

impl Safekey {
    /// Returns the safekey as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Ref {
    /// Derives a deterministic, filesystem-safe key from this reference.
    ///
    /// The algorithm is the NORMATIVE binding spec in `docs/SAFEKEY.md` §3.
    /// Both Rust and Julia implementations MUST produce bit-identical output
    /// for every entry in `tests/fixtures/safekey/vectors.json`.
    ///
    /// # Algorithm summary
    ///
    /// 1. Prefix with `doi_` or `arxiv_` (per variant).
    /// 2. Replace any character outside `[A-Za-z0-9._-]` with `_`.
    /// 3. Collapse consecutive `_` runs to a single `_`.
    /// 4. Trim leading/trailing `_`.
    /// 5. If the result exceeds 192 bytes, take the first 192 bytes plus
    ///    `_` plus the first 8 hex chars of `SHA-256(raw)` (where `raw` is
    ///    the step-1 output, before escaping).
    ///
    /// The bound on `as_str()` after step 4 is pure ASCII (steps 1-3 produce
    /// only ASCII bytes), so the byte-slice in step 5 cannot split a
    /// multibyte char.
    pub fn safekey(&self) -> Safekey {
        // Step 0: prefix per variant. Doi/ArxivId hold the bare identifier
        // (no `doi:` / `arxiv:` URI scheme — that is stripped by Ref::parse,
        // not relevant here).
        let raw = match self {
            Ref::Doi(d) => format!("doi_{}", d.as_str()),
            Ref::Arxiv(a) => format!("arxiv_{}", a.as_str()),
        };

        // Step 1: replace unsafe chars with '_'. Non-ASCII chars (emitted by
        // String::chars() as full Unicode code points) all hit the wildcard
        // arm and become a single '_'.
        let escaped: String = raw
            .chars()
            .map(|c| match c {
                'A'..='Z' | 'a'..='z' | '0'..='9' | '.' | '-' | '_' => c,
                _ => '_',
            })
            .collect();

        // Step 2: collapse consecutive '_' runs to a single '_'.
        let mut collapsed = String::with_capacity(escaped.len());
        let mut last_was_underscore = false;
        for c in escaped.chars() {
            if c == '_' {
                if !last_was_underscore {
                    collapsed.push('_');
                }
                last_was_underscore = true;
            } else {
                collapsed.push(c);
                last_was_underscore = false;
            }
        }

        // Step 3: trim leading/trailing '_'.
        let trimmed = collapsed.trim_matches('_');

        // Step 4: length-bound. After steps 1-3 `trimmed` is pure ASCII, so
        // `len()` (bytes) == char count and `&trimmed[..192]` is char-safe.
        let key = if trimmed.len() > 192 {
            let digest = sha2::Sha256::digest(raw.as_bytes());
            let hash = hex::encode(&digest[..4]);
            format!("{}_{}", &trimmed[..192], hash)
        } else {
            trimmed.to_string()
        };

        Safekey(key)
    }
}

// ---------------------------------------------------------------------------
// ErrorCode
// ---------------------------------------------------------------------------

/// The closed set of error codes doiget surfaces.
///
/// See `docs/ERRORS.md` for the persona × code matrix.
///
/// Marked `#[non_exhaustive]` so adding new variants is a minor (not major)
/// version bump.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
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
#[non_exhaustive]
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
/// Construct only via [`RateLimits::HARD_CODED`]. The struct fields are
/// `pub(crate)` so downstream code cannot synthesize a `RateLimits` with
/// different values, which would weaken `docs/LEGAL.md` §6 safeguard 8.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct RateLimits {
    pub(crate) max_concurrent_fetches: u32,
    pub(crate) max_fetches_per_second: f32,
    pub(crate) per_source_backoff_ms: u64,
}

impl RateLimits {
    /// The single, hard-coded set of rate limits. There is no other public
    /// constructor — see the type-level docs.
    pub const HARD_CODED: Self = Self {
        max_concurrent_fetches: MAX_CONCURRENT_FETCHES,
        max_fetches_per_second: MAX_FETCHES_PER_SECOND,
        per_source_backoff_ms: 200,
    };

    /// Maximum number of concurrent fetches in flight.
    pub const fn max_concurrent_fetches(&self) -> u32 {
        self.max_concurrent_fetches
    }

    /// Maximum fetch attempts per second across all sources.
    pub const fn max_fetches_per_second(&self) -> f32 {
        self.max_fetches_per_second
    }

    /// Per-source backoff in milliseconds between consecutive requests.
    pub const fn per_source_backoff_ms(&self) -> u64 {
        self.per_source_backoff_ms
    }
}

/// A successful TDM grant.
///
/// In Phase 0, the struct does not yet carry the `api_key` field that
/// `docs/CAPABILITY.md` §1 defines for Phase 1+ — but the type is marked
/// `#[non_exhaustive]` so adding `api_key: secrecy::Secret<String>` later
/// is a non-breaking change. Phase 0 callers should not construct this type
/// directly; use `CapabilityProfile::from_env()` (which today never produces
/// `Some(TdmGrant)`).
///
/// Implements `Default` so that in-crate test fixtures using
/// `TdmGrant { agree_env_var: ..., ..Default::default() }` survive future
/// field additions (e.g. `api_key` in Phase 1) without source edits.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TdmGrant {
    /// Which env var the user used to acknowledge the publisher's ToS.
    pub agree_env_var: String,
    /// When the agreement env var was first observed at startup.
    pub agreed_at: chrono::DateTime<chrono::Utc>,
}

impl Default for TdmGrant {
    fn default() -> Self {
        Self {
            agree_env_var: String::new(),
            agreed_at: chrono::Utc::now(),
        }
    }
}

/// Runtime gate for which sources may be invoked. See `docs/CAPABILITY.md`.
///
/// Marked `#[non_exhaustive]` so adding new capability classes is non-breaking.
/// Pattern-match only against the documented variants and use a wildcard arm.
///
/// **Construction**: external callers use [`CapabilityProfile::from_env()`].
/// Struct-literal construction is blocked outside this crate by
/// `#[non_exhaustive]`; this is intentional — the type's safety guarantees
/// rely on the resolution rules in `from_env`. `Default` is **not yet**
/// implemented; Phase 1 will add it once the field set stabilizes.
#[derive(Debug, Clone)]
#[non_exhaustive]
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
    /// **Phase 0 stub.** Returns a Tier-1-only profile and emits a `tracing::warn!`
    /// breadcrumb so that any environment with TDM env vars set will surface
    /// loudly in CI / logs as "Phase 0 stub did not honor your TDM env vars".
    /// Phase 1 must replace this body with the resolution algorithm specified
    /// in `docs/CAPABILITY.md` §2 and exercise both `CapabilityError` variants
    /// in tests.
    ///
    /// # Precondition: tracing subscriber must be installed first
    ///
    /// The Phase-0 audit signal is delivered via `tracing::warn!`. Callers
    /// MUST install a `tracing-subscriber` (or equivalent) **before** invoking
    /// this function, otherwise the warn is silently dropped and a
    /// misconfigured user will see no signal that their TDM env vars were
    /// ignored. The `doiget-cli` binary already does this in `main.rs`.
    ///
    /// # Warn message format (for log filtering)
    ///
    /// Each detected env var emits a `WARN`-level event with structured field
    /// `env_var = "<NAME>"` and a message starting with
    /// `"doiget-core Phase 0 stub: <NAME> is set but env-driven \
    ///  CapabilityProfile resolution is not implemented yet."`.
    /// Grep your log aggregator for `env_var=DOIGET_AGREE_TDM_*` to surface
    /// affected sessions.
    pub fn from_env() -> Result<Self, CapabilityError> {
        // Detect any Phase-1-relevant env var and warn loudly. This is
        // intentionally not a hard error in Phase 0 so that local development
        // and CI can run before Phase 1 resolution lands.
        for var in [
            "DOIGET_AGREE_TDM_ELSEVIER",
            "DOIGET_AGREE_TDM_APS",
            "DOIGET_AGREE_TDM_SPRINGER",
            "DOIGET_KEY_ELSEVIER",
            "DOIGET_KEY_APS",
            "DOIGET_KEY_SPRINGER",
        ] {
            if std::env::var_os(var).is_some() {
                tracing::warn!(
                    env_var = var,
                    "doiget-core Phase 0 stub: {} is set but env-driven \
                     CapabilityProfile resolution is not implemented yet. \
                     The TDM source will be silently disabled. Track in \
                     docs/PHASES.md and ADR-0005.",
                    var
                );
            }
        }
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

// ---------------------------------------------------------------------------
// Tests — one smoke test per legally-load-bearing constant. See
// `docs/LEGAL.md` §6 safeguard 8 and `docs/PHASES.md` §4. These also keep the
// `cargo test --workspace` job from being a false-green during Phase 0.
// ---------------------------------------------------------------------------

// `expect`/`unwrap` are idiomatic in tests where panics double as assertions.
// The workspace lints deny them in production code; relax for the test module
// only.
#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn rate_limits_hard_coded_match_legal_safeguards() {
        // docs/LEGAL.md §6 safeguard 8 names these exact values.
        assert_eq!(RateLimits::HARD_CODED.max_concurrent_fetches(), 5);
        assert!((RateLimits::HARD_CODED.max_fetches_per_second() - 5.0).abs() < f32::EPSILON);
        assert_eq!(RateLimits::HARD_CODED.per_source_backoff_ms(), 200);
    }

    #[test]
    fn batch_size_caps_match_security_doc() {
        // docs/SECURITY.md §1.4 + docs/MCP_TOOLS.md.
        assert_eq!(MCP_BATCH_MAX_SIZE, 100);
        assert_eq!(MCP_QUEUE_DEPTH_MAX, 100);
        assert_eq!(DOI_SUFFIX_MAX_LEN, 256);
        assert_eq!(MCP_STDIN_EOF_SHUTDOWN_SEC, 5);
    }

    #[test]
    fn schema_version_is_pinned_to_1_0() {
        // docs/STORE.md §3 — Phase 0/1 writes 1.0 exactly.
        // A bump to 1.1 (minor, backward-compat additions) requires updating
        // both this test and the cross-tool compat fixtures simultaneously.
        assert_eq!(SCHEMA_VERSION, "1.0");
    }

    #[test]
    fn capability_profile_from_env_is_tier_1_only_in_phase_0() {
        // Phase 0 stub guarantee: TDM is never enabled regardless of env vars.
        // Phase 1 must update this test to cover the real resolution algorithm
        // (including AgreedButNoKey / KeyButNotAgreed Err branches).
        let p = CapabilityProfile::from_env().expect("Phase 0 stub never errors");
        assert!(p.tdm_elsevier.is_none());
        assert!(p.tdm_aps.is_none());
        assert!(p.tdm_springer.is_none());
        assert_eq!(p.rate_limits.max_concurrent_fetches(), 5);
    }

    // -----------------------------------------------------------------
    // Safekey reference vectors (docs/SAFEKEY.md §3, NORMATIVE).
    //
    // The vectors.json file is the binding cross-tool contract with
    // BiblioFetch.jl: every entry MUST round-trip identically through
    // both implementations. Phase 0 ships 13 entries; the full 100-entry
    // set is gated on the BiblioFetch.jl pre-flight (ADR-0007 Status:
    // Proposed at the time of this Phase 1 implementation).
    //
    // `Ref::parse` is concurrent W3-A work and is not on `main` yet, so
    // this test branches on the input prefix (`doi:` / `arxiv:`) and
    // constructs the variant directly via the in-crate `pub(crate)`
    // tuple constructor.
    // -----------------------------------------------------------------

    #[derive(Deserialize)]
    struct SafekeyVector {
        input: String,
        expected: String,
    }

    #[derive(Deserialize)]
    struct SafekeyVectorFile {
        vectors: Vec<SafekeyVector>,
    }

    /// In-crate test helper: build a `Ref` from the user-facing form used
    /// in the vectors file, by stripping the `doi:` / `arxiv:` URI scheme
    /// and wrapping the remainder. This bypasses validation; it is fine
    /// here because the vectors are hand-curated and the test asserts the
    /// derivation algorithm, not parser semantics.
    fn ref_from_vector_input(input: &str) -> Ref {
        if let Some(rest) = input.strip_prefix("doi:") {
            Ref::Doi(Doi(rest.to_string()))
        } else if let Some(rest) = input.strip_prefix("arxiv:") {
            Ref::Arxiv(ArxivId(rest.to_string()))
        } else {
            panic!(
                "vectors.json entry has unknown ref scheme (expected doi: or arxiv: prefix): {}",
                input
            );
        }
    }

    #[test]
    fn safekey_matches_reference_vectors() {
        // include_str! resolves relative to the file containing this macro
        // call (crates/doiget-core/src/lib.rs), so we go up three levels
        // to reach the workspace root, then down to tests/fixtures.
        let raw = include_str!("../../../tests/fixtures/safekey/vectors.json");
        let parsed: SafekeyVectorFile =
            serde_json::from_str(raw).expect("vectors.json is valid JSON matching schema");

        // Phase 1 Wave 3 ships the 13-entry placeholder set. The full
        // 100-entry NORMATIVE set (docs/SAFEKEY.md §5) is a follow-up gated
        // on BiblioFetch.jl pre-flight. Use a minimum-count guard so the
        // test catches truncation but does not break when vectors.json grows.
        assert!(
            parsed.vectors.len() >= 13,
            "vectors.json has fewer entries than expected ({}); fixture may be truncated",
            parsed.vectors.len()
        );

        let mut failures: Vec<String> = Vec::new();
        for v in &parsed.vectors {
            let r = ref_from_vector_input(&v.input);
            let got = r.safekey().as_str().to_string();
            if got != v.expected {
                failures.push(format!(
                    "input={:?}\n  expected={:?}\n  got     ={:?}",
                    v.input, v.expected, got
                ));
            }
        }

        assert!(
            failures.is_empty(),
            "{}/{} safekey reference vectors failed:\n{}",
            failures.len(),
            parsed.vectors.len(),
            failures.join("\n")
        );
    }

    #[test]
    fn safekey_truncates_long_inputs_with_sha256_suffix() {
        // Construct a synthetic DOI whose suffix produces a `trimmed` longer than
        // 192 chars after step 3. 220 ASCII-safe chars + the `doi_10.1234/`
        // prefix easily exceeds 192. The resulting key must be exactly 201 chars:
        // 192 (trimmed prefix) + 1 (`_` separator) + 8 (hex of first 4 bytes of
        // SHA-256(raw)). Per docs/SAFEKEY.md §3 step 5.
        let suffix = "a".repeat(220);
        let doi = Doi(format!("10.1234/{}", suffix));
        let key = Ref::Doi(doi).safekey();
        let s = key.as_str();

        // Shape: <192 ASCII chars from {A-Za-z0-9._-}> + "_" + <8 hex chars>
        assert_eq!(
            s.len(),
            201,
            "expected 201-char truncated key, got {}: {}",
            s.len(),
            s
        );
        assert_eq!(&s[192..193], "_", "expected '_' separator at byte 192");
        let hash_part = &s[193..];
        assert_eq!(hash_part.len(), 8, "hash suffix must be 8 hex chars");
        assert!(
            hash_part
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "hash suffix must be lowercase hex: {}",
            hash_part
        );

        // Determinism: same input twice must produce the same key.
        let key2 = Ref::Doi(Doi(format!("10.1234/{}", "a".repeat(220)))).safekey();
        assert_eq!(s, key2.as_str(), "safekey must be deterministic");

        // Hash content: must equal hex(sha256(raw)[..4]) where raw is the
        // pre-escape prefixed form per docs/SAFEKEY.md §3 step 5.
        use sha2::Digest;
        let raw = format!("doi_10.1234/{}", "a".repeat(220));
        let expected_hash = {
            let digest = sha2::Sha256::digest(raw.as_bytes());
            format!(
                "{:02x}{:02x}{:02x}{:02x}",
                digest[0], digest[1], digest[2], digest[3]
            )
        };
        assert_eq!(
            hash_part, expected_hash,
            "hash must match SHA-256 of raw form"
        );
    }
}
