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

// --- Modules ---
pub mod dry_run;
pub mod http;
pub mod orchestrator;
pub mod provenance;
pub mod rate_limiter;
pub mod source;
pub mod sources;
pub mod store;

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

    /// Parses and validates a DOI string per `docs/SECURITY.md` §1.1.
    ///
    /// Accepts:
    /// - Bare DOIs: `10.<registrant>/<suffix>` where `<registrant>` is 4–9
    ///   digits and `<suffix>` is a non-empty sequence of characters drawn
    ///   from `[A-Za-z0-9._/()-]`.
    /// - The `doi:` URI scheme prefix; it is stripped before validation, so
    ///   the stored value never carries a scheme. (Matches the convention
    ///   established in `docs/SAFEKEY.md` §3 step 0.)
    ///
    /// Rejects:
    /// - Inputs missing the literal `10.` prefix (after optional scheme
    ///   strip).
    /// - Suffixes longer than [`DOI_SUFFIX_MAX_LEN`] bytes.
    /// - Empty suffixes.
    /// - Any character outside the suffix charset above (including control
    ///   characters, whitespace, and non-ASCII).
    ///
    /// # Errors
    ///
    /// Returns a [`RefParseError`] variant that names the specific rejection
    /// category. Tier 1+ callers should map any [`RefParseError`] to
    /// [`ErrorCode::InvalidRef`] when surfacing to MCP / CLI.
    pub fn parse(s: &str) -> Result<Self, RefParseError> {
        let stripped = parse::strip_doi_scheme(s);
        parse::validate_doi(stripped)?;
        Ok(Doi(stripped.to_string()))
    }
}

impl ArxivId {
    /// Returns the arXiv id as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Parses and validates an arXiv id per `docs/SECURITY.md` §1.1 and the
    /// pattern published in `docs/MCP_TOOLS.md`.
    ///
    /// Accepts:
    /// - New-style ids: `YYMM.NNNNN[vN]` where the date block is 4 digits, the
    ///   sequence number is 4–5 digits, and the optional version `vN` is one
    ///   or more digits. Examples: `2401.12345`, `2401.12345v2`.
    /// - Old-style ids: `subject-class/YYMMNNN[vN]` where the subject class
    ///   is a lowercase token (with optional internal hyphens and an
    ///   optional `.XX` two-uppercase-letter group), and the numeric body
    ///   is exactly 7 digits with optional `vN`. Examples:
    ///   `cond-mat/9501001`, `astro-ph.CO/0703123v2`.
    /// - The `arxiv:` / `arXiv:` URI scheme prefix; it is stripped before
    ///   validation.
    ///
    /// Rejects:
    /// - Inputs that match neither the new-style nor old-style shape.
    /// - Inputs containing characters outside the per-shape charset
    ///   (control chars, whitespace, non-ASCII).
    /// - Empty input.
    ///
    /// # Errors
    ///
    /// Returns a [`RefParseError`] variant that names the specific rejection
    /// category.
    pub fn parse(s: &str) -> Result<Self, RefParseError> {
        let stripped = parse::strip_arxiv_scheme(s);
        parse::validate_arxiv(stripped)?;
        Ok(ArxivId(stripped.to_string()))
    }
}

impl Ref {
    /// Parses a string into a [`Ref`], auto-detecting DOI vs arXiv.
    ///
    /// Detection rules:
    /// 1. If the input begins with the case-insensitive `doi:` scheme, the
    ///    remainder is parsed as a DOI.
    /// 2. If the input begins with the `arxiv:` or `arXiv:` scheme, the
    ///    remainder is parsed as an arXiv id.
    /// 3. Otherwise, if the input starts with `10.` it is treated as a bare
    ///    DOI; this matches the heuristic in `docs/SAFEKEY.md` §4 (Julia
    ///    reference) and is stable because DOIs always begin `10.`.
    /// 4. Failing all of the above, parsing falls back to arXiv.
    ///
    /// The returned [`Ref`] never carries the URI scheme — `as_str()` on the
    /// inner `Doi` / `ArxivId` is always the bare identifier.
    ///
    /// # Errors
    ///
    /// Returns a [`RefParseError`] from the underlying [`Doi::parse`] or
    /// [`ArxivId::parse`] call. When the input has an explicit scheme
    /// (`doi:` / `arxiv:`), the matching parser is dispatched and its error
    /// surfaces directly. When the input is bare and ambiguous, the
    /// heuristic in rule 3/4 selects the parser; an unparsable bare input
    /// surfaces the arXiv parser's error (a non-`10.` ref that also fails
    /// arXiv validation is never a valid DOI).
    pub fn parse(s: &str) -> Result<Self, RefParseError> {
        // Reject empty up front so all three parsers see a meaningful slice;
        // without this, `strip_*_scheme("")` returns "" and we'd get a
        // confusing "missing 10. prefix" error for empty input.
        if s.is_empty() {
            return Err(RefParseError::Empty);
        }

        if parse::has_doi_scheme(s) {
            return Doi::parse(s).map(Ref::Doi);
        }
        if parse::has_arxiv_scheme(s) {
            return ArxivId::parse(s).map(Ref::Arxiv);
        }
        if s.starts_with("10.") {
            return Doi::parse(s).map(Ref::Doi);
        }
        ArxivId::parse(s).map(Ref::Arxiv)
    }
}

// ---------------------------------------------------------------------------
// Parser internals
// ---------------------------------------------------------------------------

mod parse {
    use super::{RefParseError, DOI_SUFFIX_MAX_LEN};

    /// Case-insensitive `doi:` prefix detector. Matches both `doi:` and
    /// `DOI:` (and any case mix); the spec in `docs/SAFEKEY.md` §3 only
    /// names the lowercase form, but the field convention is to be lenient
    /// in what we accept (the scheme is dropped at the boundary anyway).
    pub(crate) fn has_doi_scheme(s: &str) -> bool {
        s.len() >= 4 && s.is_char_boundary(4) && s[..4].eq_ignore_ascii_case("doi:")
    }

    /// Case-insensitive `arxiv:` prefix detector. Accepts `arxiv:`,
    /// `arXiv:` (the form used in `docs/MCP_TOOLS.md`), and any other case
    /// mix.
    pub(crate) fn has_arxiv_scheme(s: &str) -> bool {
        s.len() >= 6 && s.is_char_boundary(6) && s[..6].eq_ignore_ascii_case("arxiv:")
    }

    pub(crate) fn strip_doi_scheme(s: &str) -> &str {
        if has_doi_scheme(s) {
            &s[4..]
        } else {
            s
        }
    }

    pub(crate) fn strip_arxiv_scheme(s: &str) -> &str {
        if has_arxiv_scheme(s) {
            &s[6..]
        } else {
            s
        }
    }

    /// DOI suffix charset per `docs/SECURITY.md` §1.1:
    /// `[A-Za-z0-9._/()-]`. The forward slash is permitted inside the
    /// suffix (e.g. `10.1016/...`); the registrant separator is the
    /// *first* `/` and the suffix is everything after it.
    fn is_doi_suffix_char(c: char) -> bool {
        matches!(c,
            'A'..='Z' | 'a'..='z' | '0'..='9'
            | '.' | '_' | '/' | '(' | ')' | '-'
        )
    }

    pub(crate) fn validate_doi(s: &str) -> Result<(), RefParseError> {
        if s.is_empty() {
            return Err(RefParseError::Empty);
        }

        // Must begin with literal "10."; the registrant is 4–9 digits up
        // to the first '/'. After that, everything is suffix.
        let rest = s
            .strip_prefix("10.")
            .ok_or(RefParseError::MissingDoiPrefix)?;
        let slash_idx = rest
            .find('/')
            .ok_or(RefParseError::MissingDoiSuffixSeparator)?;
        let registrant = &rest[..slash_idx];
        let suffix = &rest[slash_idx + 1..];

        // Registrant: 4–9 ASCII digits.
        if registrant.len() < 4
            || registrant.len() > 9
            || !registrant.chars().all(|c| c.is_ascii_digit())
        {
            return Err(RefParseError::InvalidDoiRegistrant);
        }

        // Suffix: non-empty, charset-restricted, length-bounded.
        if suffix.is_empty() {
            return Err(RefParseError::EmptyDoiSuffix);
        }
        if suffix.len() > DOI_SUFFIX_MAX_LEN {
            return Err(RefParseError::DoiSuffixTooLong {
                len: suffix.len(),
                max: DOI_SUFFIX_MAX_LEN,
            });
        }
        if let Some(bad) = suffix.chars().find(|c| !is_doi_suffix_char(*c)) {
            return Err(RefParseError::InvalidDoiSuffixChar { ch: bad });
        }
        Ok(())
    }

    /// Validates an arXiv id (with the `arxiv:` / `arXiv:` scheme already
    /// stripped). Tries the new-style shape first, then the old-style.
    pub(crate) fn validate_arxiv(s: &str) -> Result<(), RefParseError> {
        if s.is_empty() {
            return Err(RefParseError::Empty);
        }
        if validate_arxiv_new(s).is_ok() || validate_arxiv_old(s).is_ok() {
            return Ok(());
        }
        Err(RefParseError::InvalidArxivShape)
    }

    /// New-style arXiv id: `YYMM.NNNNN[vN]`.
    fn validate_arxiv_new(s: &str) -> Result<(), ()> {
        let dot_idx = s.find('.').ok_or(())?;
        let head = &s[..dot_idx];
        let tail = &s[dot_idx + 1..];

        // Head: exactly 4 ASCII digits.
        if head.len() != 4 || !head.chars().all(|c| c.is_ascii_digit()) {
            return Err(());
        }

        // Tail: 4–5 digits, then optional `v` followed by ≥1 digits.
        let bytes = tail.as_bytes();
        let mut i = 0;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        let digits_len = i;
        if !(4..=5).contains(&digits_len) {
            return Err(());
        }
        if i == bytes.len() {
            return Ok(());
        }
        // Optional version suffix.
        if bytes[i] != b'v' {
            return Err(());
        }
        i += 1;
        let v_start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i == v_start || i != bytes.len() {
            return Err(());
        }
        Ok(())
    }

    /// Old-style arXiv id: `subject-class/YYMMNNN[vN]`.
    /// Subject class: `[a-z]([a-z-]*[a-z])?(\.[A-Z]{2})?`.
    fn validate_arxiv_old(s: &str) -> Result<(), ()> {
        let slash_idx = s.find('/').ok_or(())?;
        let class = &s[..slash_idx];
        let id = &s[slash_idx + 1..];

        // Class: starts with [a-z], body is [a-z-], optional `.XX` (two
        // ASCII upper).
        let (core_class, dot_part) = match class.find('.') {
            Some(d) => (&class[..d], Some(&class[d + 1..])),
            None => (class, None),
        };
        if core_class.is_empty()
            || !core_class
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == '-')
            || core_class.starts_with('-')
            || core_class.ends_with('-')
        {
            return Err(());
        }
        if let Some(dp) = dot_part {
            if dp.len() != 2 || !dp.chars().all(|c| c.is_ascii_uppercase()) {
                return Err(());
            }
        }

        // Id: 7 digits, optional `vN`.
        let bytes = id.as_bytes();
        let mut i = 0;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i != 7 {
            return Err(());
        }
        if i == bytes.len() {
            return Ok(());
        }
        if bytes[i] != b'v' {
            return Err(());
        }
        i += 1;
        let v_start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i == v_start || i != bytes.len() {
            return Err(());
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// RefParseError
// ---------------------------------------------------------------------------

/// Reasons a `Doi::parse` / `ArxivId::parse` / `Ref::parse` call can fail.
///
/// Each variant maps to one rejection category in `docs/SECURITY.md` §1.1.
/// All variants funnel to [`ErrorCode::InvalidRef`] when surfacing to MCP /
/// CLI; the granular shape is preserved for tests and for future log
/// breadcrumbs. The `From<RefParseError> for ErrorCode` impl below makes
/// `?` propagation collapse to `INVALID_REF` automatically, satisfying
/// `docs/PUBLIC_API.md` §4.
///
/// Marked `#[non_exhaustive]` so adding new categories is a non-breaking
/// change. Pattern-match with a wildcard arm.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RefParseError {
    /// Input was empty.
    #[error("empty input")]
    Empty,
    /// Input did not begin with the required `10.` literal (after any
    /// scheme strip).
    #[error("DOI must begin with '10.'")]
    MissingDoiPrefix,
    /// Input started with `10.` but had no `/` separator between
    /// registrant and suffix.
    #[error("DOI must contain '/' between registrant and suffix")]
    MissingDoiSuffixSeparator,
    /// Registrant was not 4–9 ASCII digits.
    #[error("DOI registrant must be 4–9 ASCII digits")]
    InvalidDoiRegistrant,
    /// DOI suffix was empty.
    #[error("DOI suffix is empty")]
    EmptyDoiSuffix,
    /// DOI suffix exceeded `DOI_SUFFIX_MAX_LEN` bytes.
    #[error("DOI suffix is {len} bytes; maximum is {max}")]
    DoiSuffixTooLong {
        /// Observed suffix length, in bytes.
        len: usize,
        /// Hard upper bound (always [`DOI_SUFFIX_MAX_LEN`]).
        max: usize,
    },
    /// DOI suffix contained a character outside `[A-Za-z0-9._/()-]`.
    #[error("DOI suffix contains invalid character {ch:?}")]
    InvalidDoiSuffixChar {
        /// The first offending character.
        ch: char,
    },
    /// Input matched neither the new-style nor old-style arXiv shape.
    #[error("input does not match any known arXiv id shape")]
    InvalidArxivShape,
}

impl From<RefParseError> for ErrorCode {
    fn from(_: RefParseError) -> Self {
        // All parse failures collapse to INVALID_REF at the public boundary,
        // matching `docs/PUBLIC_API.md` §4 and `docs/SECURITY.md` §1.1.
        ErrorCode::InvalidRef
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
    /// Returns the bare identifier string usable as a provenance `ref` field.
    ///
    /// Equivalent to `Doi::as_str` / `ArxivId::as_str` dispatched on the
    /// variant — the URI scheme (`doi:` / `arxiv:`) is never present in the
    /// inner identifiers (it is stripped at parse time), so the result is
    /// always the bare DOI or arXiv id. Used by the CLI / MCP orchestrators
    /// to populate the `ref` column of provenance log rows
    /// (`docs/PROVENANCE_LOG.md` §3) without re-matching the variant.
    pub fn as_input_str(&self) -> &str {
        match self {
            Ref::Doi(d) => d.as_str(),
            Ref::Arxiv(a) => a.as_str(),
        }
    }

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
    /// Feature is spec'd but not yet wired in this Phase. Distinct from
    /// [`Self::InternalError`] (which signals a bug) and
    /// [`Self::CapabilityDenied`] (which signals a runtime config gate).
    /// Returned by stubs that exist to pin the public surface ahead of
    /// orchestrator implementation, so an agent can react with "wait for
    /// next minor release" rather than "report a bug" or "tweak my
    /// capability profile". Wire form: `"NOT_IMPLEMENTED"`.
    NotImplemented,
}

// ---------------------------------------------------------------------------
// DenialReason / DenialContext (ADR-0023)
// ---------------------------------------------------------------------------

/// Closed-set reasons a denial-class error envelope can carry on its
/// optional `denial_context.reason` field.
///
/// Wire form (JSON / MCP) is `snake_case` — e.g. `"redirect_not_in_allowlist"`.
/// The set is **closed** per ADR-0023 §2: adding a new variant is a minor
/// semver bump; renaming or repurposing one is a breaking change. Mirrors
/// the stability rule that already governs [`ErrorCode`].
///
/// See [`DenialContext`] for the surrounding struct, `docs/ERRORS.md` §3.1
/// for the wire surface, and `docs/PUBLIC_API.md` §8 for the
/// semver-locked surface contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DenialReason {
    /// Redirect target host did not match the source's allowlist
    /// (`HttpError::RedirectDenied`).
    RedirectNotInAllowlist,
    /// Redirect target had a non-HTTPS scheme (`HttpError::InsecureRedirect`).
    InsecureScheme,
    /// Source produced a URL whose host is on a future blocklist (reserved).
    HostInBlockList,
    /// Body exceeded [`PDF_MAX_BYTES`] (`HttpError::OversizedBody`).
    SizeCapExceeded,
    /// Store entry's `schema_version` is ahead of this binary
    /// (reserved — emitted by the store schema-rejection path).
    SchemaDrift,
    /// Source not in the runtime [`CapabilityProfile`]
    /// (`FetchError::NotEligible`).
    CapabilityNotGranted,
    /// Rate limiter rejected the call inside the current window
    /// (reserved — Phase 2+ surfaces this from the limiter).
    RateLimitWindow,
    /// SSRF guard rejected a private / link-local / cloud-metadata address
    /// (reserved — future SSRF check).
    SsrfPrivateAddress,
    /// Response Content-Type / magic-byte mismatch (`HttpError::NotAPdf`).
    ContentTypeMismatch,
}

/// Structured machine-parseable companion to `error.message` for
/// recoverable denials.
///
/// The field is **optional and additive** on the public error envelope —
/// every previously-shipped `{code, message}` envelope remains valid, and
/// agents that ignore this struct continue to work. When present, it
/// carries the concrete parameters an LLM agent can use to plan a recovery
/// (e.g. "the redirect to `evil.example.com` was denied because it is not
/// in the crossref allowlist") without text-mining `error.message`.
///
/// ## Wire shape
///
/// `#[serde(deny_unknown_fields)]`: forward-compatible field additions on
/// the wire are forbidden by design — adding a field to this struct is a
/// **breaking** change. This is why the type is **not** `#[non_exhaustive]`
/// (per `docs/PUBLIC_API.md` §8): both production rules — Rust struct
/// construction outside the crate AND wire-level extension — must agree.
///
/// All fields except `reason` are optional. Producers populate the fields
/// relevant to the reason and leave the rest at `None`; consumers MUST
/// tolerate any subset of fields being present. Optional fields are
/// skipped on serialize but accepted as missing on deserialize via
/// `#[serde(default, skip_serializing_if = "Option::is_none")]`.
///
/// [`Self::expected`] is `Option<Vec<String>>` rather than `Vec<String>`
/// so the producer can distinguish "this reason has no allowlist channel"
/// (`None` → field absent on the wire) from "this is the explicit list of
/// acceptable values, possibly empty" (`Some(vec![])` → `"expected":[]` on
/// the wire). The previous `Vec<String>` shape collapsed both states
/// into "field omitted", which an LLM agent could not safely disambiguate.
///
/// Mapping table: see ADR-0023 §4, plus the
/// `From<&HttpError> for Option<DenialContext>` and
/// `From<&FetchError> for Option<DenialContext>` impls in
/// [`crate::http`] / [`crate::source`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DenialContext {
    /// Closed-enum reason code; the only required field.
    pub reason: DenialReason,
    /// Resolver source key (e.g. `"crossref"`) when one is in scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Concrete value the producer attempted (host, path, hex magic bytes,
    /// scheme prefix). Shape is reason-specific; consumers MUST treat it
    /// as opaque text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempted: Option<String>,
    /// Allowlist entries / acceptable values. `Option<Vec<String>>` so the
    /// producer can distinguish "this reason has no allowlist channel"
    /// (`None`, field absent on the wire) from "this is the explicit list
    /// of acceptable values, possibly empty" (`Some(vec![])`, `"expected":[]`
    /// on the wire). The inner `Vec<String>` is used even when only one
    /// value is meaningful (e.g. `Some(vec!["%PDF-".into()])`) so the
    /// format does not have to flip when multiple values are acceptable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<Vec<String>>,
    /// Redirect-chain hop position, 0-indexed. `u8` because the chain is
    /// hard-capped at [`crate::http`]'s `MAX_REDIRECTS` (= 10) and any
    /// larger value indicates a bug.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hop_index: Option<u8>,
    /// Size or rate cap value (e.g. [`PDF_MAX_BYTES`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cap: Option<u64>,
    /// Observed value (e.g. response bytes when [`Self::cap`] is the byte
    /// cap, or row schema_version when [`Self::cap`] is the binary's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual: Option<u64>,
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
    /// Implements the resolution algorithm specified in
    /// [`docs/CAPABILITY.md`](../../../docs/CAPABILITY.md) §2.
    ///
    /// # Tier 1 (Open Access)
    ///
    /// Always permitted; not gated on any env var or feature.
    ///
    /// # Tier 2 (metadata)
    ///
    /// Each metadata source becomes available when its env var is set
    /// (presence-checked, value ignored) **and** the `metadata` Cargo feature
    /// was compiled in. If the env var is set but the feature is not compiled
    /// in, a `tracing::warn!` is emitted and the source is left disabled —
    /// this is not an error so that users can move binaries between machines
    /// (or switch feature sets between cargo invocations) without breaking
    /// startup. See `docs/CAPABILITY.md` §3 for the env var list.
    ///
    /// # Tier 3 (TDM)
    ///
    /// For each publisher in `{ELSEVIER, APS, SPRINGER}`, the
    /// `DOIGET_AGREE_TDM_<X>` agreement env var is paired with
    /// `DOIGET_KEY_<X>`. Resolution rules (per `docs/CAPABILITY.md` §2):
    ///
    /// - both unset → `tdm_<x> = None` (no error);
    /// - `agree == "1"` and key set → `Some(TdmGrant { .. })` (subject to the
    ///   feature gate below);
    /// - `agree == "1"` and key unset → [`CapabilityError::AgreedButNoKey`];
    /// - key set but `agree` unset (or `agree != "1"`) →
    ///   [`CapabilityError::KeyButNotAgreed`].
    ///
    /// When both env vars are set correctly **but** the corresponding
    /// `tdm-<x>` Cargo feature is not compiled in, this function emits a
    /// `tracing::warn!` and sets the grant to `None` rather than returning an
    /// error — same rationale as for the Tier 2 warn-and-skip behavior.
    ///
    /// # Precondition: tracing subscriber must be installed first
    ///
    /// Warn breadcrumbs are delivered via `tracing::warn!`. Callers MUST
    /// install a `tracing-subscriber` (or equivalent) **before** invoking
    /// this function, otherwise warnings are silently dropped. The
    /// `doiget-cli` binary does this in `main.rs`.
    ///
    /// # Errors
    ///
    /// Returns [`CapabilityError::AgreedButNoKey`] or
    /// [`CapabilityError::KeyButNotAgreed`] when the TDM env-var pair for any
    /// publisher is misconfigured. See the variant docs for the precise
    /// trigger conditions.
    ///
    /// # Note on `api_key` storage
    ///
    /// The [`TdmGrant`] struct does not yet carry an `api_key` field — it is
    /// a marker that the user agreed and supplied a key, but the key value is
    /// not threaded through `CapabilityProfile` at this phase. Sources that
    /// need the key read it from the same env var directly. See the
    /// [`TdmGrant`] doc-comment for the rationale and roadmap.
    pub fn from_env() -> Result<Self, CapabilityError> {
        // TODO Phase 1+: thread api_key through TdmGrant. The key would be
        // read here and stored as `secrecy::Secret<String>`; deferred for now
        // because adding a field to `TdmGrant` requires a coordinated
        // `secrecy` design (the dep is `optional = true` and only compiled in
        // under `tdm-*` features). See the `TdmGrant` doc-comment above and
        // `docs/CAPABILITY.md` §1.

        // -- Tier 2 metadata -------------------------------------------------
        let metadata = MetadataAccess {
            openalex: resolve_metadata_flag(
                "DOIGET_ENABLE_OPENALEX",
                "metadata",
                cfg!(feature = "metadata"),
            ),
            semantic_scholar: resolve_metadata_flag(
                "DOIGET_ENABLE_S2",
                "metadata",
                cfg!(feature = "metadata"),
            ),
            doaj: resolve_metadata_flag(
                "DOIGET_ENABLE_DOAJ",
                "metadata",
                cfg!(feature = "metadata"),
            ),
        };

        // -- Tier 3 TDM grants ----------------------------------------------
        let tdm_elsevier = resolve_tdm_grant(
            "DOIGET_AGREE_TDM_ELSEVIER",
            "DOIGET_KEY_ELSEVIER",
            "tdm-elsevier",
            cfg!(feature = "tdm-elsevier"),
        )?;
        let tdm_aps = resolve_tdm_grant(
            "DOIGET_AGREE_TDM_APS",
            "DOIGET_KEY_APS",
            "tdm-aps",
            cfg!(feature = "tdm-aps"),
        )?;
        let tdm_springer = resolve_tdm_grant(
            "DOIGET_AGREE_TDM_SPRINGER",
            "DOIGET_KEY_SPRINGER",
            "tdm-springer",
            cfg!(feature = "tdm-springer"),
        )?;

        Ok(Self {
            oa: AlwaysOn,
            metadata,
            tdm_elsevier,
            tdm_aps,
            tdm_springer,
            rate_limits: RateLimits::HARD_CODED,
        })
    }
}

/// Resolve a Tier 2 metadata flag from its env var and compile-in feature.
///
/// Returns `true` only when both the env var is present and the feature is
/// compiled in. When the env var is set without the feature, emits a
/// `tracing::warn!` and returns `false` — see [`CapabilityProfile::from_env`]
/// for the rationale (binaries may move between hosts / feature sets).
fn resolve_metadata_flag(env_var: &str, feature: &str, feature_enabled: bool) -> bool {
    let env_set = std::env::var_os(env_var).is_some();
    match (env_set, feature_enabled) {
        (true, true) => true,
        (true, false) => {
            tracing::warn!(
                env_var,
                feature,
                "{} is set but feature {} was not compiled in; the source will be unavailable",
                env_var,
                feature
            );
            false
        }
        (false, _) => false,
    }
}

/// Resolve a Tier 3 TDM grant from the `agree`/`key` env-var pair and the
/// per-publisher Cargo feature.
///
/// Implements the rules in `docs/CAPABILITY.md` §2:
///
/// - both unset → `Ok(None)`.
/// - `agree == "1"` and `key` set → `Ok(Some(TdmGrant { .. }))` (when the
///   feature is enabled), or warn-and-`Ok(None)` (when the feature is not
///   compiled in).
/// - `agree == "1"` and `key` unset →
///   [`CapabilityError::AgreedButNoKey`].
/// - `key` set and `agree` unset OR `agree` set to anything other than `"1"`
///   → [`CapabilityError::KeyButNotAgreed`].
fn resolve_tdm_grant(
    agree_var: &str,
    key_var: &str,
    feature: &str,
    feature_enabled: bool,
) -> Result<Option<TdmGrant>, CapabilityError> {
    // `agree` is "agreed" iff the value is exactly the literal "1"; any other
    // value (including "true", "yes", empty) is treated as not-agreed per
    // `docs/CAPABILITY.md` §2.
    let agree_raw = std::env::var(agree_var).ok();
    let agreed = matches!(agree_raw.as_deref(), Some("1"));
    let agree_present = agree_raw.is_some();
    // The key value itself is not stored in `TdmGrant` at this phase; we only
    // care whether it is set. See the TODO in `from_env`.
    let key_present = std::env::var_os(key_var).is_some();

    match (agreed, agree_present, key_present) {
        (true, _, true) => {
            if feature_enabled {
                Ok(Some(TdmGrant {
                    agree_env_var: agree_var.to_string(),
                    agreed_at: chrono::Utc::now(),
                }))
            } else {
                tracing::warn!(
                    env_var = agree_var,
                    feature,
                    "{} is set but feature {} was not compiled in; the source will be unavailable",
                    agree_var,
                    feature
                );
                Ok(None)
            }
        }
        (true, _, false) => Err(CapabilityError::AgreedButNoKey {
            agree_var: agree_var.to_string(),
            key_var: key_var.to_string(),
        }),
        // agree set to non-"1", key also set: KeyButNotAgreed (the key would
        // otherwise authorize the source without an explicit agreement).
        (false, true, true) => Err(CapabilityError::KeyButNotAgreed {
            agree_var: agree_var.to_string(),
        }),
        // agree unset, key set: KeyButNotAgreed (same rule).
        (false, false, true) => Err(CapabilityError::KeyButNotAgreed {
            agree_var: agree_var.to_string(),
        }),
        // agree set to non-"1" and no key: treat as no-grant. The user
        // expressed something but did not opt in and provided no credential,
        // so silent skip is the safe default (no source enabled).
        (false, true, false) => Ok(None),
        // Neither env var set: no grant, no error.
        (false, false, false) => Ok(None),
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

    // -----------------------------------------------------------------
    // CapabilityProfile::from_env — Phase 1 resolution algorithm tests.
    //
    // These tests mutate process-global env state via std::env::set_var /
    // remove_var, so each test holds an `EnvGuard` RAII drop guard that
    // captures the pre-test value of every env var it touches and restores
    // it on drop (even on panic). They also use `#[serial_test::serial]` so
    // that no two tests in this module touch env state concurrently — the
    // workspace's test runner defaults to multi-threaded.
    //
    // Spec: docs/CAPABILITY.md §2 (resolution algorithm) and §3 (env var
    // reference table).
    // -----------------------------------------------------------------

    /// RAII guard that captures the prior value of an env var on construction
    /// and restores it on drop. Use one guard per touched var per test.
    struct EnvGuard {
        var: &'static str,
        prior: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        /// Capture and clear `var`. Use `set` afterwards to install a value.
        fn unset(var: &'static str) -> Self {
            let prior = std::env::var_os(var);
            // SAFETY (env mutation): tests are serialized via
            // `#[serial_test::serial]`. `remove_var` is sound when no other
            // thread reads or writes the environment concurrently.
            std::env::remove_var(var);
            EnvGuard { var, prior }
        }

        /// Capture, then set `var` to `value`.
        fn set(var: &'static str, value: &str) -> Self {
            let prior = std::env::var_os(var);
            std::env::set_var(var, value);
            EnvGuard { var, prior }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.prior {
                Some(v) => std::env::set_var(self.var, v),
                None => std::env::remove_var(self.var),
            }
        }
    }

    /// Convenience: unset every Tier 2 / Tier 3 env var the resolution
    /// algorithm reads, returning a vector of guards that restore them on
    /// drop. Callers can then `EnvGuard::set` individual vars on top.
    fn unset_all_capability_env_vars() -> Vec<EnvGuard> {
        [
            "DOIGET_ENABLE_OPENALEX",
            "DOIGET_ENABLE_S2",
            "DOIGET_ENABLE_DOAJ",
            "DOIGET_AGREE_TDM_ELSEVIER",
            "DOIGET_KEY_ELSEVIER",
            "DOIGET_AGREE_TDM_APS",
            "DOIGET_KEY_APS",
            "DOIGET_AGREE_TDM_SPRINGER",
            "DOIGET_KEY_SPRINGER",
        ]
        .iter()
        .map(|v| EnvGuard::unset(v))
        .collect()
    }

    #[test]
    #[serial_test::serial]
    fn from_env_no_env_vars_set_returns_tier_1_only() {
        // Rule: with every relevant env var unset, the resolved profile has
        // all TDM grants `None` and all metadata flags `false`. Hard-coded
        // rate limits still apply. (Replaces the old Phase 0 stub test.)
        let _g = unset_all_capability_env_vars();

        let p = CapabilityProfile::from_env().expect("clean env never errors");
        assert!(p.tdm_elsevier.is_none());
        assert!(p.tdm_aps.is_none());
        assert!(p.tdm_springer.is_none());
        assert!(!p.metadata.openalex);
        assert!(!p.metadata.semantic_scholar);
        assert!(!p.metadata.doaj);
        assert_eq!(p.rate_limits.max_concurrent_fetches(), 5);
    }

    #[test]
    #[serial_test::serial]
    fn from_env_no_tdm_returns_tier_1_profile() {
        // Rule (CAPABILITY.md §2): with every TDM env var unset, all
        // `tdm_*` fields are `None` and no error is produced.
        let _g = unset_all_capability_env_vars();

        let p = CapabilityProfile::from_env().expect("no TDM env -> Ok");
        assert!(p.tdm_elsevier.is_none());
        assert!(p.tdm_aps.is_none());
        assert!(p.tdm_springer.is_none());
    }

    #[test]
    #[serial_test::serial]
    fn from_env_agreed_but_no_key_errs() {
        // Rule (CAPABILITY.md §2): agree=1 + key unset -> AgreedButNoKey.
        let _g = unset_all_capability_env_vars();
        let _agree = EnvGuard::set("DOIGET_AGREE_TDM_ELSEVIER", "1");

        let result = CapabilityProfile::from_env();
        match result {
            Err(CapabilityError::AgreedButNoKey { agree_var, key_var }) => {
                assert_eq!(agree_var, "DOIGET_AGREE_TDM_ELSEVIER");
                assert_eq!(key_var, "DOIGET_KEY_ELSEVIER");
            }
            other => panic!("expected AgreedButNoKey, got {:?}", other),
        }
    }

    #[test]
    #[serial_test::serial]
    fn from_env_key_but_not_agreed_errs() {
        // Rule (CAPABILITY.md §2): key set + agree unset -> KeyButNotAgreed.
        // A leaked DOIGET_KEY_ELSEVIER must not silently enable a source.
        let _g = unset_all_capability_env_vars();
        let _key = EnvGuard::set("DOIGET_KEY_ELSEVIER", "sk-test");

        let result = CapabilityProfile::from_env();
        match result {
            Err(CapabilityError::KeyButNotAgreed { agree_var }) => {
                assert_eq!(agree_var, "DOIGET_AGREE_TDM_ELSEVIER");
            }
            other => panic!("expected KeyButNotAgreed, got {:?}", other),
        }
    }

    #[test]
    #[serial_test::serial]
    fn from_env_agree_not_one_errs() {
        // Rule (CAPABILITY.md §2): the agree var must be exactly "1". Any
        // other value (here: "true") is treated as not-agreed; combined
        // with a key set, that triggers KeyButNotAgreed.
        let _g = unset_all_capability_env_vars();
        let _agree = EnvGuard::set("DOIGET_AGREE_TDM_ELSEVIER", "true");
        let _key = EnvGuard::set("DOIGET_KEY_ELSEVIER", "sk-test");

        let result = CapabilityProfile::from_env();
        match result {
            Err(CapabilityError::KeyButNotAgreed { agree_var }) => {
                assert_eq!(agree_var, "DOIGET_AGREE_TDM_ELSEVIER");
            }
            other => panic!("expected KeyButNotAgreed, got {:?}", other),
        }
    }

    #[test]
    #[serial_test::serial]
    fn from_env_both_set_correctly_returns_grant() {
        // Rule (CAPABILITY.md §2): agree=1 + key set -> Some(TdmGrant) when
        // the corresponding feature is compiled in; else None (warn-and-skip).
        // The feature gate for elsevier is `tdm-elsevier`; this test asserts
        // both branches via `cfg!`.
        let _g = unset_all_capability_env_vars();
        let _agree = EnvGuard::set("DOIGET_AGREE_TDM_ELSEVIER", "1");
        let _key = EnvGuard::set("DOIGET_KEY_ELSEVIER", "sk-test");

        let p = CapabilityProfile::from_env().expect("agree=1 + key -> Ok");

        if cfg!(feature = "tdm-elsevier") {
            let grant = p
                .tdm_elsevier
                .as_ref()
                .expect("feature tdm-elsevier compiled in -> Some(TdmGrant)");
            assert_eq!(grant.agree_env_var, "DOIGET_AGREE_TDM_ELSEVIER");
        } else {
            assert!(
                p.tdm_elsevier.is_none(),
                "feature tdm-elsevier NOT compiled in -> None (warn-and-skip)"
            );
        }
    }

    #[test]
    #[serial_test::serial]
    fn from_env_metadata_env_warns_without_feature() {
        // Rule (CAPABILITY.md §2): metadata env var without the `metadata`
        // feature -> source disabled (warn-and-skip, not an error).
        // We don't capture the tracing warn here; we just assert the field
        // is `false` when the feature is absent and `true` when present.
        let _g = unset_all_capability_env_vars();
        let _enable = EnvGuard::set("DOIGET_ENABLE_OPENALEX", "1");

        let p = CapabilityProfile::from_env().expect("metadata env never errors");

        if cfg!(feature = "metadata") {
            assert!(p.metadata.openalex);
        } else {
            assert!(!p.metadata.openalex);
        }
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

    // -----------------------------------------------------------------
    // Doi::parse / ArxivId::parse / Ref::parse — Phase 1 W3-A.
    // Spec: docs/SECURITY.md §1.1 (input validation). The rejection
    // category set is the binding contract; each test case below names
    // which rule it exercises in a comment.
    // -----------------------------------------------------------------

    // ---- Doi::parse happy paths (≥6) --------------------------------

    #[test]
    fn doi_parse_accepts_bare_canonical_form() {
        // Rule: "10.<registrant>/<suffix>" is the canonical bare form.
        let d = Doi::parse("10.1234/example").expect("canonical bare DOI");
        assert_eq!(d.as_str(), "10.1234/example");
    }

    #[test]
    fn doi_parse_accepts_doi_uri_scheme() {
        // Rule: the `doi:` scheme is stripped at construction; as_str
        // never carries it (matches docs/SAFEKEY.md §3 step 0).
        let d = Doi::parse("doi:10.1234/example").expect("doi: scheme accepted");
        assert_eq!(d.as_str(), "10.1234/example");
    }

    #[test]
    fn doi_parse_accepts_complex_real_world_suffix() {
        // Rule: suffix charset includes `.`, `(`, `)`, `-`. From a real
        // PhysRevLett DOI used elsewhere in the test fixture set.
        let d = Doi::parse("10.1103/PhysRevLett.130.200601").expect("real-world PhysRev DOI");
        assert_eq!(d.as_str(), "10.1103/PhysRevLett.130.200601");
    }

    #[test]
    fn doi_parse_accepts_parens_in_suffix() {
        // Rule: `(` and `)` are explicitly listed in the spec charset.
        let d = Doi::parse("10.1016/S0370-1573(98)00122-3").expect("parens in suffix");
        assert_eq!(d.as_str(), "10.1016/S0370-1573(98)00122-3");
    }

    #[test]
    fn doi_parse_accepts_nested_slashes_in_suffix() {
        // Rule: `/` is a suffix character; only the first `/` is the
        // registrant/suffix separator.
        let d = Doi::parse("10.1234/foo/bar/baz").expect("nested slashes");
        assert_eq!(d.as_str(), "10.1234/foo/bar/baz");
    }

    #[test]
    fn doi_parse_accepts_suffix_at_max_len_boundary() {
        // Rule: a suffix of exactly DOI_SUFFIX_MAX_LEN bytes is accepted;
        // 1 byte more is rejected (covered separately below).
        let suffix = "a".repeat(DOI_SUFFIX_MAX_LEN);
        let input = format!("10.1234/{}", suffix);
        let d = Doi::parse(&input).expect("suffix at max len");
        assert_eq!(d.as_str().len(), "10.1234/".len() + DOI_SUFFIX_MAX_LEN);
    }

    #[test]
    fn doi_parse_uri_scheme_is_case_insensitive() {
        // Rule: be lenient on scheme casing; the scheme is stripped
        // either way so the stored form is identical.
        let d = Doi::parse("DOI:10.1234/example").expect("uppercase scheme");
        assert_eq!(d.as_str(), "10.1234/example");
    }

    // ---- Doi::parse rejection paths (≥6) ----------------------------

    #[test]
    fn doi_parse_rejects_missing_10_prefix() {
        // Rule: must start with "10." literal.
        assert_eq!(
            Doi::parse("11.1234/example"),
            Err(RefParseError::MissingDoiPrefix)
        );
    }

    #[test]
    fn doi_parse_rejects_empty_input() {
        // Rule: empty inputs are not valid DOIs.
        assert_eq!(Doi::parse(""), Err(RefParseError::Empty));
    }

    #[test]
    fn doi_parse_rejects_missing_suffix_separator() {
        // Rule: must contain a `/` between registrant and suffix.
        assert_eq!(
            Doi::parse("10.1234"),
            Err(RefParseError::MissingDoiSuffixSeparator)
        );
    }

    #[test]
    fn doi_parse_rejects_empty_suffix() {
        // Rule: suffix must be non-empty.
        assert_eq!(Doi::parse("10.1234/"), Err(RefParseError::EmptyDoiSuffix));
    }

    #[test]
    fn doi_parse_rejects_invalid_registrant_too_short() {
        // Rule: registrant must be 4–9 digits.
        assert_eq!(
            Doi::parse("10.12/example"),
            Err(RefParseError::InvalidDoiRegistrant)
        );
    }

    #[test]
    fn doi_parse_rejects_non_digit_registrant() {
        // Rule: registrant chars must all be ASCII digits.
        assert_eq!(
            Doi::parse("10.12ab/example"),
            Err(RefParseError::InvalidDoiRegistrant)
        );
    }

    #[test]
    fn doi_parse_rejects_control_char_in_suffix() {
        // Rule (from docs/SECURITY.md §1.1, log-injection mitigation):
        // control chars are not in the suffix charset; reject before they
        // can reach the provenance log.
        let result = Doi::parse("10.1234/foo\nbar");
        assert!(
            matches!(
                result,
                Err(RefParseError::InvalidDoiSuffixChar { ch: '\n' })
            ),
            "got {:?}",
            result
        );
    }

    #[test]
    fn doi_parse_rejects_suffix_over_max_len() {
        // Rule: DOI_SUFFIX_MAX_LEN + 1 bytes is rejected.
        let suffix = "a".repeat(DOI_SUFFIX_MAX_LEN + 1);
        let input = format!("10.1234/{}", suffix);
        let result = Doi::parse(&input);
        match result {
            Err(RefParseError::DoiSuffixTooLong { len, max }) => {
                assert_eq!(len, DOI_SUFFIX_MAX_LEN + 1);
                assert_eq!(max, DOI_SUFFIX_MAX_LEN);
            }
            other => panic!("expected DoiSuffixTooLong, got {:?}", other),
        }
    }

    #[test]
    fn doi_parse_rejects_non_ascii_in_suffix() {
        // Rule: spec charset is ASCII-only; non-ASCII becomes an
        // InvalidDoiSuffixChar (consistent with safekey behavior of
        // collapsing such chars to '_', which is a downstream concern).
        let result = Doi::parse("10.1234/物理学");
        assert!(
            matches!(result, Err(RefParseError::InvalidDoiSuffixChar { .. })),
            "got {:?}",
            result
        );
    }

    // ---- ArxivId::parse happy paths (≥6) ----------------------------

    #[test]
    fn arxiv_parse_accepts_new_style_4_digit_seq() {
        // Rule: new-style YYMM.NNNN (4-digit sequence number).
        let a = ArxivId::parse("0704.0001").expect("new-style 4-digit seq");
        assert_eq!(a.as_str(), "0704.0001");
    }

    #[test]
    fn arxiv_parse_accepts_new_style_5_digit_seq() {
        // Rule: new-style YYMM.NNNNN (5-digit sequence number, post-2015).
        let a = ArxivId::parse("2401.12345").expect("new-style 5-digit seq");
        assert_eq!(a.as_str(), "2401.12345");
    }

    #[test]
    fn arxiv_parse_accepts_new_style_with_version() {
        // Rule: optional `vN` version suffix.
        let a = ArxivId::parse("2401.12345v2").expect("with version");
        assert_eq!(a.as_str(), "2401.12345v2");
    }

    #[test]
    fn arxiv_parse_accepts_old_style() {
        // Rule: old-style subject-class/YYMMNNN.
        let a = ArxivId::parse("cond-mat/9501001").expect("old-style cond-mat");
        assert_eq!(a.as_str(), "cond-mat/9501001");
    }

    #[test]
    fn arxiv_parse_accepts_old_style_with_subclass_and_version() {
        // Rule: old-style subject-class may have a `.XX` two-upper subclass
        // and an optional `vN` suffix.
        let a = ArxivId::parse("astro-ph.CO/0703123v2").expect("old-style with subclass + version");
        assert_eq!(a.as_str(), "astro-ph.CO/0703123v2");
    }

    #[test]
    fn arxiv_parse_accepts_arxiv_uri_scheme() {
        // Rule: `arxiv:` / `arXiv:` scheme is stripped at construction.
        let a = ArxivId::parse("arxiv:2401.12345").expect("arxiv: scheme");
        assert_eq!(a.as_str(), "2401.12345");
    }

    #[test]
    fn arxiv_parse_accepts_arxiv_uri_scheme_mixed_case() {
        // Rule: scheme case-insensitive; matches the `arXiv:` form named
        // in docs/MCP_TOOLS.md.
        let a = ArxivId::parse("arXiv:2401.12345v2").expect("arXiv: scheme");
        assert_eq!(a.as_str(), "2401.12345v2");
    }

    // ---- ArxivId::parse rejection paths (≥6) ------------------------

    #[test]
    fn arxiv_parse_rejects_empty_input() {
        // Rule: empty rejected up-front.
        assert_eq!(ArxivId::parse(""), Err(RefParseError::Empty));
    }

    #[test]
    fn arxiv_parse_rejects_no_dot_or_slash() {
        // Rule: must contain `.` (new-style) or `/` (old-style).
        assert_eq!(
            ArxivId::parse("notanarxivid"),
            Err(RefParseError::InvalidArxivShape)
        );
    }

    #[test]
    fn arxiv_parse_rejects_new_style_wrong_head_length() {
        // Rule: head must be exactly 4 digits.
        assert_eq!(
            ArxivId::parse("240.12345"),
            Err(RefParseError::InvalidArxivShape)
        );
    }

    #[test]
    fn arxiv_parse_rejects_new_style_seq_too_short() {
        // Rule: seq must be 4–5 digits.
        assert_eq!(
            ArxivId::parse("2401.123"),
            Err(RefParseError::InvalidArxivShape)
        );
    }

    #[test]
    fn arxiv_parse_rejects_old_style_wrong_id_length() {
        // Rule: old-style id is exactly 7 digits.
        assert_eq!(
            ArxivId::parse("cond-mat/95001"),
            Err(RefParseError::InvalidArxivShape)
        );
    }

    #[test]
    fn arxiv_parse_rejects_invalid_version_suffix() {
        // Rule: version suffix is `v` followed by ≥1 digits, nothing else.
        assert_eq!(
            ArxivId::parse("2401.12345v"),
            Err(RefParseError::InvalidArxivShape)
        );
    }

    #[test]
    fn arxiv_parse_rejects_control_char() {
        // Rule (docs/SECURITY.md §1.1 log-injection): no control chars.
        assert_eq!(
            ArxivId::parse("2401.12345\n"),
            Err(RefParseError::InvalidArxivShape)
        );
    }

    #[test]
    fn arxiv_parse_rejects_non_ascii() {
        // Rule: ASCII-only.
        assert_eq!(
            ArxivId::parse("2401.物理"),
            Err(RefParseError::InvalidArxivShape)
        );
    }

    // ---- Ref::parse happy paths (≥6) --------------------------------

    #[test]
    fn ref_parse_dispatches_doi_scheme_to_doi() {
        // Detection rule 1: explicit `doi:` scheme.
        match Ref::parse("doi:10.1234/example").expect("doi: dispatched to Doi") {
            Ref::Doi(d) => assert_eq!(d.as_str(), "10.1234/example"),
            other => panic!("expected Ref::Doi, got {:?}", other),
        }
    }

    #[test]
    fn ref_parse_dispatches_arxiv_scheme_to_arxiv() {
        // Detection rule 2: explicit `arxiv:` scheme.
        match Ref::parse("arxiv:2401.12345").expect("arxiv: dispatched to Arxiv") {
            Ref::Arxiv(a) => assert_eq!(a.as_str(), "2401.12345"),
            other => panic!("expected Ref::Arxiv, got {:?}", other),
        }
    }

    #[test]
    fn ref_parse_dispatches_arxiv_mixed_case_scheme() {
        // Detection rule 2 (case-insensitive): `arXiv:` form.
        match Ref::parse("arXiv:cond-mat/9501001").expect("arXiv: dispatched") {
            Ref::Arxiv(a) => assert_eq!(a.as_str(), "cond-mat/9501001"),
            other => panic!("expected Ref::Arxiv, got {:?}", other),
        }
    }

    #[test]
    fn ref_parse_bare_doi_resolves_to_doi() {
        // Detection rule 3: bare input starting with `10.` is a DOI.
        match Ref::parse("10.1234/foo").expect("bare DOI") {
            Ref::Doi(d) => assert_eq!(d.as_str(), "10.1234/foo"),
            other => panic!("expected Ref::Doi, got {:?}", other),
        }
    }

    #[test]
    fn ref_parse_bare_arxiv_new_resolves_to_arxiv() {
        // Detection rule 4: bare input not starting with `10.` falls
        // through to arXiv. Tests the ambiguous-input branch named in the
        // PR brief: `2401.12345` should resolve to ArxivId.
        match Ref::parse("2401.12345").expect("bare new-style arXiv") {
            Ref::Arxiv(a) => assert_eq!(a.as_str(), "2401.12345"),
            other => panic!("expected Ref::Arxiv, got {:?}", other),
        }
    }

    #[test]
    fn ref_parse_bare_arxiv_old_resolves_to_arxiv() {
        // Detection rule 4: bare old-style arXiv id.
        match Ref::parse("cond-mat/9501001").expect("bare old-style arXiv") {
            Ref::Arxiv(a) => assert_eq!(a.as_str(), "cond-mat/9501001"),
            other => panic!("expected Ref::Arxiv, got {:?}", other),
        }
    }

    // ---- Ref::parse rejection paths (≥6) ----------------------------

    #[test]
    fn ref_parse_rejects_empty() {
        // Rule: empty up-front.
        assert_eq!(Ref::parse(""), Err(RefParseError::Empty));
    }

    #[test]
    fn ref_parse_doi_scheme_with_invalid_doi_propagates_doi_error() {
        // When the scheme is explicit, we surface the parser's error
        // verbatim — not a generic "shape mismatch".
        assert_eq!(
            Ref::parse("doi:10.1234"),
            Err(RefParseError::MissingDoiSuffixSeparator)
        );
    }

    #[test]
    fn ref_parse_arxiv_scheme_with_invalid_arxiv_propagates_arxiv_error() {
        assert_eq!(
            Ref::parse("arxiv:notanid"),
            Err(RefParseError::InvalidArxivShape)
        );
    }

    #[test]
    fn ref_parse_bare_with_10_prefix_uses_doi_errors() {
        // Bare `10.…` heuristic: DOI parser is dispatched and its error
        // surfaces (here: bad registrant).
        assert_eq!(
            Ref::parse("10.12/x"),
            Err(RefParseError::InvalidDoiRegistrant)
        );
    }

    #[test]
    fn ref_parse_bare_without_10_prefix_uses_arxiv_errors() {
        // Bare ambiguous fallback: ArxivId parser is dispatched and its
        // error surfaces. `1.2.3` is neither a DOI nor an arXiv shape.
        assert_eq!(Ref::parse("1.2.3"), Err(RefParseError::InvalidArxivShape));
    }

    #[test]
    fn ref_parse_rejects_doi_scheme_with_oversized_suffix() {
        // Length-bound: DOI suffix > DOI_SUFFIX_MAX_LEN through Ref::parse
        // surfaces DoiSuffixTooLong, not a generic InvalidArxivShape.
        let suffix = "a".repeat(DOI_SUFFIX_MAX_LEN + 5);
        let input = format!("doi:10.1234/{}", suffix);
        match Ref::parse(&input) {
            Err(RefParseError::DoiSuffixTooLong { .. }) => {}
            other => panic!("expected DoiSuffixTooLong, got {:?}", other),
        }
    }

    #[test]
    fn ref_parse_round_trip_via_serde_preserves_inner_string() {
        // Wire-format check: Doi/ArxivId are #[serde(transparent)], and a
        // round-trip through Ref::parse → serde_json → Ref must preserve
        // the inner identifier. Guards against accidental scheme leakage
        // into the stored form.
        let r = Ref::parse("doi:10.1234/example").expect("parse ok");
        let json = serde_json::to_string(&r).expect("serialize");
        // The transparent inner value is the bare identifier (no `doi:`).
        assert!(
            json.contains("10.1234/example") && !json.contains("doi:"),
            "scheme leaked into wire form: {}",
            json
        );
    }

    #[test]
    fn ref_parse_error_maps_to_invalid_ref_error_code() {
        // Public-API contract (docs/PUBLIC_API.md §4): all parse failures
        // collapse to ErrorCode::InvalidRef at the public boundary.
        let err: ErrorCode = RefParseError::Empty.into();
        assert_eq!(err, ErrorCode::InvalidRef);
        let err2: ErrorCode = RefParseError::MissingDoiPrefix.into();
        assert_eq!(err2, ErrorCode::InvalidRef);
    }

    // -----------------------------------------------------------------
    // DenialReason / DenialContext (ADR-0023) — wire-shape tests.
    // -----------------------------------------------------------------

    #[test]
    fn denial_reason_serializes_snake_case() {
        // ADR-0023 §2 / docs/PUBLIC_API.md §8: wire form is snake_case.
        let s = serde_json::to_string(&DenialReason::RedirectNotInAllowlist).expect("ser");
        assert_eq!(s, "\"redirect_not_in_allowlist\"");
        let s = serde_json::to_string(&DenialReason::SizeCapExceeded).expect("ser");
        assert_eq!(s, "\"size_cap_exceeded\"");
        let s = serde_json::to_string(&DenialReason::ContentTypeMismatch).expect("ser");
        assert_eq!(s, "\"content_type_mismatch\"");
    }

    #[test]
    fn denial_reason_round_trip_via_serde() {
        // Round-trip every closed-set variant so adding a new variant
        // forces this test to be updated (the closed-set contract).
        for r in [
            DenialReason::RedirectNotInAllowlist,
            DenialReason::InsecureScheme,
            DenialReason::HostInBlockList,
            DenialReason::SizeCapExceeded,
            DenialReason::SchemaDrift,
            DenialReason::CapabilityNotGranted,
            DenialReason::RateLimitWindow,
            DenialReason::SsrfPrivateAddress,
            DenialReason::ContentTypeMismatch,
        ] {
            let s = serde_json::to_string(&r).expect("ser");
            let back: DenialReason = serde_json::from_str(&s).expect("de");
            assert_eq!(back, r, "round-trip mismatch for {:?} -> {}", r, s);
        }
    }

    #[test]
    fn denial_context_round_trips_full_shape() {
        // A populated context (the redirect-denied case from ADR-0023 §1
        // example) survives a JSON round-trip. Whole-struct equality
        // exercises the `PartialEq` derive added per ADR-0023 §3 (added
        // in the multi-agent review feedback PR — see ADR-0023 history).
        let dc = DenialContext {
            reason: DenialReason::RedirectNotInAllowlist,
            source: Some("crossref".to_string()),
            attempted: Some("evil.example.com".to_string()),
            expected: Some(vec![
                "api.crossref.org".to_string(),
                "*.crossref.org".to_string(),
            ]),
            hop_index: Some(1),
            cap: None,
            actual: None,
        };
        let s = serde_json::to_string(&dc).expect("ser");
        let back: DenialContext = serde_json::from_str(&s).expect("de");
        assert_eq!(back, dc);
    }

    #[test]
    fn denial_context_serialize_elides_empty_fields() {
        // `skip_serializing_if = "Option::is_none"` must keep the wire form
        // lean: every `None` field MUST NOT appear on the wire. Reason is
        // always present.
        let dc = DenialContext {
            reason: DenialReason::CapabilityNotGranted,
            source: None,
            attempted: None,
            expected: None,
            hop_index: None,
            cap: None,
            actual: None,
        };
        let s = serde_json::to_string(&dc).expect("ser");
        assert_eq!(s, "{\"reason\":\"capability_not_granted\"}");
    }

    #[test]
    fn denial_context_expected_some_empty_vec_preserves_explicit_empty_allowlist() {
        // Post-refinement disambiguation: `expected: Some(vec![])` is the
        // "explicit empty allowlist" signal and MUST survive the wire as
        // `"expected":[]`. Only `expected: None` is skipped on serialize.
        // This is the bug the previous `Vec<String>` shape masked.
        let dc = DenialContext {
            reason: DenialReason::RedirectNotInAllowlist,
            source: Some("crossref".to_string()),
            attempted: Some("evil.example.com".to_string()),
            expected: Some(Vec::new()),
            hop_index: None,
            cap: None,
            actual: None,
        };
        let s = serde_json::to_string(&dc).expect("ser");
        assert!(
            s.contains("\"expected\":[]"),
            "expected:[] must survive on the wire (got: {s})"
        );
        let back: DenialContext = serde_json::from_str(&s).expect("de");
        assert_eq!(back.expected, Some(Vec::new()));
    }

    #[test]
    fn denial_context_deserialize_tolerates_missing_optional_fields() {
        // Consumer-side contract (ADR-0023 §3): consumers MUST tolerate
        // any subset of fields being present. Missing optional fields
        // deserialize to their defaults via `#[serde(default)]`.
        let wire = r#"{"reason":"size_cap_exceeded","cap":104857600,"actual":209715200}"#;
        let dc: DenialContext = serde_json::from_str(wire).expect("de");
        assert_eq!(dc.reason, DenialReason::SizeCapExceeded);
        assert_eq!(dc.cap, Some(104857600));
        assert_eq!(dc.actual, Some(209715200));
        assert!(dc.source.is_none());
        assert!(dc.attempted.is_none());
        assert!(dc.expected.is_none());
        assert!(dc.hop_index.is_none());
    }

    #[test]
    fn full_error_envelope_with_denial_context_serializes_to_pinned_json() {
        // Pins the byte-exact wire shape of the full failure envelope
        // documented in docs/ERRORS.md §3 + §3.1 and ADR-0023 §1. A
        // future regression that flips key order or skip-rules anywhere
        // in the chain breaks this test loudly.
        //
        // Note: serde_json's `Map` (used by `json!`) sorts keys
        // alphabetically when the `preserve_order` feature is NOT
        // enabled (we do not enable it). Embedding a `DenialContext`
        // via `json!` first re-serialises it through the same alphabet-
        // sorted Map path, so the inner field order is also alphabetical
        // here — NOT the struct field-order produced by direct
        // `to_string(&DenialContext)`. This is by design: the public
        // wire shape is canonicalised by serde_json's Map ordering, so
        // the byte-exact pin below documents that exact canonicalisation.
        let denial = DenialContext {
            reason: DenialReason::RedirectNotInAllowlist,
            source: Some("crossref".into()),
            attempted: Some("evil.example.com".into()),
            expected: Some(vec!["api.crossref.org".into(), "*.crossref.org".into()]),
            hop_index: Some(1),
            cap: None,
            actual: None,
        };
        let envelope = serde_json::json!({
            "ok": false,
            "error": {
                "code": ErrorCode::NetworkError,
                "message": "redirect target evil.example.com not in allowlist for source crossref",
                "denial_context": denial,
            }
        });
        let actual = serde_json::to_string(&envelope).expect("serialize envelope");
        let expected = r#"{"error":{"code":"NETWORK_ERROR","denial_context":{"attempted":"evil.example.com","expected":["api.crossref.org","*.crossref.org"],"hop_index":1,"reason":"redirect_not_in_allowlist","source":"crossref"},"message":"redirect target evil.example.com not in allowlist for source crossref"},"ok":false}"#;
        assert_eq!(actual, expected);
    }

    #[test]
    fn denial_context_rejects_unknown_fields() {
        // `#[serde(deny_unknown_fields)]` (ADR-0023 §3, PUBLIC_API.md §8):
        // an unknown field on the wire MUST be a deserialize error so
        // forward-compat field additions stay a breaking change.
        let wire = r#"{"reason":"capability_not_granted","banana":1}"#;
        let result: Result<DenialContext, _> = serde_json::from_str(wire);
        assert!(
            result.is_err(),
            "deny_unknown_fields must reject 'banana': {:?}",
            result.map(|d| d.reason),
        );
    }
}
