//! User-extensible capability gate (ADR-0028, #220).
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

use camino::Utf8Path;
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
pub fn load(config_path: &Utf8Path) -> Result<Vec<UserExtensionHost>, UserExtensionError> {
    let text = match std::fs::read_to_string(config_path.as_std_path()) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
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
    #[serde(flatten)]
    _other: serde::de::IgnoredAny,
}

#[derive(Debug, Default, Deserialize)]
struct RawNetwork {
    #[serde(default)]
    additional_hosts: Vec<RawHost>,
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

/// Parse a `config.toml` body string. Pure function; the path is used
/// only for error messages.
fn parse_str(
    text: &str,
    config_path: &Utf8Path,
) -> Result<Vec<UserExtensionHost>, UserExtensionError> {
    let raw: RawConfig = toml::from_str(text).map_err(|e| UserExtensionError::Parse {
        path: config_path.to_string(),
        source: e,
    })?;
    let raw_hosts = raw.network.unwrap_or_default().additional_hosts;

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
    Ok(validated)
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
        assert_eq!(validate_pattern("  example.org"), Err(PatternError::Whitespace));
        assert_eq!(validate_pattern("example.org  "), Err(PatternError::Whitespace));
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
        assert_eq!(parse_str("", p("config.toml")).unwrap(), vec![]);
    }

    #[test]
    fn parse_config_without_network_section_returns_no_hosts() {
        let toml = r#"
            [store]
            root = "/tmp"
        "#;
        assert_eq!(parse_str(toml, p("config.toml")).unwrap(), vec![]);
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
        assert_eq!(parse_str(toml, p("config.toml")).unwrap(), vec![]);
    }

    #[test]
    fn parse_rejects_unknown_field_inside_additional_hosts_entry() {
        // The [[network.additional_hosts]] table itself uses
        // deny_unknown_fields, so typos like `hsot = "..."` surface
        // as a parse error (review pass I5).
        let toml = r#"
            [[network.additional_hosts]]
            host = "ruj.uj.edu.pl"
            commet = "typo"
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
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].host.as_str(), "ruj.uj.edu.pl");
        assert_eq!(
            got[0].note.as_deref(),
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
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].host.as_str(), "ruj.uj.edu.pl");
        assert!(got[0].note.is_none());
        assert_eq!(got[1].host.as_str(), "*.aps.org");
        assert_eq!(got[1].note.as_deref(), Some("user override"));
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
        let err = parse_str(
            "[[network.additional_hosts\nhost=\"foo\"",
            p("config.toml"),
        )
        .expect_err("malformed toml must error");
        assert!(matches!(err, UserExtensionError::Parse { .. }));
    }

    // ---- load -------------------------------------------------------

    #[test]
    fn load_returns_empty_when_file_missing() {
        let td = tempfile::TempDir::new().unwrap();
        let path = Utf8Path::from_path(td.path())
            .unwrap()
            .join("missing.toml");
        let got = load(&path).expect("missing file MUST be Ok(empty)");
        assert_eq!(got, vec![]);
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
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].host.as_str(), "ruj.uj.edu.pl");
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
        merge_into_allowlists(&mut allowlists, &parsed);
        let oa = allowlists
            .iter()
            .find(|a| a.source == "oa-publisher")
            .unwrap();
        assert!(oa.matches("ruj.uj.edu.pl"));
        assert!(oa.matches("alpha.uj.edu.pl"));
        assert!(!oa.matches("ruj.uj.edu.ru"));
    }
}
