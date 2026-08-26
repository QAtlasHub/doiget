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
//! The `0600` check is then a real control rather than a sentence.
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

/// Parsed `credentials.toml`.
///
/// Absent, unreadable and malformed all yield the default (no keys): the
/// file is optional, and one bad line must not take a fetch down. Every
/// such case emits `tracing::warn!` — silence about this file is precisely
/// what cost a user their configuration.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct Credentials {
    keys: Vec<(String, String)>,
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
}

#[derive(Debug, Default, Deserialize)]
struct RawFile {
    #[serde(default)]
    tdm: Option<std::collections::BTreeMap<String, RawEntry>>,
    #[serde(flatten)]
    _other: serde::de::IgnoredAny,
}

#[derive(Debug, Default, Deserialize)]
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

/// Load the credentials file, or the empty set.
///
/// Never fails; every failure mode is warned about. See [`Credentials`].
#[must_use]
pub fn load_or_default() -> Credentials {
    let path = match path() {
        Ok(p) => p,
        Err(e) => {
            tracing::debug!(error = %e, "no config directory; credentials.toml not read");
            return Credentials::default();
        }
    };
    let text = match std::fs::read_to_string(path.as_std_path()) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Credentials::default(),
        Err(e) => {
            tracing::warn!(
                path = %path,
                error = %e,
                "credentials.toml exists but could not be read; keys from it are \
                 unavailable and only DOIGET_KEY_* will be used"
            );
            return Credentials::default();
        }
    };
    warn_if_group_or_world_accessible(&path);
    parse(&text, &path)
}

/// Parse a `credentials.toml` body. Pure; the path is for messages only.
fn parse(text: &str, path: &camino::Utf8Path) -> Credentials {
    let raw: RawFile = match toml::from_str(text) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                path = %path,
                error = %e,
                "credentials.toml is malformed; keys from it are unavailable and only \
                 DOIGET_KEY_* will be used"
            );
            return Credentials::default();
        }
    };
    let Some(tdm) = raw.tdm else {
        return Credentials::default();
    };

    let mut keys = Vec::new();
    for (publisher, entry) in tdm {
        if !PUBLISHERS.contains(&publisher.as_str()) {
            tracing::warn!(
                path = %path,
                publisher = %publisher,
                "credentials.toml has [tdm.{}], which is not a publisher doiget knows; \
                 expected one of {:?}",
                publisher,
                PUBLISHERS
            );
            continue;
        }
        if entry.agreed.is_some() {
            // Reported, not obeyed. A field parsed and discarded in silence
            // is exactly what #509 is about.
            tracing::warn!(
                path = %path,
                publisher = %publisher,
                "credentials.toml sets [tdm.{}] agreed, which doiget does NOT read. The \
                 per-publisher agreement is environment-only (DOIGET_AGREE_TDM_{}=1) so \
                 that it is an act taken in the session that runs the fetch — \
                 docs/LEGAL.md §6a.2. Only api_key is read from this file.",
                publisher,
                publisher.to_uppercase()
            );
        }
        // Blank means unset, as everywhere else: an empty key cannot
        // authenticate, and building a grant around one would mask the
        // misconfiguration `AgreedButNoKey` exists to surface.
        let key = entry
            .api_key
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty());
        if let Some(k) = key {
            keys.push((publisher, k));
        }
    }
    Credentials { keys }
}

/// Warn when the file is group- or world-accessible on POSIX.
///
/// `docs/CONFIG.md` §6 promised this warning from the day it was written.
/// It did not exist, and neither did the reader it was attached to (#509).
#[cfg(unix)]
fn warn_if_group_or_world_accessible(path: &camino::Utf8Path) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(meta) = std::fs::metadata(path.as_std_path()) else {
        return;
    };
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        tracing::warn!(
            path = %path,
            mode = format!("{mode:04o}"),
            "credentials.toml is readable beyond its owner (mode {:04o}); it holds \
             publisher API keys. Run: chmod 600 {}",
            mode,
            path
        );
    }
}

/// No-op off POSIX: Windows ACLs are not a mode, and a warning phrased in
/// `chmod` terms would be advice the user cannot follow.
#[cfg(not(unix))]
fn warn_if_group_or_world_accessible(_path: &camino::Utf8Path) {}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn p() -> camino::Utf8PathBuf {
        camino::Utf8PathBuf::from("/tmp/credentials.toml")
    }

    #[test]
    fn an_api_key_is_read_per_publisher() {
        let c = parse(
            "[tdm.elsevier]\napi_key = \"abc\"\n\n[tdm.aps]\napi_key = \"def\"\n",
            &p(),
        );
        assert_eq!(c.api_key("elsevier"), Some("abc"));
        assert_eq!(c.api_key("aps"), Some("def"));
        assert_eq!(c.api_key("springer"), None);
    }

    /// The whole point of the split: the file may carry the key, never the
    /// agreement (`docs/LEGAL.md` §6a.2). There is no accessor for `agreed`
    /// at all, so no caller can be tempted to consult one.
    #[test]
    fn agreed_in_the_file_grants_nothing() {
        let c = parse("[tdm.elsevier]\napi_key = \"abc\"\nagreed = true\n", &p());
        assert_eq!(
            c.api_key("elsevier"),
            Some("abc"),
            "the key is still read; only the agreement is refused"
        );
    }

    #[test]
    fn a_blank_key_is_treated_as_unset() {
        let c = parse("[tdm.aps]\napi_key = \"   \"\n", &p());
        assert!(c.is_empty(), "a blank key cannot authenticate");
    }

    #[test]
    fn a_malformed_file_yields_no_keys_rather_than_a_panic() {
        assert!(parse("this is not toml = = =", &p()).is_empty());
    }

    #[test]
    fn an_unknown_publisher_table_is_skipped() {
        assert!(
            parse("[tdm.wiley]\napi_key = \"abc\"\n", &p()).is_empty(),
            "unknown publishers grant nothing"
        );
    }

    #[test]
    fn an_absent_tdm_table_is_not_an_error() {
        assert!(parse("[something_else]\nx = 1\n", &p()).is_empty());
        assert!(parse("", &p()).is_empty());
    }
}
