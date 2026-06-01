// allow: outbound-network
//! `doiget version [--check]` — print the current version and optionally
//! query GitHub Releases for the latest stable tag.
//!
//! Without `--check` the command prints the compiled-in version string and
//! exits immediately (no network, no side-effects).
//!
//! With `--check` it queries the GitHub Releases API, compares with the
//! compiled-in version, and emits a machine-readable JSON object:
//!
//! ```json
//! {
//!   "current":          "0.4.1-beta.0",
//!   "latest":           "0.4.0",
//!   "newer_available":  false,
//!   "html_url":         "https://github.com/…/releases/tag/v0.4.0"
//! }
//! ```
//!
//! When the check fails (rate-limited, network down) the `latest` and
//! `html_url` fields are `null` and an `error` key is populated instead,
//! so callers never receive a hard error just because GitHub is slow.

use std::io::Write;

use anyhow::Result;
use serde::Serialize;

use super::output::OutputMode;

const CURRENT: &str = env!("CARGO_PKG_VERSION");

const RELEASES_API: &str = "https://api.github.com/repos/sotashimozono/doiget/releases";

/// Output shape for `--check` in JSON mode.
#[derive(Debug, Serialize)]
pub struct VersionCheckResult {
    /// Current compiled-in version.
    pub current: &'static str,
    /// Latest stable tag without the `v` prefix (e.g. `"0.4.0"`).
    /// `null` when the check could not complete.
    pub latest: Option<String>,
    /// `true` when `latest` is set and strictly newer than `current`.
    /// `null` when `latest` is `null`.
    pub newer_available: Option<bool>,
    /// GitHub release page URL. `null` when the check could not complete.
    pub html_url: Option<String>,
    /// Human-readable error reason when the check failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Entry point for `doiget version [--check]`.
pub async fn run(check: bool, mode: OutputMode) -> Result<()> {
    if mode == OutputMode::Quiet || mode == OutputMode::Mcp {
        return Ok(());
    }

    if !check {
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        if mode == OutputMode::Json {
            writeln!(
                out,
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({ "version": CURRENT }))?
            )?;
        } else {
            writeln!(out, "doiget {CURRENT}")?;
        }
        return Ok(());
    }

    let result = fetch_latest().await;

    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    if mode == OutputMode::Json {
        writeln!(out, "{}", serde_json::to_string_pretty(&result)?)?;
    } else {
        match (&result.latest, &result.newer_available) {
            (Some(latest), Some(true)) => {
                writeln!(out, "doiget {CURRENT} — update available: {latest}")?;
                if let Some(url) = &result.html_url {
                    writeln!(out, "  {url}")?;
                }
            }
            (Some(latest), _) => {
                writeln!(
                    out,
                    "doiget {CURRENT} — up to date (latest stable: {latest})"
                )?;
            }
            (None, _) => {
                let reason = result.error.as_deref().unwrap_or("unknown");
                writeln!(out, "doiget {CURRENT} — version check failed: {reason}")?;
            }
        }
    }

    Ok(())
}

/// Query the GitHub Releases API and return the latest stable (non-prerelease,
/// non-draft) release. Returns a populated `error` field on any failure so
/// callers are never hard-blocked by a network hiccup.
async fn fetch_latest() -> VersionCheckResult {
    match try_fetch_latest().await {
        Ok(r) => r,
        Err(e) => VersionCheckResult {
            current: CURRENT,
            latest: None,
            newer_available: None,
            html_url: None,
            error: Some(e.to_string()),
        },
    }
}

async fn try_fetch_latest() -> anyhow::Result<VersionCheckResult> {
    let client = reqwest::Client::builder()
        .user_agent(format!("doiget/{CURRENT}"))
        .build()?;

    let resp = client.get(RELEASES_API).send().await.map_err(|e| {
        if e.is_connect() || e.is_timeout() {
            anyhow::anyhow!("unreachable")
        } else {
            anyhow::anyhow!("{e}")
        }
    })?;

    if resp.status().as_u16() == 403 {
        return Err(anyhow::anyhow!("rate_limited"));
    }

    let releases: serde_json::Value = resp.error_for_status()?.json().await?;

    let arr = releases
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("unexpected API shape"))?;

    // First non-draft, non-prerelease release without a pre-release suffix.
    let stable = arr.iter().find(|r| {
        let draft = r.get("draft").and_then(|v| v.as_bool()).unwrap_or(true);
        let prerelease = r
            .get("prerelease")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        if draft || prerelease {
            return false;
        }
        let tag = r.get("tag_name").and_then(|v| v.as_str()).unwrap_or("");
        let bare = tag.strip_prefix('v').unwrap_or(tag);
        !has_prerelease_suffix(bare)
    });

    let Some(release) = stable else {
        return Err(anyhow::anyhow!("no stable release found"));
    };

    let tag = release
        .get("tag_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing tag_name"))?;
    let html_url = release
        .get("html_url")
        .and_then(|v| v.as_str())
        .map(String::from);
    let latest = tag.strip_prefix('v').unwrap_or(tag).to_string();

    Ok(VersionCheckResult {
        current: CURRENT,
        newer_available: Some(is_newer(&latest, CURRENT)),
        latest: Some(latest),
        html_url,
        error: None,
    })
}

/// Return `true` when `candidate` is strictly newer than `current` by
/// dot-separated numeric comparison on the version core (before any `-`
/// pre-release suffix).
fn is_newer(candidate: &str, current: &str) -> bool {
    let parse = |s: &str| -> Vec<u64> {
        let core = s.split('-').next().unwrap_or(s);
        core.split('.').map(|p| p.parse().unwrap_or(0)).collect()
    };
    parse(candidate) > parse(current)
}

/// Return `true` when a version string has a known pre-release suffix.
fn has_prerelease_suffix(version: &str) -> bool {
    let lower = version.to_ascii_lowercase();
    lower.contains("-beta") || lower.contains("-alpha") || lower.contains("-rc")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_newer_detects_upgrade() {
        assert!(is_newer("0.5.0", "0.4.0"));
        assert!(is_newer("0.4.1", "0.4.0"));
        assert!(!is_newer("0.4.0", "0.4.0"));
        assert!(!is_newer("0.3.9", "0.4.0"));
    }

    #[test]
    fn is_newer_ignores_prerelease_suffix_in_current() {
        assert!(!is_newer("0.4.0", "0.4.1-beta.0"));
        assert!(is_newer("0.5.0", "0.4.1-beta.0"));
    }

    #[test]
    fn has_prerelease_suffix_cases() {
        assert!(has_prerelease_suffix("0.4.1-beta.0"));
        assert!(has_prerelease_suffix("1.0.0-alpha"));
        assert!(has_prerelease_suffix("1.0.0-rc.1"));
        assert!(!has_prerelease_suffix("0.4.0"));
    }
}
