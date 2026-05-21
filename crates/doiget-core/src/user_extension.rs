//! User-extensible capability gate (ADR-0028, #220).
//!
//! Parses the `[[network.additional_hosts]]` array-of-tables from the
//! user's `config.toml`, validates each entry against the restricted
//! pattern grammar (literal FQDN or single-suffix wildcard `*.foo.bar`),
//! and exposes a helper that merges the user-added hosts into the
//! orchestrator's `oa-publisher` [`crate::http::SourceAllowlist`].
//!
//! This module is intentionally minimal: it does NOT layer config.toml
//! with env vars or implement the full `docs/CONFIG.md` resolution
//! ladder. The full reader is a separate slice (S3b); this slice ships
//! only the surface needed for ADR-0028 D2 (user-extensible allowlist).
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
//!
//! # Rejected at parse time:
//! # [[network.additional_hosts]]
//! # host = "*.edu.*"            # multi-segment glob — rejected
//! # host = "*"                  # bare wildcard — rejected
//! # host = "user@host.com"      # `@` not in host charset
//! ```
//!
//! # Provenance & doctor (deferred to S3b)
//!
//! ADR-0028 D2-2 / D2-3 / D2-4 (the `verified_by = "user"` provenance
//! field, `config doctor` surface, `capabilities` count) ship in the
//! S3b follow-up; this slice is purely the **data path** so a user's
//! `ruj.uj.edu.pl` actually passes the redirect-allowlist gate.

use camino::Utf8Path;
use serde::Deserialize;

use crate::http::SourceAllowlist;

/// One user-added host entry from `[[network.additional_hosts]]`.
///
/// The `note` field is free-text user documentation (e.g. "Jagiellonian
/// University Repository — Green OA"); it is recorded in the
/// provenance log alongside the host but never used for matching.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[non_exhaustive]
pub struct UserExtensionHost {
    /// The host pattern. Either a literal FQDN (`example.org`) or a
    /// single-suffix wildcard (`*.example.org`). Validated by
    /// [`validate_pattern`] before this struct reaches a caller.
    pub host: String,
    /// Optional free-text note. Surfaced by `doiget config doctor`
    /// (S3b) but never consulted for matching.
    #[serde(default)]
    pub note: Option<String>,
}

impl UserExtensionHost {
    /// Construct a new entry with no note. Used by tests; production
    /// callers go through [`load`].
    #[doc(hidden)]
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            note: None,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    network: Option<NetworkSection>,
}

#[derive(Debug, Default, Deserialize)]
struct NetworkSection {
    #[serde(default)]
    additional_hosts: Vec<UserExtensionHost>,
}

/// Errors from loading `config.toml`'s user-extension section.
#[derive(Debug, thiserror::Error)]
pub enum UserExtensionError {
    /// Filesystem read failed (other than "not found", which is treated
    /// as "no user extensions" — see [`load`]).
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
    /// One of the `host` patterns failed grammar validation per
    /// ADR-0028 D2-1.
    #[error("invalid host pattern `{pattern}` in {path}: {reason}")]
    InvalidPattern {
        /// The path containing the offending entry.
        path: String,
        /// The pattern that did not validate.
        pattern: String,
        /// Human-readable reason.
        reason: String,
    },
}

/// Load user-extension hosts from a `config.toml` path.
///
/// Returns an empty `Vec` if the path does not exist (the user simply
/// has not extended the gate). Returns `Err` only when the file exists
/// but cannot be read, parsed, or contains an invalid pattern. The
/// orchestrator is expected to log an `Err` as a warning and continue
/// with the curated allowlist; failing the whole fetch on a malformed
/// optional config would be hostile.
///
/// # Errors
///
/// [`UserExtensionError::Io`] on filesystem error (other than not-found),
/// [`UserExtensionError::Parse`] on malformed TOML, and
/// [`UserExtensionError::InvalidPattern`] on the first entry that
/// violates ADR-0028 D2-1.
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

/// Parse a `config.toml` body string. Pure function; the path is used
/// only for error messages. Useful in unit tests where we don't want
/// to touch the filesystem.
fn parse_str(
    text: &str,
    config_path: &Utf8Path,
) -> Result<Vec<UserExtensionHost>, UserExtensionError> {
    let cf: ConfigFile = toml::from_str(text).map_err(|e| UserExtensionError::Parse {
        path: config_path.to_string(),
        source: e,
    })?;
    let hosts = cf.network.unwrap_or_default().additional_hosts;
    for h in &hosts {
        validate_pattern(&h.host).map_err(|reason| UserExtensionError::InvalidPattern {
            path: config_path.to_string(),
            pattern: h.host.clone(),
            reason,
        })?;
    }
    Ok(hosts)
}

/// Validate a host pattern per ADR-0028 D2-1.
///
/// Accepted shapes:
///
/// - Literal FQDN: `example.org`, `ruj.uj.edu.pl`. Must contain only
///   `[A-Za-z0-9.-]`, with at least one `.` and no leading/trailing
///   `.` or `-`.
/// - Single-suffix wildcard: `*.example.org`. The `*` MUST be the
///   *only* `*` in the pattern, MUST be at the very start, and MUST
///   be followed by `.` and a valid FQDN.
///
/// Rejected shapes:
///
/// - Bare wildcard `*`.
/// - Multi-segment globs `*.edu.*`, `*.*`.
/// - Mid-string wildcards `foo.*.org`, `f*o.bar`.
/// - Empty / whitespace-only.
/// - Non-host characters (`@`, `/`, `:`, port suffixes, scheme prefix).
///
/// # Errors
///
/// Returns a human-readable reason on rejection. The caller wraps it in
/// [`UserExtensionError::InvalidPattern`].
pub fn validate_pattern(pattern: &str) -> Result<(), String> {
    if pattern.is_empty() {
        return Err("empty pattern".into());
    }
    if pattern.trim() != pattern {
        return Err("leading or trailing whitespace".into());
    }
    if pattern == "*" {
        return Err("bare wildcard `*` is not allowed".into());
    }
    // Split off a single leading `*.`, then require the remainder to be
    // a valid FQDN with no further `*`.
    let body = match pattern.strip_prefix("*.") {
        Some(rest) => rest,
        None if pattern.contains('*') => {
            return Err(
                "wildcard `*` is only allowed as the first character followed by `.`".into(),
            );
        }
        None => pattern,
    };
    if body.is_empty() {
        return Err("nothing after wildcard prefix `*.`".into());
    }
    if body.contains('*') {
        return Err("multi-segment globs are not allowed; use a single `*.<suffix>`".into());
    }
    validate_fqdn(body)
}

/// `body` must look like a plausible FQDN: ASCII letters / digits /
/// hyphens / dots, at least one dot, no empty labels, labels do not
/// start or end with `-`.
fn validate_fqdn(body: &str) -> Result<(), String> {
    if !body.contains('.') {
        return Err("host must contain at least one `.`".into());
    }
    for label in body.split('.') {
        if label.is_empty() {
            return Err("empty label (consecutive `.` or leading/trailing `.`)".into());
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err(format!("label `{label}` starts or ends with `-`"));
        }
        if !label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err(format!(
                "label `{label}` contains a non-host character (allowed: A-Z a-z 0-9 - .)"
            ));
        }
    }
    Ok(())
}

/// Merge a slice of [`UserExtensionHost`] into the `oa-publisher`
/// entry of an existing allowlist vector.
///
/// If the vector already contains a `SourceAllowlist` with
/// `source == "oa-publisher"`, the user hosts are appended to its
/// `redirect_hosts`. Otherwise a new `oa-publisher` allowlist entry is
/// pushed onto the vector. A no-op when `user_hosts` is empty.
///
/// The `note` field is intentionally dropped at the merge boundary —
/// it is preserved on the [`UserExtensionHost`] passed to S3b's
/// provenance plumbing, but the allowlist itself only needs the
/// pattern string for matching.
pub fn merge_into_allowlists(
    allowlists: &mut Vec<SourceAllowlist>,
    user_hosts: &[UserExtensionHost],
) {
    if user_hosts.is_empty() {
        return;
    }
    let new_patterns: Vec<String> = user_hosts.iter().map(|h| h.host.clone()).collect();
    if let Some(oa) = allowlists.iter_mut().find(|a| a.source == "oa-publisher") {
        oa.redirect_hosts.extend(new_patterns);
        return;
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

    // ---- validate_pattern (ADR-0028 D2-1) ---------------------------

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
    fn validate_pattern_rejects_bare_wildcard() {
        assert!(validate_pattern("*").is_err());
    }

    #[test]
    fn validate_pattern_rejects_multi_segment_glob() {
        for bad in ["*.edu.*", "*.ac.*", "*.*", "*.example.*"] {
            let err = validate_pattern(bad).unwrap_err();
            assert!(
                err.contains("multi-segment") || err.contains("wildcard"),
                "expected wildcard-related error for `{bad}`, got: {err}"
            );
        }
    }

    #[test]
    fn validate_pattern_rejects_mid_wildcard() {
        for bad in ["foo.*.org", "f*o.bar", "*foo.bar"] {
            assert!(validate_pattern(bad).is_err(), "should reject `{bad}`");
        }
    }

    #[test]
    fn validate_pattern_rejects_non_host_chars() {
        for bad in ["user@host.com", "host.com/", "host.com:80", "https://x.y"] {
            assert!(validate_pattern(bad).is_err(), "should reject `{bad}`");
        }
    }

    #[test]
    fn validate_pattern_rejects_no_dot() {
        assert!(validate_pattern("singlelabel").is_err());
    }

    #[test]
    fn validate_pattern_rejects_empty_label() {
        for bad in [".example.org", "example..org", "example.org.", ""] {
            assert!(validate_pattern(bad).is_err(), "should reject `{bad}`");
        }
    }

    #[test]
    fn validate_pattern_rejects_label_starting_with_hyphen() {
        assert!(validate_pattern("-foo.example.org").is_err());
        assert!(validate_pattern("foo.-example.org").is_err());
        assert!(validate_pattern("foo.example-.org").is_err());
    }

    #[test]
    fn validate_pattern_rejects_whitespace() {
        assert!(validate_pattern("  example.org").is_err());
        assert!(validate_pattern("example.org  ").is_err());
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
    fn parse_config_without_additional_hosts_returns_no_hosts() {
        let toml = r#"
            [network]
            contact_email = "x@y.org"
        "#;
        assert_eq!(parse_str(toml, p("config.toml")).unwrap(), vec![]);
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
        assert_eq!(got[0].host, "ruj.uj.edu.pl");
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
        assert_eq!(got[0].host, "ruj.uj.edu.pl");
        assert!(got[0].note.is_none());
        assert_eq!(got[1].host, "*.aps.org");
        assert_eq!(got[1].note.as_deref(), Some("user override"));
    }

    #[test]
    fn parse_rejects_invalid_pattern_with_path_in_error() {
        let toml = r#"
            [[network.additional_hosts]]
            host = "*.edu.*"
        "#;
        let err = parse_str(toml, p("/home/u/.config/doiget/config.toml"))
            .expect_err("invalid pattern must error");
        let msg = err.to_string();
        assert!(
            msg.contains("*.edu.*"),
            "error should mention the pattern, got: {msg}"
        );
        assert!(
            msg.contains("/home/u/.config/doiget/config.toml"),
            "error should mention the path, got: {msg}"
        );
    }

    #[test]
    fn parse_rejects_malformed_toml() {
        // Missing closing `]`.
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
        assert_eq!(got[0].host, "ruj.uj.edu.pl");
    }

    // ---- merge_into_allowlists -------------------------------------

    #[test]
    fn merge_appends_to_existing_oa_publisher_entry() {
        let mut allowlists = vec![
            SourceAllowlist::new("crossref", vec!["api.crossref.org".into()]),
            SourceAllowlist::new("oa-publisher", vec!["pmc.ncbi.nlm.nih.gov".into()]),
        ];
        let user_hosts = vec![UserExtensionHost::new("ruj.uj.edu.pl")];
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
        // crossref entry must remain untouched.
        assert_eq!(allowlists.len(), 2);
    }

    #[test]
    fn merge_creates_oa_publisher_entry_if_missing() {
        let mut allowlists = vec![SourceAllowlist::new(
            "crossref",
            vec!["api.crossref.org".into()],
        )];
        let user_hosts = vec![UserExtensionHost::new("ruj.uj.edu.pl")];
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
    fn merged_pattern_is_matched_by_source_allowlist() {
        // End-to-end: parse a wildcard, merge into allowlist, ask
        // SourceAllowlist::matches() about a concrete host. This is the
        // load-bearing assertion that user_extension + SourceAllowlist
        // compose correctly.
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
        assert!(
            oa.matches("ruj.uj.edu.pl"),
            "wildcard `*.uj.edu.pl` MUST match `ruj.uj.edu.pl`"
        );
        assert!(
            oa.matches("alpha.uj.edu.pl"),
            "wildcard must also match other subdomains"
        );
        assert!(
            !oa.matches("ruj.uj.edu.ru"),
            "wildcard MUST NOT match other suffixes"
        );
    }
}
