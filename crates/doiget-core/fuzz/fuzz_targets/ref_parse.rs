#![no_main]
//! Fuzz target for [`doiget_core::Ref::parse`].
//!
//! `Ref::parse` is the dispatcher used by the CLI (`doiget fetch <ref>`)
//! and the MCP `doiget_fetch` tool to interpret untrusted input. It
//! auto-detects DOI vs arXiv shape; see `docs/SECURITY.md` §1.1.
//!
//! The harness asserts that `Ref::parse` never panics on arbitrary input;
//! it must always return either `Ok(Ref)` or `Err(RefParseError)`.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = doiget_core::Ref::parse(s);
    }
});
