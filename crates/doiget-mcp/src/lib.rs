//! doiget MCP server (stdio).
//!
//! Phase 0 ships only this skeleton. The actual JSON-RPC loop, tool dispatch,
//! and 9-tool surface land in Phase 3.
//!
//! See `docs/MCP_TOOLS.md` for the tool spec and `ADR-0001` for the stdio-only
//! transport decision.

#![warn(missing_docs)]
#![forbid(unsafe_code)]
// Stricter than the workspace lint: doiget-mcp must NEVER write to stdout outside
// JSON-RPC frames. See docs/SECURITY.md §3.
#![deny(clippy::print_stdout)]

use doiget_core::CapabilityProfile;

/// MCP server handle. Owns the resolved `CapabilityProfile` and (in Phase 3) a
/// `tokio` runtime that drives the JSON-RPC loop on stdin / stdout.
#[derive(Debug)]
pub struct Server {
    profile: CapabilityProfile,
}

impl Server {
    /// Construct a server with the given runtime capability profile.
    pub fn new(profile: CapabilityProfile) -> Self {
        Self { profile }
    }

    /// Run the MCP server until stdin reaches EOF (Phase 3+).
    ///
    /// Phase 0 returns an error indicating the server is not yet implemented.
    pub async fn run(&self) -> anyhow::Result<()> {
        let _ = &self.profile;
        anyhow::bail!(
            "doiget-mcp v{} (Phase 0): MCP server is not yet implemented. \
             See docs/PHASES.md.",
            doiget_core::VERSION
        )
    }
}
