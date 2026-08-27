//! `credentials.toml` — the per-publisher TDM **API keys** (#509).
//!
//! `docs/CONFIG.md` §6 specified this file in full — schema, precedence, and
//! a `0600` permission warning — and nothing read it. A user who followed a
//! NORMATIVE document wrote their Elsevier key into a file doiget ignored,
//! and the source then reported itself unavailable for want of a key. That
//! is the #442 / #454 / #476 shape, landed on credentials.
//!
//! ## What this file carries, and what it deliberately does not
//!
//! **`api_key`: yes.** A long-lived key is genuinely better off in a file
//! than in the environment. It survives a shell restart, it is not visible
//! in `ps`, and it does not leak into the environment of every subprocess.
//! The `0600` check is then a real control rather than a sentence — which
//! means it has to be *visible*, so it is an [`Advisory`] that `config
//! doctor` prints rather than a `tracing::warn!` the default log level
//! throws away.
//!
//! **`agreed`: no.** `docs/LEGAL.md` §6a.2 makes the per-publisher
//! agreement an *enforced control*, and part of why it is meaningful is
//! that `DOIGET_AGREE_TDM_<PUBLISHER>=1` is a variable the user sets in the
//! session that runs the fetch. A boolean written once into a file and
//! forgotten is a weaker act of consent, and weakening it as a side effect
//! of adding a convenience is the kind of accident ADR-0048 was written
//! about. So the key may come from either place; **the agreement is
//! environment-only**, and an `agreed` key here is reported rather than
//! silently discarded — a documented field with no reader is the defect
//! this module exists to close.
//!
//! ## Precedence
//!
//! `DOIGET_KEY_<PUBLISHER>` wins over `[tdm.<publisher>] api_key`, matching
//! the `docs/CONFIG.md` §1 chain and the `store_root` / `contact_email`
//! rungs (#441, #504).

use camino::Utf8PathBuf;
use serde::Deserialize;

/// Publisher slugs this file may carry, matching the `[tdm.<slug>]` tables
/// in `docs/CONFIG.md` §6 and the `tdm-<slug>` Cargo features.
pub const PUBLISHERS: [&str; 4] = ["elsevier", "aps", "springer", "ieee"];

/// Something worth telling the user that does not stop the rest of the
/// file being used.
///
/// These were `tracing::warn!` calls and nothing else, which the CLI's
/// `EnvFilter::from_default_env()` suppresses at the default level — so the
/// permission warning `docs/CONFIG.md` §6 promises, and the "you typed a key
/// that did not load" line, reached nobody. Carrying them as data lets
/// `config doctor` print them, and lets tests assert on them rather than on
/// log output that no assertion can see.
///
/// File-level failures (unreadable, malformed) are [`CredentialsError`]
/// instead: those invalidate the whole file rather than one entry.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Advisory {
    /// The file is readable beyond its owner. POSIX only.
    InsecurePermissions {
        /// The offending file.
        path: String,
        /// The permission bits, masked to `0o777`.
        mode: u32,
    },
    /// `[tdm.<publisher>]` names a publisher doiget does not know.
    UnknownPublisher {
        /// The unrecognised slug.
        publisher: String,
    },
    /// `agreed` is set. It is parsed only so it can be reported; the
    /// agreement is environment-only (`docs/LEGAL.md` §6a.2).
    AgreedIgnored {
        /// The publisher whose table carried it.
        publisher: String,
    },
    /// `api_key` is present but blank, so it cannot authenticate.
    ///
    /// Covers both `api_key = ""` and a whitespace-only value. The first is
    /// the commoner typo and was, before this, indistinguishable from the
    /// key being absent — `Option::unwrap_or_default()` collapsed `None` and
    /// `Some("")` to the same empty string.
    BlankKey {
        /// The publisher whose table carried it.
        publisher: String,
    },
}

impl std::fmt::Display for Advisory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InsecurePermissions { path, mode } => write!(
                f,
                "credentials.toml is readable beyond its owner (mode {mode:04o}); it holds \
                 publisher API keys. Run: chmod 600 {path}"
            ),
            Self::UnknownPublisher { publisher } => write!(
                f,
                "credentials.toml has [tdm.{publisher}], which is not a publisher doiget \
                 knows; expected one of {PUBLISHERS:?}"
            ),
            Self::AgreedIgnored { publisher } => write!(
                f,
                "credentials.toml sets [tdm.{publisher}] agreed, which doiget does NOT read. \
                 The per-publisher agreement is environment-only \
                 (DOIGET_AGREE_TDM_{}=1) so that it is an act taken in the session that runs \
                 the fetch — docs/LEGAL.md §6a.2. Only api_key is read from this file.",
                publisher.to_uppercase()
            ),
            Self::BlankKey { publisher } => write!(
                f,
                "credentials.toml sets [tdm.{publisher}] api_key to a blank value, which \
                 cannot authenticate and is treated as unset. Remove the line or give it a key."
            ),
        }
    }
}

/// Parsed `credentials.toml`.
///
/// An absent file yields the default (no keys); unreadable and malformed are
/// [`CredentialsError`], which [`load_or_default`] turns back into the
/// default for the fetch path so one bad line cannot take a fetch down.
/// Per-entry problems ride along as [`Advisory`] values. Nothing about this
/// file is dropped in silence — that is what cost a user their configuration.
#[derive(Default, Clone)]
#[non_exhaustive]
pub struct Credentials {
    keys: Vec<(String, String)>,
    advisories: Vec<Advisory>,
}

/// Hand-written so a stray `{:?}` cannot print a publisher API key.
///
/// `TdmGrant` wraps the same value in `secrecy::SecretString` precisely so
/// `Debug` never renders it; between this file and that wrapper the key is
/// a plain `String`, and a `#[derive(Debug)]` here would have re-opened the
/// hole one hop earlier. Publisher names are not secret and are shown.
impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field(
                "keys",
                &self
                    .keys
                    .iter()
                    .map(|(p, _)| format!("{p}: <redacted>"))
                    .collect::<Vec<_>>(),
            )
            .field("advisories", &self.advisories)
            .finish()
    }
}

impl Credentials {
    /// The `api_key` for `publisher`, if the file supplied one.
    #[must_use]
    pub fn api_key(&self, publisher: &str) -> Option<&str> {
        self.keys
            .iter()
            .find(|(p, _)| p == publisher)
            .map(|(_, k)| k.as_str())
    }

    /// True when the file carried no usable key at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// How many publishers supplied a usable key.
    ///
    /// For `config doctor`, which reports that a credentials file was read
    /// without printing anything from it.
    #[must_use]
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// Everything worth telling the user that did not stop the file being
    /// used. See [`Advisory`].
    #[must_use]
    pub fn advisories(&self) -> &[Advisory] {
        &self.advisories
    }
}

// No `Debug` on either raw type: `RawEntry::api_key` is the untrimmed key
// straight off disk, one hop before the redacting `Debug` on `Credentials`.
// A derive here is what a future `tracing::debug!(?raw, ..)` would leak
// through.
#[derive(Default, Deserialize)]
struct RawFile {
    #[serde(default)]
    tdm: Option<std::collections::BTreeMap<String, RawEntry>>,
    #[serde(flatten)]
    _other: serde::de::IgnoredAny,
}

#[derive(Default, Deserialize)]
struct RawEntry {
    #[serde(default)]
    api_key: Option<String>,
    /// Parsed only so its presence can be *reported*. The agreement is
    /// environment-only (`docs/LEGAL.md` §6a.2); see the module docs.
    #[serde(default)]
    agreed: Option<bool>,
    #[serde(flatten)]
    _other: serde::de::IgnoredAny,
}

/// `<config_dir>/doiget/credentials.toml`.
///
/// # Errors
///
/// As [`crate::user_extension::config_dir`].
pub fn path() -> Result<Utf8PathBuf, crate::user_extension::ConfigDirError> {
    Ok(crate::user_extension::config_dir()?
        .join("doiget")
        .join("credentials.toml"))
}

/// Why `credentials.toml` could not be read.
///
/// Exists so `doiget config doctor` can *report* the failure. Without it
/// the only surface was `tracing::warn!`, which the CLI's
/// `EnvFilter::from_default_env()` suppresses at the default level — so a
/// malformed or unreadable credentials file produced no warning, no doctor
/// line, and only the downstream "source unavailable" this module exists to
/// prevent.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CredentialsError {
    /// The file exists but could not be read.
    #[error("{path}: {source}")]
    Io {
        /// The file that could not be read.
        path: String,
        /// The underlying filesystem error.
        source: std::io::Error,
    },
    /// The file is not valid TOML.
    ///
    /// Deliberately **not** carrying the `toml::de::Error`. Its `Display`
    /// renders the offending source line verbatim, and the commonest way
    /// this file is malformed is an unterminated `api_key = "..."` — so the
    /// error most likely to be printed is the one whose snippet *is* the
    /// key. Its `Debug` is worse: it holds the whole file. `config doctor`
    /// prints this variant, so it gets the position and the parser's
    /// message and nothing off the line itself.
    #[error("{path}:{line}:{column}: {message}")]
    Parse {
        /// The file that failed to parse.
        path: String,
        /// 1-based line of the failure.
        line: usize,
        /// 1-based column of the failure.
        column: usize,
        /// The parser's message, which quotes no file content.
        message: String,
    },
}

/// Build a [`CredentialsError::Parse`] that names the position without
/// echoing anything from the file. See the variant's own note.
fn redacted_parse_error(
    path: &camino::Utf8Path,
    text: &str,
    e: &toml::de::Error,
) -> CredentialsError {
    let offset = e.span().map_or(0, |s| s.start).min(text.len());
    let before = &text[..offset];
    let line = before.matches('\n').count() + 1;
    let column = before
        .rsplit_once('\n')
        .map_or(before, |(_, tail)| tail)
        .chars()
        .count()
        + 1;
    CredentialsError::Parse {
        path: path.to_string(),
        line,
        column,
        message: e.message().to_string(),
    }
}

/// Load `credentials.toml` from an explicit path, surfacing failures.
///
/// A missing file is `Ok(empty)` — TDM is opt-in and most installs have no
/// such file. Mirrors [`crate::user_extension::load`], which `config
/// doctor` already reports on the same way.
///
/// # Errors
///
/// [`CredentialsError::Io`] for a read failure other than not-found;
/// [`CredentialsError::Parse`] for malformed TOML.
pub fn load(path: &camino::Utf8Path) -> Result<Credentials, CredentialsError> {
    let text = match std::fs::read_to_string(path.as_std_path()) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Credentials::default()),
        Err(source) => {
            return Err(CredentialsError::Io {
                path: path.to_string(),
                source,
            })
        }
    };
    let mut creds = parse(&text, path)?;
    // Prepended: a world-readable key file is the most serious thing this
    // function can find, and `config doctor` prints advisories in order.
    if let Some(a) = permission_advisory(path) {
        creds.advisories.insert(0, a);
    }
    Ok(creds)
}

/// Load the credentials file, or the empty set.
///
/// Never fails; every failure mode is warned about. This is the fetch-path
/// entry point — one bad line must not take a fetch down. Callers that want
/// to *report* the failure use [`load`] instead. See [`Credentials`].
#[must_use]
pub fn load_or_default() -> Credentials {
    let path = match path() {
        Ok(p) => p,
        Err(e) => {
            tracing::debug!(error = %e, "no config directory; credentials.toml not read");
            return Credentials::default();
        }
    };
    match load(&path) {
        Ok(c) => {
            // The fetch path has no checklist to print to, so advisories
            // stay logs here. `config doctor` is the surface that shows
            // them unconditionally.
            for a in &c.advisories {
                tracing::warn!(path = %path, "{a}");
            }
            c
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "credentials.toml could not be read; keys from it are unavailable and \
                 only DOIGET_KEY_* will be used"
            );
            Credentials::default()
        }
    }
}

/// Parse a `credentials.toml` body. Pure; the path is for messages only.
fn parse(text: &str, path: &camino::Utf8Path) -> Result<Credentials, CredentialsError> {
    // A malformed file is a FILE-level failure and is returned, so
    // `config doctor` can name it. The per-entry problems below (unknown
    // publisher, an `agreed` key, a blank value) are advisories: they do
    // not invalidate the rest of the file, so they stay warnings.
    let raw: RawFile = toml::from_str(text).map_err(|e| redacted_parse_error(path, text, &e))?;
    let Some(tdm) = raw.tdm else {
        return Ok(Credentials::default());
    };

    let mut keys = Vec::new();
    let mut advisories = Vec::new();
    for (publisher, entry) in tdm {
        if !PUBLISHERS.contains(&publisher.as_str()) {
            advisories.push(Advisory::UnknownPublisher { publisher });
            continue;
        }
        if entry.agreed.is_some() {
            // Reported, not obeyed. A field parsed and discarded in silence
            // is exactly what #509 is about.
            advisories.push(Advisory::AgreedIgnored {
                publisher: publisher.clone(),
            });
        }
        // Blank means unset, as everywhere else: an empty key cannot
        // authenticate, and building a grant around one would mask the
        // misconfiguration `AgreedButNoKey` exists to surface.
        //
        // It still gets a line. A present-but-blank value is a thing the
        // user typed and believes is configured, so dropping it in silence
        // is the #442 / #476 shape this module was written to close.
        //
        // Matched on the `Option` rather than through `unwrap_or_default()`:
        // that collapsed `None` (no line at all, which is normal) and
        // `Some("")` (a line the user wrote, which is the typo) into the
        // same empty string, so the commonest form of the very case this
        // reports produced no advisory at all.
        if let Some(raw) = entry.api_key {
            let key = raw.trim();
            if key.is_empty() {
                advisories.push(Advisory::BlankKey { publisher });
            } else {
                keys.push((publisher, key.to_string()));
            }
        }
    }
    Ok(Credentials { keys, advisories })
}

/// Report when the file is group- or world-accessible on POSIX.
///
/// `docs/CONFIG.md` §6 promised this warning from the day it was written.
/// It did not exist, and neither did the reader it was attached to (#509).
/// Returned as data rather than logged, so `config doctor` can show it — as
/// a `tracing::warn!` it was invisible at the CLI's default log level, which
/// made "the `0600` check is a real control" untrue.
#[cfg(unix)]
fn permission_advisory(path: &camino::Utf8Path) -> Option<Advisory> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(path.as_std_path()).ok()?;
    let mode = meta.permissions().mode() & 0o777;
    (mode & 0o077 != 0).then(|| Advisory::InsecurePermissions {
        path: path.to_string(),
        mode,
    })
}

/// `None` off POSIX: Windows ACLs are not a mode, and a warning phrased in
/// `chmod` terms would be advice the user cannot follow.
#[cfg(not(unix))]
fn permission_advisory(_path: &camino::Utf8Path) -> Option<Advisory> {
    None
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn p() -> camino::Utf8PathBuf {
        camino::Utf8PathBuf::from("/tmp/credentials.toml")
    }

    /// Parse and unwrap, for the cases where the file is well-formed.
    fn ok(text: &str) -> Credentials {
        parse(text, &p()).expect("well-formed credentials.toml")
    }

    #[test]
    fn an_api_key_is_read_per_publisher() {
        let c = ok("[tdm.elsevier]\napi_key = \"abc\"\n\n[tdm.aps]\napi_key = \"def\"\n");
        assert_eq!(c.api_key("elsevier"), Some("abc"));
        assert_eq!(c.api_key("aps"), Some("def"));
        assert_eq!(c.api_key("springer"), None);
    }

    /// The whole point of the split: the file may carry the key, never the
    /// agreement (`docs/LEGAL.md` §6a.2). There is no accessor for `agreed`
    /// at all, so no caller can be tempted to consult one.
    #[test]
    fn agreed_in_the_file_grants_nothing() {
        let c = ok("[tdm.elsevier]\napi_key = \"abc\"\nagreed = true\n");
        assert_eq!(
            c.api_key("elsevier"),
            Some("abc"),
            "the key is still read; only the agreement is refused"
        );
    }

    #[test]
    fn a_blank_key_is_treated_as_unset() {
        let c = ok("[tdm.aps]\napi_key = \"   \"\n");
        assert!(c.is_empty(), "a blank key cannot authenticate");
    }

    /// `agreed` is refused, but its presence must be *said*, not swallowed.
    #[test]
    fn agreed_in_the_file_is_reported() {
        let c = ok("[tdm.elsevier]\napi_key = \"abc\"\nagreed = true\n");
        assert_eq!(
            c.advisories(),
            [Advisory::AgreedIgnored {
                publisher: "elsevier".to_string()
            }]
        );
    }

    #[test]
    fn an_unknown_publisher_is_reported() {
        let c = ok("[tdm.wiley]\napi_key = \"abc\"\n");
        assert_eq!(
            c.advisories(),
            [Advisory::UnknownPublisher {
                publisher: "wiley".to_string()
            }]
        );
    }

    /// A malformed file is a FILE-level failure, returned rather than
    /// warned about, so `doiget config doctor` can name it. Before #509's
    /// follow-up the only surface was `tracing::warn!`, which the CLI's
    /// default `EnvFilter` suppresses — the user saw nothing at all.
    #[test]
    fn a_malformed_file_is_a_reportable_error_not_a_silent_empty_set() {
        match parse("this is not toml = = =", &p()) {
            Err(CredentialsError::Parse { path, .. }) => {
                assert!(path.contains("credentials.toml"), "path: {path}");
            }
            other => panic!("expected a Parse error naming the file; got {other:?}"),
        }
    }

    /// `config doctor` prints this error. `toml::de::Error`'s own `Display`
    /// quotes the offending source line, and the commonest malformation of
    /// this file is an unterminated `api_key = "..."` — so rendering it
    /// would print the key to the terminal, from the one module whose whole
    /// job is that the key is never printed.
    #[test]
    fn a_parse_error_names_the_position_without_quoting_the_line() {
        let secret = "sk-do-not-print-me";
        let text = format!("[tdm.elsevier]\napi_key = \"{secret}\n");
        let e = parse(&text, &p()).expect_err("an unterminated string must not parse");
        let rendered = format!("{e}");
        assert!(
            !rendered.contains(secret),
            "the key leaked through Display: {rendered}"
        );
        assert!(
            !format!("{e:?}").contains(secret),
            "the key leaked through Debug: {e:?}"
        );
        assert!(
            rendered.contains(":2:"),
            "the position is still named: {rendered}"
        );
    }

    /// A blank value is a thing the user typed. It is still treated as
    /// unset, but it must not disappear without a word.
    ///
    /// Asserts on the advisory, not just on the resulting key count: the
    /// previous version of this test used `api_key = ""` and checked only
    /// `is_empty()`, so it passed while the reporting it is named for did
    /// not happen at all for that exact input.
    #[test]
    fn a_present_but_blank_key_is_reported_not_silently_dropped() {
        for text in [
            "[tdm.aps]\napi_key = \"\"\n",
            "[tdm.aps]\napi_key = \"   \"\n",
        ] {
            let c = ok(text);
            assert!(c.is_empty(), "{text:?} must grant no key");
            assert_eq!(
                c.advisories(),
                [Advisory::BlankKey {
                    publisher: "aps".to_string()
                }],
                "{text:?} must be reported, not dropped"
            );
        }
    }

    /// The absent case is normal and must stay quiet, or the advisory list
    /// fills with noise on every well-formed file and stops being read.
    #[test]
    fn an_absent_key_is_not_an_advisory() {
        let c = ok("[tdm.aps]\n");
        assert!(c.is_empty());
        assert!(c.advisories().is_empty(), "{:?}", c.advisories());
    }

    #[test]
    fn an_unknown_publisher_table_is_skipped() {
        assert!(
            ok("[tdm.wiley]\napi_key = \"abc\"\n").is_empty(),
            "unknown publishers grant nothing"
        );
    }

    #[test]
    fn an_absent_tdm_table_is_not_an_error() {
        assert!(ok("[something_else]\nx = 1\n").is_empty());
        assert!(ok("").is_empty());
    }

    /// A well-formed file must have nothing to say.
    #[test]
    fn a_clean_file_produces_no_advisories() {
        let c = ok("[tdm.elsevier]\napi_key = \"abc\"\n");
        assert!(c.advisories().is_empty(), "{:?}", c.advisories());
    }
}
