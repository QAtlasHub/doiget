#![no_main]
//! Fuzz target for [`doiget_core::ArxivId::parse`].
//!
//! Parsing is the trust boundary for untrusted input received via the CLI
//! and the MCP server. See `docs/SECURITY.md` §1.1.
//!
//! The harness asserts that `ArxivId::parse` never panics on arbitrary
//! input; it must always return either `Ok(ArxivId)` or
//! `Err(RefParseError)`.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = doiget_core::ArxivId::parse(s);
    }
});
