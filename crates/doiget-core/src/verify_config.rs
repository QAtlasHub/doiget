//! `[verify]` section of the user `config.toml`, read by `doiget verify`.
//!
//! Mirrors [`crate::user_extension`]: this is a minimal, section-scoped
//! reader (NOT the full `docs/CONFIG.md` resolution ladder). It parses
//! only the `[verify]` table, tolerates every other section, and treats
//! a missing file as "all defaults" rather than an error. CLI flags
//! (`--strict`) layer on top in the command handler; this module knows
//! nothing about flags or env vars.
//!
//! ```toml
//! [verify]
//! # How to treat an entry that carries no DOI / arXiv id:
//! #   "warn"  — report it, do not fail the run (default)
//! #   "error" — count it as a failure (exit non-zero)
//! #   "skip"  — do not emit a record for it at all
//! on_missing_id = "warn"
//! # Treat a well-formed id that does not resolve as a failure.
//! strict = false
//! ```

use camino::Utf8Path;
use serde::Deserialize;
use thiserror::Error;

/// Policy for an entry with no DOI / arXiv identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnMissingId {
    /// Report the entry but do not fail the run.
    #[default]
    Warn,
    /// Count the entry as a failure (non-zero exit).
    Error,
    /// Do not emit a record for the entry at all.
    Skip,
}

/// Resolved `[verify]` configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct VerifyConfig {
    /// Treat a well-formed-but-non-resolving id as a failure.
    pub strict: bool,
    /// Policy for id-less entries.
    pub on_missing_id: OnMissingId,
}

/// Why loading the `[verify]` section failed.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum VerifyConfigError {
    /// Filesystem error other than not-found (which is not an error).
    #[error("reading verify config {path}: {source}")]
    Io {
        /// The config path that could not be read.
        path: String,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The file is not valid TOML, or `[verify]` has an unknown key /
    /// an invalid `on_missing_id` value.
    #[error("parsing verify config {path}: {message}")]
    Parse {
        /// The config path that failed to parse.
        path: String,
        /// `toml::de::Error::to_string()`.
        message: String,
    },
}

/// Top-level shape; only `[verify]` is load-bearing. Other sections
/// (e.g. `[network]`) are tolerated so this reader can run against the
/// same `config.toml` the user-extension loader reads.
#[derive(Debug, Default, Deserialize)]
struct RawConfig {
    #[serde(default)]
    verify: Option<RawVerify>,
    #[serde(flatten)]
    _other: serde::de::IgnoredAny,
}

/// The `[verify]` table. `deny_unknown_fields` so a typo
/// (`on_missing_ids = …`) fails loudly rather than silently defaulting.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawVerify {
    #[serde(default)]
    strict: bool,
    #[serde(default)]
    on_missing_id: OnMissingId,
}

/// Load the `[verify]` section from `config_path`.
///
/// A missing file yields [`VerifyConfig::default`] (not an error), so a
/// user who never wrote a `config.toml` gets the lenient defaults. A
/// present-but-malformed file is an error the caller surfaces (verify
/// degrades to defaults with a warning rather than aborting).
pub fn load(config_path: &Utf8Path) -> Result<VerifyConfig, VerifyConfigError> {
    let text = match std::fs::read_to_string(config_path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(VerifyConfig::default());
        }
        Err(e) => {
            return Err(VerifyConfigError::Io {
                path: config_path.to_string(),
                source: e,
            });
        }
    };
    let raw: RawConfig = toml::from_str(&text).map_err(|e| VerifyConfigError::Parse {
        path: config_path.to_string(),
        message: e.to_string(),
    })?;
    Ok(match raw.verify {
        Some(v) => VerifyConfig {
            strict: v.strict,
            on_missing_id: v.on_missing_id,
        },
        None => VerifyConfig::default(),
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;

    fn write(dir: &tempfile::TempDir, body: &str) -> Utf8PathBuf {
        let p = Utf8PathBuf::try_from(dir.path().join("config.toml")).expect("utf-8");
        std::fs::write(&p, body).expect("write config");
        p
    }

    #[test]
    fn missing_file_is_defaults() {
        let cfg = load(Utf8Path::new("/no/such/config.toml")).expect("missing is ok");
        assert_eq!(cfg, VerifyConfig::default());
        assert_eq!(cfg.on_missing_id, OnMissingId::Warn);
        assert!(!cfg.strict);
    }

    #[test]
    fn empty_or_unrelated_sections_are_defaults() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = write(&dir, "[network]\nadditional_hosts = []\n");
        let cfg = load(&p).expect("parses");
        assert_eq!(cfg, VerifyConfig::default());
    }

    #[test]
    fn reads_on_missing_id_and_strict() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = write(&dir, "[verify]\non_missing_id = \"error\"\nstrict = true\n");
        let cfg = load(&p).expect("parses");
        assert_eq!(cfg.on_missing_id, OnMissingId::Error);
        assert!(cfg.strict);
    }

    #[test]
    fn skip_value_parses() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = write(&dir, "[verify]\non_missing_id = \"skip\"\n");
        let cfg = load(&p).expect("parses");
        assert_eq!(cfg.on_missing_id, OnMissingId::Skip);
    }

    #[test]
    fn unknown_key_in_verify_is_an_error() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = write(&dir, "[verify]\non_missing_ids = \"warn\"\n");
        assert!(matches!(load(&p), Err(VerifyConfigError::Parse { .. })));
    }

    #[test]
    fn invalid_on_missing_id_value_is_an_error() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = write(&dir, "[verify]\non_missing_id = \"sometimes\"\n");
        assert!(matches!(load(&p), Err(VerifyConfigError::Parse { .. })));
    }
}
