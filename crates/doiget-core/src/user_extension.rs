//! User-extensible capability gate (ADR-0028, #220).
//!
//! The single reader for the user's `config.toml`. Parses
//! `[[network.additional_hosts]]`, the two `[network]` trust flags, and
//! `[store] root` (#441) — one opener, so no two commands can disagree
//! about which file they are describing.
//!
//! Parses the `[[network.additional_hosts]]` array-of-tables from the
//! user's `config.toml`, validates each entry against the restricted
//! ADR-0028 D2-1 pattern grammar, and merges the user-added hosts into
//! the orchestrator's `oa-publisher` [`crate::http::SourceAllowlist`].
//!
//! This module is intentionally minimal — it does NOT layer
//! `config.toml` with env vars or implement the full
//! `docs/CONFIG.md` resolution ladder. The full reader is a separate
//! slice (S3b); this slice ships only the surface needed for
//! ADR-0028 D2 (user-extensible allowlist) to actually work end-to-end.
//!
//! # Wire contract (ADR-0028 D2-1)
//!
//! ```toml
//! [[network.additional_hosts]]
//! host = "ruj.uj.edu.pl"
//! note = "Jagiellonian University Repository — Green OA"
//!
//! [[network.additional_hosts]]
//! host = "*.uj.edu.pl"          # single-suffix wildcard, allowed
//! ```
//!
//! Every rejection class enforced by [`validate_pattern`]:
//!
//! - empty string
//! - leading or trailing whitespace
//! - bare wildcard (`*`)
//! - multi-segment glob (`*.edu.*`, `*.*`, `*.foo.*`)
//! - mid-string wildcard (`foo.*.org`, `f*o.bar`, `*foo.bar`)
//! - no `.` (single-label hostnames)
//! - non-host characters (`@`, `/`, `:`, port suffixes, scheme prefix)
//! - empty leading / inner / trailing label
//! - label starting or ending with `-`
//!
//! Each rejection maps to a [`PatternError`] variant for downstream
//! consumers (S3b `doiget config doctor`) to branch on programmatically.
//!
//! # Provenance & doctor (deferred to S3b)
//!
//! ADR-0028 D2-2 / D2-3 / D2-4 (the `verified_by = "user"` provenance
//! field, `doiget config doctor` surface, `doiget capabilities`
//! `user_extension_count`) ship in the S3b follow-up. The
//! MCP-server-side merge in `doiget-mcp` is also deferred — this
//! slice wires user extensions only through the CLI production path
//! (`commands::fetch::build_http_client`). A user who configures
//! `[[network.additional_hosts]]` and invokes via `doiget serve`
//! (MCP) currently sees no effect; that is the load-bearing item
//! S3b closes.

use camino::{Utf8Path, Utf8PathBuf};
use serde::Deserialize;

use crate::http::SourceAllowlist;

/// A validated host pattern from `[[network.additional_hosts]]`.
///
/// Construction goes through [`HostPattern::new`] (or the
/// `TryFrom<&str>` / `TryFrom<String>` impls), which run
/// [`validate_pattern`] internally. The serde `Deserialize` impl
/// also goes through `TryFrom<String>`, so any path that produces a
/// `HostPattern` value — including TOML deserialisation — has
/// passed validation. The invariant "this is a syntactically valid
/// ADR-0028 D2-1 pattern" is therefore type-level.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HostPattern(String);

impl HostPattern {
    /// Construct after validation. Most callers prefer the
    /// `TryFrom` impls.
    ///
    /// # Errors
    ///
    /// Returns [`PatternError`] for any input that fails
    /// [`validate_pattern`].
    pub fn new(raw: impl Into<String>) -> Result<Self, PatternError> {
        let s: String = raw.into();
        validate_pattern(&s)?;
        Ok(Self(s))
    }

    /// Borrow the validated pattern as a `&str`.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for HostPattern {
    type Error = PatternError;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<String> for HostPattern {
    type Error = PatternError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl<'de> serde::Deserialize<'de> for HostPattern {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

/// One user-added host entry from `[[network.additional_hosts]]`.
///
/// The `note` field is free-text user documentation (e.g.
/// "Jagiellonian University Repository — Green OA"); it is recorded
/// in the provenance log alongside the host (S3b) but never consulted
/// for matching.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[non_exhaustive]
pub struct UserExtensionHost {
    /// Validated host pattern (literal FQDN or single-suffix
    /// wildcard `*.foo.bar`). Construction is type-enforced via
    /// [`HostPattern`].
    pub host: HostPattern,
    /// Optional free-text note.
    #[serde(default)]
    pub note: Option<String>,
}

impl UserExtensionHost {
    /// Test-only constructor. Production callers go through [`load`].
    #[cfg(test)]
    #[allow(clippy::expect_used)]
    pub(crate) fn for_test(host: &str) -> Self {
        Self {
            host: HostPattern::new(host).expect("test host must be valid"),
            note: None,
        }
    }
}

/// Closed-enum classification of a single pattern's rejection
/// reason. Used by [`validate_pattern`] and carried as the `kind`
/// field on [`InvalidPatternIssue`]. S3b's `doiget config doctor`
/// surface branches on this for actionable per-error rendering.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum PatternError {
    /// Empty string.
    #[error("empty pattern")]
    Empty,
    /// Leading or trailing whitespace.
    #[error("pattern has leading or trailing whitespace")]
    Whitespace,
    /// Bare `*` with no leading-segment suffix.
    #[error("bare wildcard `*` is not allowed")]
    BareWildcard,
    /// `*` appearing anywhere other than the very first character
    /// followed by `.`.
    #[error("wildcard `*` is only allowed as the first character followed by `.`")]
    MisplacedWildcard,
    /// Multi-segment glob (a second `*` after stripping `*.` prefix).
    #[error("multi-segment globs are not allowed; use a single `*.<suffix>`")]
    MultiSegmentGlob,
    /// Nothing after the `*.` prefix (`*.` alone).
    #[error("nothing after wildcard prefix `*.`")]
    EmptySuffix,
    /// No `.` in the host portion (single-label hostnames rejected).
    #[error("host must contain at least one `.`")]
    NoDot,
    /// A label is empty (consecutive `.`s or leading/trailing `.`).
    #[error("empty label (consecutive `.` or leading/trailing `.`)")]
    EmptyLabel,
    /// A label starts or ends with `-`.
    #[error("label `{label}` starts or ends with `-`")]
    LabelHyphenBorder {
        /// The offending label.
        label: String,
    },
    /// A label contains a character outside the allowed host charset.
    #[error("label `{label}` contains a non-host character (allowed: A-Z a-z 0-9 - .)")]
    BadChar {
        /// The offending label.
        label: String,
    },
}

/// Errors from loading `config.toml`'s user-extension section.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum UserExtensionError {
    /// Filesystem read failed (other than "not found", which is
    /// treated as "no user extensions" — see [`load`]).
    #[error("io reading {path}: {source}")]
    Io {
        /// The path we tried to read.
        path: String,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// TOML deserialisation failed (malformed file).
    #[error("toml parse of {path}: {source}")]
    Parse {
        /// The path that failed to parse.
        path: String,
        /// The underlying TOML deserialiser error.
        #[source]
        source: toml::de::Error,
    },
    /// One or more `host` patterns failed grammar validation per
    /// ADR-0028 D2-1. The vec collects every offending entry so the
    /// user sees all errors in a single pass rather than fixing them
    /// one at a time (review pass I6).
    #[error("invalid host pattern(s) in {path}: {issues:?}")]
    InvalidPatterns {
        /// The path containing the offending entries.
        path: String,
        /// All rejected patterns in this file.
        issues: Vec<InvalidPatternIssue>,
    },
}

/// One per-entry rejection inside [`UserExtensionError::InvalidPatterns`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct InvalidPatternIssue {
    /// The raw pattern string from the TOML.
    pub pattern: String,
    /// Closed-enum rejection reason.
    pub kind: PatternError,
}

impl std::fmt::Display for InvalidPatternIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "`{}`: {}", self.pattern, self.kind)
    }
}

/// Load user-extension hosts from a `config.toml` path.
///
/// Returns an empty `Vec` if the path does not exist (the user simply
/// has not extended the gate). Returns `Err` only when the file
/// exists but cannot be read, parsed, or contains invalid pattern(s).
///
/// Every entry in the returned vec has its `host` already validated
/// at the type level via [`HostPattern`].
///
/// # Errors
///
/// - [`UserExtensionError::Io`] for filesystem errors other than
///   not-found.
/// - [`UserExtensionError::Parse`] for malformed TOML.
/// - [`UserExtensionError::InvalidPatterns`] for invalid patterns;
///   the variant carries *every* offending entry, not just the first
///   (review pass I6).
pub fn load(config_path: &Utf8Path) -> Result<UserExtensionConfig, UserExtensionError> {
    let text = match std::fs::read_to_string(config_path.as_std_path()) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(UserExtensionConfig::default())
        }
        Err(e) => {
            return Err(UserExtensionError::Io {
                path: config_path.to_string(),
                source: e,
            })
        }
    };
    parse_str(&text, config_path)
}

/// Raw TOML shape used internally so we can collect every bad
/// pattern in a single pass (vs failing on the first via
/// `HostPattern`'s deserialize). The outer config / network tables
/// tolerate unknown keys via `IgnoredAny`-flatten — the S3b full
/// reader will own `deny_unknown_fields` on a complete schema.
#[derive(Debug, Default, Deserialize)]
struct RawConfig {
    #[serde(default)]
    network: Option<RawNetwork>,
    #[serde(default)]
    store: Option<RawStore>,
    #[serde(flatten)]
    _other: serde::de::IgnoredAny,
}

/// `[store]` — read here rather than by a second reader so there is exactly
/// one answer to "which config.toml" (#441). A separate opener is how
/// `doiget fetch` and `doiget config doctor` once disagreed about the file
/// they were describing.
#[derive(Debug, Default, Deserialize)]
struct RawStore {
    /// `[store] root` — the central library path. Documented in
    /// `docs/CONFIG.md` §3 and ADR-0036 since 0.7, and parsed by nobody
    /// until #441: `resolve_store_root` read the env var and then fell
    /// straight through to the cwd default, so the file rung the docs,
    /// `config init` and `config doctor` all pointed at did nothing.
    #[serde(default)]
    root: Option<String>,
    #[serde(flatten)]
    _other: serde::de::IgnoredAny,
}

#[derive(Debug, Default, Deserialize)]
struct RawNetwork {
    #[serde(default)]
    additional_hosts: Vec<RawHost>,
    /// `[network] trust_academic_repos = true` — activates the built-in curated
    /// academic institution allowlist (issue #323). See [`academic_repo_hosts`].
    #[serde(default)]
    trust_academic_repos: bool,
    /// `[network] trust_oa_registries = true` — activates the built-in curated
    /// open-access registry allowlist (issue #405). See [`oa_registry_hosts`].
    #[serde(default)]
    trust_oa_registries: bool,
    #[serde(flatten)]
    _other: serde::de::IgnoredAny,
}

/// The `[[network.additional_hosts]]` table itself is the load-
/// bearing schema and DOES forbid unknown keys — that's the level
/// where typos like `hsot = "..."` should fail loudly.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHost {
    host: String,
    #[serde(default)]
    note: Option<String>,
}

/// Parsed result from a `config.toml` user-extension section (issue #323).
///
/// Returned by [`load`]; callers merge [`Self::additional_hosts`] into the
/// allowlist unconditionally, and optionally merge [`academic_repo_hosts`]
/// when [`Self::trust_academic_repos`] is `true`.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct UserExtensionConfig {
    /// Hosts from `[[network.additional_hosts]]`.
    pub additional_hosts: Vec<UserExtensionHost>,
    /// `true` when `[network] trust_academic_repos = true` is set in
    /// `config.toml`. Callers that respect this flag SHOULD merge
    /// [`academic_repo_hosts`] into their allowlist.
    pub trust_academic_repos: bool,
    /// `true` when `[network] trust_oa_registries = true` is set in
    /// `config.toml`. Callers that respect this flag SHOULD merge
    /// [`oa_registry_hosts`] into their allowlist.
    pub trust_oa_registries: bool,
    /// `[store] root`, verbatim, when present and non-empty (#441).
    ///
    /// Returned as the raw string rather than a path: expanding it needs a
    /// home directory, and this crate has no home-directory dependency.
    /// The CLI and MCP resolvers own that step, and both place this rung
    /// below `DOIGET_STORE_ROOT` and above the cwd default (ADR-0036).
    pub store_root: Option<String>,
}

/// Returns the built-in curated set of academic institution host patterns.
///
/// Activated via `[network] trust_academic_repos = true` in `config.toml`
/// (issue #323). Each entry is a single-suffix wildcard matching the
/// TLD-style registration block used by institutions in that country.
///
/// All patterns are valid per [`validate_pattern`] (single-suffix wildcards
/// only; no multi-segment globs). The set is intentionally conservative —
/// covering major national academic TLD patterns used for institutional
/// Green OA repositories — while keeping the security posture minimal.
pub fn academic_repo_hosts() -> Vec<UserExtensionHost> {
    const PATTERNS: &[(&str, &str)] = &[
        ("*.ac.uk", "UK academic institutions (Universities UK)"),
        ("*.ac.jp", "Japanese academic institutions (NII)"),
        ("*.jst.go.jp", "J-STAGE / JST academic platform (Japan)"),
        ("*.edu.au", "Australian universities (TEQSA)"),
        ("*.edu.cn", "Chinese universities (MoE)"),
        ("*.ac.cn", "Chinese academic institutions"),
        ("*.edu.pl", "Polish universities (MEiN)"),
        ("*.ac.nz", "New Zealand universities"),
        ("*.ac.za", "South African universities (DHET)"),
        ("*.ac.in", "Indian academic institutions"),
        ("*.edu.br", "Brazilian universities (CAPES)"),
        ("*.edu.tw", "Taiwanese universities (MoE)"),
        ("*.edu.tr", "Turkish universities (YÖK)"),
        ("*.edu.ar", "Argentine universities (SPU)"),
        ("*.edu.mx", "Mexican universities (SEP)"),
    ];
    PATTERNS
        .iter()
        .filter_map(|(pat, note)| {
            HostPattern::new(*pat).ok().map(|host| UserExtensionHost {
                host,
                note: Some(note.to_string()),
            })
        })
        .collect()
}

/// Returns the built-in curated set of open-access **registry** host
/// patterns.
///
/// Activated via `[network] trust_oa_registries = true` in `config.toml`
/// (issue #405). Companion to [`academic_repo_hosts`], and deliberately a
/// separate flag: that set is "institutions publish their own Green OA
/// here", this one is "cross-publisher registries and repositories index
/// or host Gold OA here". They are different trust arguments, so a user
/// can take either without the other.
///
/// Why this exists: the default allowlist covers publishers, so a Green-OA
/// copy on an institutional repository was reachable behind one flag while
/// cross-publisher OA registries were not reachable at all.
///
/// DOAJ is deliberately NOT in this set. ADR-0037 promoted `doaj.org` to the
/// default `oa_publisher_allowlist` unconditionally, because the project had
/// already trusted it under the `"doaj"` metadata key and the two keys simply
/// disagreed. The hosts below are different: none of them appears anywhere in
/// `http.rs`, so for them this flag is genuinely new trust and opt-in is
/// correct.
///
/// Every entry is a registry or repository whose *purpose* is open
/// distribution, not a publisher platform — enabling this must not become
/// a way to reach paywalled content. Both the apex and the `*.` wildcard
/// are listed where the apex itself serves content: a single-suffix
/// wildcard does not match the apex ([`validate_pattern`]), and the DOAJ
/// redirect in #405 targeted the bare apex.
pub fn oa_registry_hosts() -> Vec<UserExtensionHost> {
    const PATTERNS: &[(&str, &str)] = &[
        ("scielo.org", "SciELO — Latin American / Iberian OA network"),
        ("*.scielo.org", "SciELO national portals"),
        ("*.scielo.br", "SciELO Brazil"),
        ("zenodo.org", "Zenodo — CERN general-purpose OA repository"),
        ("*.zenodo.org", "Zenodo subdomains"),
        ("osf.io", "OSF / OSF Preprints (Center for Open Science)"),
        ("*.osf.io", "OSF preprint servers (psyarxiv, socarxiv, ...)"),
        ("hal.science", "HAL — French national OA repository"),
        ("*.hal.science", "HAL institutional portals"),
        (
            "core.ac.uk",
            "CORE — OA aggregator (Open University / Jisc)",
        ),
    ];
    PATTERNS
        .iter()
        .filter_map(|(pat, note)| {
            HostPattern::new(*pat).ok().map(|host| UserExtensionHost {
                host,
                note: Some(note.to_string()),
            })
        })
        .collect()
}

/// Parse a `config.toml` body string. Pure function; the path is used
/// only for error messages.
fn parse_str(
    text: &str,
    config_path: &Utf8Path,
) -> Result<UserExtensionConfig, UserExtensionError> {
    let raw: RawConfig = toml::from_str(text).map_err(|e| UserExtensionError::Parse {
        path: config_path.to_string(),
        source: e,
    })?;
    let raw_net = raw.network.unwrap_or_default();
    let trust_academic_repos = raw_net.trust_academic_repos;
    let trust_oa_registries = raw_net.trust_oa_registries;
    let raw_hosts = raw_net.additional_hosts;

    // Two-phase: collect ALL invalid patterns rather than failing on
    // the first. Saves the user an iterative edit-run-error cycle
    // when migrating a large additional_hosts block (review pass I6).
    let mut issues = Vec::new();
    let mut validated = Vec::with_capacity(raw_hosts.len());
    for raw_host in raw_hosts {
        match HostPattern::new(raw_host.host.clone()) {
            Ok(host) => validated.push(UserExtensionHost {
                host,
                note: raw_host.note,
            }),
            Err(kind) => issues.push(InvalidPatternIssue {
                pattern: raw_host.host,
                kind,
            }),
        }
    }
    if !issues.is_empty() {
        return Err(UserExtensionError::InvalidPatterns {
            path: config_path.to_string(),
            issues,
        });
    }
    // An empty `root = ""` is treated as absent rather than as "the empty
    // path": it would otherwise resolve to a store at the filesystem root
    // or to a panic downstream, and a blank value plainly means "unset".
    let store_root = raw
        .store
        .and_then(|st| st.root)
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty());

    Ok(UserExtensionConfig {
        additional_hosts: validated,
        trust_academic_repos,
        trust_oa_registries,
        store_root,
    })
}

/// Turn a raw `[store] root` value into a path, expanding a leading `~`.
///
/// A config file has no shell, so `~/papers` written there would otherwise
/// become a literal directory named `~` next to the cwd — a silent wrong
/// answer of exactly the kind #441 was about. `DOIGET_STORE_ROOT` needs no
/// equivalent because the shell expands it before the process starts.
///
/// Kept out of the parser so parsing stays a pure function of the file
/// text; both the CLI and the MCP resolver call this at the point of use,
/// which is also what keeps their two answers identical.
///
/// Falls back to the value verbatim when neither `HOME` nor `USERPROFILE`
/// is set — a wrong path the user can see beats a silent substitution.
#[must_use]
pub fn expand_store_root(raw: &str) -> Utf8PathBuf {
    let rest = match raw.strip_prefix('~') {
        Some(r) => r,
        None => return Utf8PathBuf::from(raw),
    };
    // Only `~` and `~/...` — `~alice/...` is another user's home, which is
    // not something this can resolve, so it is left verbatim.
    // A backslash char literal is written as a unicode escape so no tool
    // that rewrites this file can mangle it.
    if !(rest.is_empty() || rest.starts_with('/') || rest.starts_with('\u{5c}')) {
        return Utf8PathBuf::from(raw);
    }
    let home = std::env::var("HOME")
        .ok()
        .filter(|h| !h.is_empty())
        .or_else(|| std::env::var("USERPROFILE").ok().filter(|h| !h.is_empty()));
    match home {
        Some(h) => {
            let trimmed = rest.trim_start_matches(['/', '\u{5c}']);
            if trimmed.is_empty() {
                Utf8PathBuf::from(h)
            } else {
                Utf8PathBuf::from(h).join(trimmed)
            }
        }
        None => Utf8PathBuf::from(raw),
    }
}

/// Validate a host pattern per ADR-0028 D2-1.
///
/// Accepted shapes:
///
/// - Literal FQDN: `example.org`, `ruj.uj.edu.pl`. Only
///   `[A-Za-z0-9.-]`, at least one `.`, no empty / hyphen-bordering
///   labels.
/// - Single-suffix wildcard: `*.example.org`. The `*` MUST be the
///   first character, MUST be followed by `.`, and MUST be the only
///   `*` in the pattern.
///
/// # Errors
///
/// Returns the closed-enum [`PatternError`] for any rejected input.
pub fn validate_pattern(pattern: &str) -> Result<(), PatternError> {
    if pattern.is_empty() {
        return Err(PatternError::Empty);
    }
    if pattern.trim() != pattern {
        return Err(PatternError::Whitespace);
    }
    if pattern == "*" {
        return Err(PatternError::BareWildcard);
    }
    let body = match pattern.strip_prefix("*.") {
        Some(rest) => {
            // A `*` in the body after stripping `*.` means a second
            // wildcard (e.g. `*.edu.*` → body = `edu.*`).
            if rest.contains('*') {
                return Err(PatternError::MultiSegmentGlob);
            }
            rest
        }
        None if pattern.contains('*') => {
            // A `*` not in leading `*.` position (e.g. `foo.*.org`,
            // `f*o.bar`, `*foo.bar`).
            return Err(PatternError::MisplacedWildcard);
        }
        None => pattern,
    };
    if body.is_empty() {
        return Err(PatternError::EmptySuffix);
    }
    validate_fqdn(body)
}

fn validate_fqdn(body: &str) -> Result<(), PatternError> {
    if !body.contains('.') {
        return Err(PatternError::NoDot);
    }
    for label in body.split('.') {
        if label.is_empty() {
            return Err(PatternError::EmptyLabel);
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err(PatternError::LabelHyphenBorder {
                label: label.to_string(),
            });
        }
        if !label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err(PatternError::BadChar {
                label: label.to_string(),
            });
        }
    }
    Ok(())
}

/// Merge a slice of [`UserExtensionHost`] into the `oa-publisher`
/// entry of an existing allowlist vector.
///
/// Duplicates are de-duplicated against the existing `redirect_hosts`
/// to keep the host list minimal and the future `verified_by`
/// provenance count honest (review pass I9). If the vector contains
/// no `oa-publisher` entry, one is created.
///
/// A no-op when `user_hosts` is empty.
///
/// The `note` field is intentionally dropped at the merge boundary —
/// it remains on [`UserExtensionHost`] for S3b's provenance plumbing
/// to consume from the same parsed vector.
pub fn merge_into_allowlists(
    allowlists: &mut Vec<SourceAllowlist>,
    user_hosts: &[UserExtensionHost],
) {
    if user_hosts.is_empty() {
        return;
    }
    if let Some(oa) = allowlists.iter_mut().find(|a| a.source == "oa-publisher") {
        for h in user_hosts {
            let s = h.host.as_str();
            if !oa.redirect_hosts.iter().any(|p| p == s) {
                oa.redirect_hosts.push(s.to_string());
            }
        }
        return;
    }
    let mut new_patterns: Vec<String> = Vec::with_capacity(user_hosts.len());
    for h in user_hosts {
        let s = h.host.as_str().to_string();
        if !new_patterns.contains(&s) {
            new_patterns.push(s);
        }
    }
    allowlists.push(SourceAllowlist::new("oa-publisher", new_patterns));
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn p(s: &str) -> &Utf8Path {
        Utf8Path::new(s)
    }

    // ---- validate_pattern (typed PatternError) ----------------------

    #[test]
    fn validate_pattern_accepts_literal_fqdn() {
        assert!(validate_pattern("ruj.uj.edu.pl").is_ok());
        assert!(validate_pattern("example.org").is_ok());
        assert!(validate_pattern("a.b.c.d.e").is_ok());
    }

    #[test]
    fn validate_pattern_accepts_single_suffix_wildcard() {
        assert!(validate_pattern("*.uj.edu.pl").is_ok());
        assert!(validate_pattern("*.aps.org").is_ok());
    }

    #[test]
    fn validate_pattern_rejects_empty() {
        assert_eq!(validate_pattern(""), Err(PatternError::Empty));
    }

    #[test]
    fn validate_pattern_rejects_whitespace() {
        assert_eq!(
            validate_pattern("  example.org"),
            Err(PatternError::Whitespace)
        );
        assert_eq!(
            validate_pattern("example.org  "),
            Err(PatternError::Whitespace)
        );
    }

    #[test]
    fn validate_pattern_rejects_bare_wildcard() {
        assert_eq!(validate_pattern("*"), Err(PatternError::BareWildcard));
    }

    #[test]
    fn validate_pattern_rejects_multi_segment_globs() {
        for bad in ["*.edu.*", "*.ac.*", "*.*", "*.example.*"] {
            assert_eq!(
                validate_pattern(bad),
                Err(PatternError::MultiSegmentGlob),
                "{bad} should be MultiSegmentGlob"
            );
        }
    }

    #[test]
    fn validate_pattern_rejects_misplaced_wildcards() {
        for bad in ["foo.*.org", "f*o.bar", "*foo.bar"] {
            assert_eq!(
                validate_pattern(bad),
                Err(PatternError::MisplacedWildcard),
                "{bad} should be MisplacedWildcard"
            );
        }
    }

    #[test]
    fn validate_pattern_rejects_non_host_chars() {
        for bad in ["user@host.com", "host.com/", "host.com:80", "https://x.y"] {
            assert!(
                matches!(
                    validate_pattern(bad),
                    Err(PatternError::BadChar { .. }) | Err(PatternError::EmptyLabel)
                ),
                "{bad} should be BadChar or EmptyLabel; got {:?}",
                validate_pattern(bad)
            );
        }
    }

    #[test]
    fn validate_pattern_rejects_no_dot() {
        assert_eq!(validate_pattern("singlelabel"), Err(PatternError::NoDot));
    }

    #[test]
    fn validate_pattern_rejects_empty_label_classes() {
        for bad in [".example.org", "example..org", "example.org."] {
            assert_eq!(
                validate_pattern(bad),
                Err(PatternError::EmptyLabel),
                "{bad} should be EmptyLabel"
            );
        }
    }

    #[test]
    fn validate_pattern_rejects_hyphen_bordering_labels() {
        for (bad, label) in [
            ("-foo.example.org", "-foo"),
            ("foo.-example.org", "-example"),
            ("foo.example-.org", "example-"),
        ] {
            assert_eq!(
                validate_pattern(bad),
                Err(PatternError::LabelHyphenBorder {
                    label: label.to_string()
                }),
                "{bad} should be LabelHyphenBorder({label})"
            );
        }
    }

    #[test]
    fn validate_pattern_rejects_empty_suffix_after_wildcard() {
        assert_eq!(validate_pattern("*."), Err(PatternError::EmptySuffix));
    }

    // ---- HostPattern type ------------------------------------------

    #[test]
    fn host_pattern_new_validates() {
        assert!(HostPattern::new("ruj.uj.edu.pl").is_ok());
        assert_eq!(HostPattern::new(""), Err(PatternError::Empty));
    }

    #[test]
    fn host_pattern_try_from_str_and_string() {
        let from_str: HostPattern = "*.aps.org".try_into().expect("ok");
        let from_string: HostPattern = String::from("*.aps.org").try_into().expect("ok");
        assert_eq!(from_str, from_string);
    }

    #[test]
    fn host_pattern_deserialize_validates() {
        // The serde impl runs validate_pattern; an invalid value
        // fails deserialisation (review pass C1 — the invariant is
        // type-level, not only enforced post-deserialise).
        let bad = toml::from_str::<HostPattern>("\"*.edu.*\"");
        assert!(bad.is_err(), "TOML deserialize MUST validate the pattern");
    }

    // ---- parse_str -------------------------------------------------

    #[test]
    fn parse_empty_config_returns_no_hosts() {
        let cfg = parse_str("", p("config.toml")).unwrap();
        assert_eq!(cfg.additional_hosts, vec![]);
        assert!(!cfg.trust_academic_repos);
    }

    #[test]
    fn parse_config_without_network_section_returns_no_hosts() {
        let toml = r#"
            [store]
            root = "/tmp"
        "#;
        let cfg = parse_str(toml, p("config.toml")).unwrap();
        assert_eq!(cfg.additional_hosts, vec![]);
        assert!(!cfg.trust_academic_repos);
    }

    #[test]
    fn parse_config_with_unknown_network_fields_is_accepted() {
        // S3b's full reader will own deny_unknown_fields on a
        // complete schema; until then we tolerate the rest of the
        // [network] table (`contact_email`, future `cooldown_ms`,
        // etc.) so an existing user config still loads.
        let toml = r#"
            [network]
            contact_email = "x@y.org"
            cooldown_ms = 250
        "#;
        let cfg = parse_str(toml, p("config.toml")).unwrap();
        assert_eq!(cfg.additional_hosts, vec![]);
        assert!(!cfg.trust_academic_repos);
    }

    #[test]
    fn parse_rejects_unknown_field_inside_additional_hosts_entry() {
        // The [[network.additional_hosts]] table itself uses
        // deny_unknown_fields, so typos and stray keys surface as a
        // parse error (review pass I5). The unknown-key string is
        // chosen so the typos lint doesn't fight us — `notez`
        // doesn't trip any English-word dictionary while still
        // exercising the deny_unknown_fields code path.
        let toml = r#"
            [[network.additional_hosts]]
            host = "ruj.uj.edu.pl"
            notez = "typo"
        "#;
        let err = parse_str(toml, p("config.toml")).expect_err("typo must fail");
        assert!(matches!(err, UserExtensionError::Parse { .. }));
    }

    #[test]
    fn parse_one_literal_host_with_note() {
        let toml = r#"
            [[network.additional_hosts]]
            host = "ruj.uj.edu.pl"
            note = "Jagiellonian University Repository"
        "#;
        let got = parse_str(toml, p("config.toml")).unwrap();
        assert_eq!(got.additional_hosts.len(), 1);
        assert_eq!(got.additional_hosts[0].host.as_str(), "ruj.uj.edu.pl");
        assert_eq!(
            got.additional_hosts[0].note.as_deref(),
            Some("Jagiellonian University Repository")
        );
    }

    #[test]
    fn parse_multiple_hosts_mixed_literal_and_wildcard() {
        let toml = r#"
            [[network.additional_hosts]]
            host = "ruj.uj.edu.pl"

            [[network.additional_hosts]]
            host = "*.aps.org"
            note = "user override"
        "#;
        let got = parse_str(toml, p("config.toml")).unwrap();
        assert_eq!(got.additional_hosts.len(), 2);
        assert_eq!(got.additional_hosts[0].host.as_str(), "ruj.uj.edu.pl");
        assert!(got.additional_hosts[0].note.is_none());
        assert_eq!(got.additional_hosts[1].host.as_str(), "*.aps.org");
        assert_eq!(
            got.additional_hosts[1].note.as_deref(),
            Some("user override")
        );
    }

    #[test]
    fn parse_collects_all_invalid_patterns_not_just_first() {
        // Review pass I6: every offender appears in the error.
        let toml = r#"
            [[network.additional_hosts]]
            host = "*.edu.*"

            [[network.additional_hosts]]
            host = "ok.example.org"

            [[network.additional_hosts]]
            host = "user@host.com"
        "#;
        let err = parse_str(toml, p("/home/u/.config/doiget/config.toml"))
            .expect_err("invalid patterns must error");
        match err {
            UserExtensionError::InvalidPatterns { path, issues } => {
                assert_eq!(path, "/home/u/.config/doiget/config.toml");
                assert_eq!(issues.len(), 2, "both bad patterns collected");
                assert_eq!(issues[0].pattern, "*.edu.*");
                assert_eq!(issues[0].kind, PatternError::MultiSegmentGlob);
                assert_eq!(issues[1].pattern, "user@host.com");
                assert!(matches!(
                    issues[1].kind,
                    PatternError::BadChar { .. } | PatternError::EmptyLabel
                ));
            }
            other => panic!("expected InvalidPatterns, got {other:?}"),
        }
    }

    #[test]
    fn parse_rejects_malformed_toml() {
        let err = parse_str("[[network.additional_hosts\nhost=\"foo\"", p("config.toml"))
            .expect_err("malformed toml must error");
        assert!(matches!(err, UserExtensionError::Parse { .. }));
    }

    // ---- load -------------------------------------------------------

    #[test]
    fn load_returns_empty_when_file_missing() {
        let td = tempfile::TempDir::new().unwrap();
        let path = Utf8Path::from_path(td.path()).unwrap().join("missing.toml");
        let got = load(&path).expect("missing file MUST be Ok(empty)");
        assert_eq!(got.additional_hosts, vec![]);
        assert!(!got.trust_academic_repos);
    }

    #[test]
    fn load_reads_real_file() {
        use std::io::Write;
        let td = tempfile::TempDir::new().unwrap();
        let path = Utf8Path::from_path(td.path()).unwrap().join("config.toml");
        let mut f = std::fs::File::create(path.as_std_path()).unwrap();
        f.write_all(
            br#"
[[network.additional_hosts]]
host = "ruj.uj.edu.pl"
note = "Jagiellonian"
"#,
        )
        .unwrap();
        let got = load(&path).expect("ok");
        assert_eq!(got.additional_hosts.len(), 1);
        assert_eq!(got.additional_hosts[0].host.as_str(), "ruj.uj.edu.pl");
    }

    // ---- merge_into_allowlists -------------------------------------

    #[test]
    fn merge_appends_to_existing_oa_publisher_entry() {
        let mut allowlists = vec![
            SourceAllowlist::new("crossref", vec!["api.crossref.org".into()]),
            SourceAllowlist::new("oa-publisher", vec!["pmc.ncbi.nlm.nih.gov".into()]),
        ];
        let user_hosts = vec![UserExtensionHost::for_test("ruj.uj.edu.pl")];
        merge_into_allowlists(&mut allowlists, &user_hosts);

        let oa = allowlists
            .iter()
            .find(|a| a.source == "oa-publisher")
            .unwrap();
        assert_eq!(
            oa.redirect_hosts,
            vec![
                "pmc.ncbi.nlm.nih.gov".to_string(),
                "ruj.uj.edu.pl".to_string()
            ]
        );
        assert_eq!(allowlists.len(), 2);
    }

    #[test]
    fn merge_creates_oa_publisher_entry_if_missing() {
        let mut allowlists = vec![SourceAllowlist::new(
            "crossref",
            vec!["api.crossref.org".into()],
        )];
        let user_hosts = vec![UserExtensionHost::for_test("ruj.uj.edu.pl")];
        merge_into_allowlists(&mut allowlists, &user_hosts);
        assert_eq!(allowlists.len(), 2);
        let oa = allowlists
            .iter()
            .find(|a| a.source == "oa-publisher")
            .unwrap();
        assert_eq!(oa.redirect_hosts, vec!["ruj.uj.edu.pl".to_string()]);
    }

    #[test]
    fn merge_is_noop_on_empty_user_hosts() {
        let mut allowlists = vec![SourceAllowlist::new(
            "crossref",
            vec!["api.crossref.org".into()],
        )];
        let snapshot: Vec<(String, Vec<String>)> = allowlists
            .iter()
            .map(|a| (a.source.clone(), a.redirect_hosts.clone()))
            .collect();
        merge_into_allowlists(&mut allowlists, &[]);
        let after: Vec<(String, Vec<String>)> = allowlists
            .iter()
            .map(|a| (a.source.clone(), a.redirect_hosts.clone()))
            .collect();
        assert_eq!(snapshot, after);
    }

    #[test]
    fn merge_dedupes_against_existing_entries() {
        // Review pass I9.
        let mut allowlists = vec![SourceAllowlist::new(
            "oa-publisher",
            vec!["ruj.uj.edu.pl".into()],
        )];
        let user_hosts = vec![
            UserExtensionHost::for_test("ruj.uj.edu.pl"),
            UserExtensionHost::for_test("*.uj.edu.pl"),
            UserExtensionHost::for_test("*.uj.edu.pl"),
        ];
        merge_into_allowlists(&mut allowlists, &user_hosts);
        let oa = allowlists
            .iter()
            .find(|a| a.source == "oa-publisher")
            .unwrap();
        assert_eq!(
            oa.redirect_hosts,
            vec!["ruj.uj.edu.pl".to_string(), "*.uj.edu.pl".to_string()]
        );
    }

    #[test]
    fn merge_dedupes_when_creating_new_entry() {
        let mut allowlists = Vec::new();
        let user_hosts = vec![
            UserExtensionHost::for_test("ruj.uj.edu.pl"),
            UserExtensionHost::for_test("ruj.uj.edu.pl"),
        ];
        merge_into_allowlists(&mut allowlists, &user_hosts);
        assert_eq!(allowlists.len(), 1);
        assert_eq!(allowlists[0].redirect_hosts, vec!["ruj.uj.edu.pl"]);
    }

    #[test]
    fn merged_pattern_is_matched_by_source_allowlist() {
        let parsed = parse_str(
            r#"
[[network.additional_hosts]]
host = "*.uj.edu.pl"
"#,
            p("config.toml"),
        )
        .unwrap();
        let mut allowlists = vec![SourceAllowlist::new("oa-publisher", vec![])];
        merge_into_allowlists(&mut allowlists, &parsed.additional_hosts);
        let oa = allowlists
            .iter()
            .find(|a| a.source == "oa-publisher")
            .unwrap();
        assert!(oa.matches("ruj.uj.edu.pl"));
        assert!(oa.matches("alpha.uj.edu.pl"));
        assert!(!oa.matches("ruj.uj.edu.ru"));
    }

    // ---- trust_academic_repos ----------------------------------------

    #[test]
    fn parse_trust_academic_repos_false_by_default() {
        let cfg = parse_str("[network]\ncooldown_ms = 100", p("config.toml")).unwrap();
        assert!(!cfg.trust_academic_repos);
    }

    #[test]
    fn parse_trust_academic_repos_true_when_set() {
        let toml = "[network]\ntrust_academic_repos = true";
        let cfg = parse_str(toml, p("config.toml")).unwrap();
        assert!(cfg.trust_academic_repos);
        assert_eq!(cfg.additional_hosts, vec![]);
    }

    // ---- trust_oa_registries (#405) ----------------------------------

    #[test]
    fn parse_trust_oa_registries_false_by_default() {
        let cfg = parse_str("[network]\n", p("config.toml")).unwrap();
        assert!(!cfg.trust_oa_registries);
    }

    #[test]
    fn parse_trust_oa_registries_true_when_set() {
        let toml = "[network]\ntrust_oa_registries = true";
        let cfg = parse_str(toml, p("config.toml")).unwrap();
        assert!(cfg.trust_oa_registries);
        // Independent of the academic flag — the two trust arguments are
        // different, so taking one must not imply the other.
        assert!(!cfg.trust_academic_repos);
        assert_eq!(cfg.additional_hosts, vec![]);
    }

    #[test]
    fn oa_registry_hosts_are_valid_patterns() {
        let hosts = oa_registry_hosts();
        assert!(
            !hosts.is_empty(),
            "at least one OA registry pattern expected"
        );
        for h in &hosts {
            // Every entry must survive the ADR-0028 D2-1 validator. Unlike
            // the academic set these include bare apexes (`osf.io`), so
            // the assertion is validity, not wildcard shape.
            validate_pattern(h.host.as_str()).unwrap_or_else(|e| {
                panic!("invalid OA registry pattern {}: {e:?}", h.host.as_str())
            });
            assert!(
                h.note.is_some(),
                "every curated entry carries a note: {}",
                h.host.as_str()
            );
        }
    }

    /// ADR-0037 moved DOAJ out of this set and into the DEFAULT
    /// `oa_publisher_allowlist`, so the flag must NOT carry it — otherwise
    /// the two places disagree again, in the other direction.
    #[test]
    fn oa_registry_hosts_exclude_doaj_which_is_now_a_default() {
        let hosts = oa_registry_hosts();
        let patterns: Vec<&str> = hosts.iter().map(|h| h.host.as_str()).collect();
        for gone in &["doaj.org", "*.doaj.org"] {
            assert!(
                !patterns.contains(gone),
                "{gone} belongs to oa_publisher_allowlist since ADR-0037, not to this flag;                  got {patterns:?}"
            );
        }
        for expected in &[
            "scielo.org",
            "zenodo.org",
            "osf.io",
            "hal.science",
            "core.ac.uk",
        ] {
            assert!(
                patterns.contains(expected),
                "expected OA registry pattern {expected} not found in {patterns:?}"
            );
        }
    }

    /// The two curated sets must stay disjoint: an entry in both would make
    /// one flag silently widen what the other advertises.
    #[test]
    fn curated_sets_are_disjoint() {
        let academic: Vec<String> = academic_repo_hosts()
            .iter()
            .map(|h| h.host.as_str().to_string())
            .collect();
        for h in oa_registry_hosts() {
            assert!(
                !academic.contains(&h.host.as_str().to_string()),
                "{} is in both curated sets",
                h.host.as_str()
            );
        }
    }

    #[test]
    fn academic_repo_hosts_are_valid_patterns() {
        let hosts = academic_repo_hosts();
        assert!(
            !hosts.is_empty(),
            "at least one academic host pattern expected"
        );
        // Every returned entry must round-trip through HostPattern::new
        for h in &hosts {
            assert!(
                h.host.as_str().starts_with("*."),
                "academic patterns are single-suffix wildcards: {}",
                h.host.as_str()
            );
        }
    }

    #[test]
    fn academic_repo_hosts_match_expected_domains() {
        let hosts = academic_repo_hosts();
        let patterns: Vec<&str> = hosts.iter().map(|h| h.host.as_str()).collect();
        for expected in &["*.ac.uk", "*.ac.jp", "*.edu.au", "*.edu.cn", "*.edu.br"] {
            assert!(
                patterns.contains(expected),
                "expected academic pattern {expected} not found"
            );
        }
    }
    /// #441: `[store] root` reaches the caller at all.
    #[test]
    fn store_root_is_parsed_from_the_same_file_as_the_network_gate() {
        let cfg = parse_str(
            "[store]\nroot = \"/home/alice/papers\"\n\n[network]\ntrust_academic_repos = true\n",
            Utf8Path::new("test.toml"),
        )
        .expect("parses");
        assert_eq!(cfg.store_root.as_deref(), Some("/home/alice/papers"));
        assert!(
            cfg.trust_academic_repos,
            "the network section must still be read from the same pass"
        );
    }

    /// A blank value is "unset", not the empty path — which would resolve
    /// to the filesystem root.
    #[test]
    fn blank_store_root_parses_as_absent() {
        let cfg =
            parse_str("[store]\nroot = \"  \"\n", Utf8Path::new("test.toml")).expect("parses");
        assert_eq!(cfg.store_root, None);
    }

    /// No `[store]` table at all is fine, and must not disturb anything.
    #[test]
    fn absent_store_table_is_not_an_error() {
        let cfg = parse_str(
            "[network]\ntrust_oa_registries = true\n",
            Utf8Path::new("t.toml"),
        )
        .expect("parses");
        assert_eq!(cfg.store_root, None);
        assert!(cfg.trust_oa_registries);
    }

    /// A config file has no shell, so `~` must be expanded here or it
    /// becomes a literal directory named `~`.
    #[test]
    #[serial_test::serial]
    fn expand_store_root_resolves_a_leading_tilde() {
        let prior_home = std::env::var("HOME").ok();
        let prior_profile = std::env::var("USERPROFILE").ok();
        std::env::set_var("HOME", "/home/alice");
        std::env::remove_var("USERPROFILE");

        assert_eq!(
            expand_store_root("~/papers")
                .as_str()
                .replace('\u{5c}', "/"),
            "/home/alice/papers"
        );
        assert_eq!(expand_store_root("~").as_str(), "/home/alice");
        // Absolute and relative paths pass through untouched.
        assert_eq!(expand_store_root("/srv/papers").as_str(), "/srv/papers");
        assert_eq!(expand_store_root("papers").as_str(), "papers");
        // `~alice` is another user's home; not something this can resolve,
        // so it must be left alone rather than silently mangled.
        assert_eq!(expand_store_root("~bob/papers").as_str(), "~bob/papers");

        match prior_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        if let Some(v) = prior_profile {
            std::env::set_var("USERPROFILE", v);
        }
    }
}
