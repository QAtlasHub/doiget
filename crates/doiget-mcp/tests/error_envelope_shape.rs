//! Every `ok: false` envelope carries a structured `error` OBJECT (ADR-0055).
//!
//! `docs/ERRORS.md` §3 names `every_bare_string_error_site_is_a_known_one` as
//! the guard that pins which tools have not got there yet, "so it can shrink
//! but not grow". The guard did not exist. The document was accurate about
//! the four tools it listed and wrong about the mechanism keeping the list
//! honest -- a claim about the world resting on code that was never written,
//! which is the defect class that document defines.
//!
//! It exists now, and the set it pins is EMPTY: `doiget_resolve_citation`,
//! `doiget_batch_resolve_citations`, `doiget_tag` and `doiget_annotate` build
//! `error_object` like every other tool. Kept as a guard rather than deleted
//! along with the exemption, because the failure mode is a new `json!` literal
//! typing `"error": format!(...)` -- which is how the four got there.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use camino::Utf8PathBuf;

fn router_src() -> Utf8PathBuf {
    Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs")
}

/// Every place the router names an `error` field, in BOTH forms it uses.
///
/// Two forms exist, and the first version of this file knew about one:
///
/// ```text
/// json!({ ..., "error": <value> })          // matched
/// map.insert("error".into(), <value>)       // INVISIBLE
/// ```
///
/// `fetch_paper_error_envelope` uses the second, and it backs the
/// `INVALID_REF` / `STORE_ERROR` / `LOG_ERROR` / `INTERNAL_ERROR` arms of
/// `doiget_fetch_paper` -- among the most reachable failures in the crate.
/// So the guard whose docstring called the exempt set EMPTY could not see the
/// busiest envelope builder in the file. Found by review.
fn error_field_value(line: &str) -> Option<&str> {
    for key in [
        "\"error\":",
        "insert(\"error\".into(),",
        "insert(\"error\".to_string(),",
    ] {
        if let Some((_, rest)) = line.split_once(key) {
            return Some(rest);
        }
    }
    None
}

/// The accepted right-hand sides for an `error` field.
///
/// `error_object(..)` is the builder; `error_obj` is the `serde_json::Map`
/// the hand-assembled envelopes fill in and insert. Both are objects.
fn is_structured(value: &str) -> bool {
    let v = value.trim_start();
    if v.starts_with("error_object(") {
        return true;
    }
    // Exact identifier, not a prefix. `v.starts_with("error_obj")` also
    // accepted `error_obj_msg`, so a bare-string regression could be waved
    // through by naming the local variable carefully -- the guard defeated by
    // spelling, which is the failure it exists to prevent.
    v.strip_prefix("error_obj")
        .is_some_and(|rest| !rest.starts_with(|c: char| c.is_alphanumeric() || c == '_'))
}

#[test]
fn every_bare_string_error_site_is_a_known_one() {
    let src = std::fs::read_to_string(router_src()).expect("router source is readable");

    let mut bare = Vec::new();
    for (lineno, line) in src.lines().enumerate() {
        let trimmed = line.trim_start();
        // Prose is not an envelope.
        if trimmed.starts_with("//") {
            continue;
        }
        let Some(rest) = error_field_value(trimmed) else {
            continue;
        };
        // A key with its value on the next line is written `"error":` alone;
        // the builder call is what follows, so look there instead.
        let value = if rest.trim().is_empty() {
            src.lines().nth(lineno + 1).unwrap_or_default()
        } else {
            rest
        };
        if !is_structured(value) {
            bare.push(format!("src/lib.rs:{}: {}", lineno + 1, trimmed));
        }
    }

    // One assertion, not two. The first version kept a
    // `KNOWN_BARE_STRING_TOOLS` exemption list and told a failing
    // contributor to add their tool to it -- advice that could not work,
    // because the second assertion checked `bare.is_empty()`
    // unconditionally and never consulted the list. An escape hatch that
    // does not open is worse than none: it sends the next person down a
    // path ending in rewriting the test anyway.
    assert!(
        bare.is_empty(),
        "an ok:false envelope answers with a bare string instead of error_object(..), so the caller has no code to branch on and no disposition to decide a retry from (ADR-0055). Build the object; if a tool genuinely cannot, say so in docs/ERRORS.md and change this test deliberately:
  {}",
        bare.join("
  ")
    );
}

/// The document must not still tell readers that four tools are exempt when
/// none are. This is the half of the original pairing that was doing real
/// work -- a doc describing a state the code left behind is the historical
/// defect -- and it needs no exemption list to detect it.
#[test]
fn the_document_does_not_claim_exemptions_that_no_longer_exist() {
    let errors_md = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/ERRORS.md");
    let doc = std::fs::read_to_string(&errors_md).expect("docs/ERRORS.md is readable");
    assert!(
        !doc.contains("do not yet emit that object at all"),
        "docs/ERRORS.md section 3 still says some tools answer with a bare string in error, but every_bare_string_error_site_is_a_known_one finds none. Update the document, or this guard is vouching for prose nobody checked."
    );
}
