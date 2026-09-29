//! What this binary is and how it was installed -- without a network call
//! (#594).
//!
//! An agent ran a doiget four releases old through the one surface that
//! consumes `oa_url` programmatically, and nothing in the session said so:
//! `doiget_health` reported a version with nothing to compare it against.
//! A version check against the latest release is a network call nobody asked
//! for, which ADR-0015 rules out; `doiget version --check` exists for a user
//! who does ask. What can be reported for free is the rest of the picture:
//!
//! - the **release channel** the version belongs to (`stable`, or `beta` for
//!   a `-beta.N` build);
//! - **which binary is running** -- `current_exe`, the thing an MCP config
//!   names by path and a user never sees;
//! - **how it was installed**, from the manifest `scripts/install.sh` /
//!   `install.ps1` leave beside the binary, or else from where the binary
//!   lives (npm, cargo, Homebrew, Nix, a Claude Desktop `.mcpb` extension);
//! - the **command that updates it** for that install method.
//!
//! A manifest whose version differs from the running binary means the file
//! was replaced by something other than the installer, which is said too.

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

/// File name of the manifest the installers write next to the binary.
pub const MANIFEST_NAME: &str = "doiget.install.json";

/// What an installer recorded about the binary it placed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallManifest {
    /// `install.sh` or `install.ps1`.
    pub installer: String,
    /// The version it installed.
    pub version: String,
    /// The release asset.
    #[serde(default)]
    pub asset: Option<String>,
    /// The verified SHA-256 of that asset.
    #[serde(default)]
    pub sha256: Option<String>,
    /// RFC 3339 UTC.
    #[serde(default)]
    pub installed_at: Option<String>,
}

/// The build and install report.
#[derive(Debug, Clone, Serialize)]
pub struct InstallInfo {
    /// `CARGO_PKG_VERSION` of the running binary.
    pub version: &'static str,
    /// `stable`, or `beta` for a pre-release version.
    pub channel: &'static str,
    /// The running binary, when the OS reports it.
    pub binary: Option<Utf8PathBuf>,
    /// How it was installed: `install.sh`, `install.ps1`, `npm`, `cargo`,
    /// `homebrew`, `nix`, `mcpb`, or `unknown`.
    pub method: &'static str,
    /// The installer's manifest, when one sits beside the binary.
    pub manifest: Option<InstallManifest>,
    /// `false` when a manifest names a different version than the running
    /// binary: the file was replaced other than by the installer.
    pub manifest_matches: Option<bool>,
    /// How to update an install of this kind. doiget never updates itself.
    pub update: &'static str,
    /// How to compare with the latest release: an explicit request, never
    /// made on the user's behalf (ADR-0015).
    pub check: &'static str,
}

/// Report on the running binary. Reads at most one small local file.
#[must_use]
pub fn install_info() -> InstallInfo {
    let binary = std::env::current_exe()
        .ok()
        .and_then(|p| std::fs::canonicalize(&p).ok().or(Some(p)))
        .and_then(|p| Utf8PathBuf::from_path_buf(p).ok());
    let manifest = binary
        .as_ref()
        .and_then(|b| b.parent().map(|d| d.join(MANIFEST_NAME)))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| parse_manifest(&s));
    describe(crate::VERSION, binary, manifest)
}

/// The manifest's text as an [`InstallManifest`], or `None` when it is not
/// one: an unreadable manifest is reported as no manifest, and the method is
/// read from the path instead. A leading BOM is allowed -- Windows
/// PowerShell 5.1 writes one with `-Encoding UTF8`, and older `install.ps1`
/// runs did.
#[must_use]
pub fn parse_manifest(text: &str) -> Option<InstallManifest> {
    serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()
}

/// What an installer records when the new binary would not report its
/// version: it names no version, so it is compared with none.
pub const UNKNOWN_VERSION: &str = "unknown";

/// [`install_info`] from its inputs, for tests.
#[must_use]
pub fn describe(
    version: &'static str,
    binary: Option<Utf8PathBuf>,
    manifest: Option<InstallManifest>,
) -> InstallInfo {
    let method = match &manifest {
        Some(m) if m.installer == "install.ps1" => "install.ps1",
        Some(_) => "install.sh",
        None => binary
            .as_ref()
            .map_or("unknown", |b| method_from_path(b.as_str())),
    };
    let manifest_matches = manifest
        .as_ref()
        .filter(|m| m.version != UNKNOWN_VERSION)
        .map(|m| m.version == version);
    InstallInfo {
        version,
        channel: if version.contains('-') {
            "beta"
        } else {
            "stable"
        },
        binary,
        method,
        manifest,
        manifest_matches,
        update: update_command(method),
        check: "doiget version --check (asks GitHub Releases; doiget never checks on its own)",
    }
}

/// Infer the install method from where the binary lives.
fn method_from_path(path: &str) -> &'static str {
    let p = path.replace('\\', "/").to_lowercase();
    if p.contains("/node_modules/") {
        "npm"
    } else if p.contains("/.cargo/bin/") {
        "cargo"
    } else if p.contains("/cellar/") || p.contains("/homebrew/") || p.contains("/linuxbrew/") {
        "homebrew"
    } else if p.starts_with("/nix/store/") {
        "nix"
    } else if p.contains("claude extensions") || p.contains("/claude/extensions/") {
        "mcpb"
    } else {
        "unknown"
    }
}

fn update_command(method: &str) -> &'static str {
    match method {
        "install.sh" => {
            "re-run: curl -fsSL https://raw.githubusercontent.com/QAtlasHub/doiget/main/scripts/install.sh | sh"
        }
        "install.ps1" => {
            "re-run: irm https://raw.githubusercontent.com/QAtlasHub/doiget/main/scripts/install.ps1 | iex"
        }
        "npm" => {
            "npm install -g doiget-cli@latest (npx -y doiget-cli runs the latest unless a version is pinned)"
        }
        "cargo" => "cargo install doiget-cli --locked",
        "homebrew" => "brew upgrade doiget",
        "nix" => "nix profile upgrade doiget (or update the flake input)",
        "mcpb" => {
            "install the .mcpb from the latest GitHub release: a Desktop Extension is not updated automatically"
        }
        _ => "reinstall from https://github.com/QAtlasHub/doiget/releases/latest",
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn m(installer: &str, version: &str) -> InstallManifest {
        InstallManifest {
            installer: installer.into(),
            version: version.into(),
            asset: None,
            sha256: None,
            installed_at: None,
        }
    }

    #[test]
    fn the_channel_follows_the_version() {
        assert_eq!(describe("0.9.0", None, None).channel, "stable");
        assert_eq!(describe("0.9.0-beta.3", None, None).channel, "beta");
        // Any pre-release is off the stable channel, not only -beta.N.
        assert_eq!(describe("0.9.0-rc.1", None, None).channel, "beta");
    }

    #[test]
    fn a_manifest_with_a_bom_parses_and_a_malformed_one_is_no_manifest() {
        let json = r#"{"installer":"install.ps1","version":"0.9.0"}"#;
        let with_bom = format!("\u{feff}{json}");
        assert_eq!(
            parse_manifest(&with_bom).map(|m| m.installer),
            Some("install.ps1".to_string())
        );
        assert_eq!(parse_manifest("{not json"), None);
        assert_eq!(
            parse_manifest(r#"{"version":"0.9.0"}"#),
            None,
            "no installer"
        );
    }

    #[test]
    fn an_unknown_manifest_version_is_not_called_a_replacement() {
        let info = describe("0.9.0", None, Some(m("install.sh", UNKNOWN_VERSION)));
        assert_eq!(info.method, "install.sh");
        assert_eq!(info.manifest_matches, None);
    }

    /// The #594 case: the binary an MCP config names, installed by
    /// install.ps1, later replaced by hand.
    #[test]
    fn a_manifest_names_the_installer_and_a_replaced_binary_is_flagged() {
        let bin = Some(Utf8PathBuf::from(
            "C:/Users/u/AppData/Local/Programs/doiget/doiget.exe",
        ));
        let info = describe("0.8.12", bin.clone(), Some(m("install.ps1", "0.8.9")));
        assert_eq!(info.method, "install.ps1");
        assert_eq!(info.manifest_matches, Some(false));
        assert!(info.update.contains("install.ps1"));
        let info = describe("0.8.12", bin, Some(m("install.ps1", "0.8.12")));
        assert_eq!(info.manifest_matches, Some(true));
    }

    #[test]
    fn without_a_manifest_the_method_is_read_from_the_path() {
        for (path, method) in [
            (
                "/home/u/.npm/_npx/abc/node_modules/doiget-cli-linux-x64/bin/doiget",
                "npm",
            ),
            ("/home/u/.cargo/bin/doiget", "cargo"),
            ("/opt/homebrew/Cellar/doiget/0.8.13/bin/doiget", "homebrew"),
            ("/nix/store/abc-doiget-0.8.13/bin/doiget", "nix"),
            (
                "/Users/u/Library/Application Support/Claude/Claude Extensions/doiget/server/doiget",
                "mcpb",
            ),
            ("/usr/local/bin/doiget", "unknown"),
        ] {
            let info = describe("0.9.0", Some(Utf8PathBuf::from(path)), None);
            assert_eq!(info.method, method, "{path}");
            assert_eq!(info.manifest_matches, None);
        }
        assert!(describe("0.9.0", None, None)
            .check
            .contains("doiget version --check"));
    }
}
