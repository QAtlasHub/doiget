#![no_main]
//! Fuzz target for [`doiget_core::Doi::parse`].
//!
//! Parsing is the trust boundary for untrusted input received via the CLI
//! and the MCP server. See `docs/SECURITY.md` §1.1.
//!
//! The harness asserts that `Doi::parse` never panics on arbitrary input;
//! it must always return either `Ok(Doi)` or `Err(RefParseError)`.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // `parse` takes `&str`, so reject non-UTF-8 inputs early. libFuzzer
    // explores both the UTF-8 and non-UTF-8 input spaces; the cheap
    // `from_utf8` check just routes the latter past the validator.
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = doiget_core::Doi::parse(s);
    }
});
