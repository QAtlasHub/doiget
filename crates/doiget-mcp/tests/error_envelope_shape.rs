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

/// Tools still permitted to answer with a bare string in `error`.
///
/// EMPTY, deliberately. An entry here is a promise to a caller that it cannot
/// branch on the failure, so adding one needs a reason in `docs/ERRORS.md`
/// §3 rather than a line here.
const KNOWN_BARE_STRING_TOOLS: &[&str] = &[];

fn router_src() -> Utf8PathBuf {
    Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs")
}

/// The accepted right-hand sides for an `"error":` key.
///
/// `error_object(..)` is the builder; `error_obj` is the `serde_json::Map`
/// the five hand-assembled envelopes fill in and insert. Both are objects.
fn is_structured(value: &str) -> bool {
    let v = value.trim_start();
    v.starts_with("error_object(") || v.starts_with("error_obj")
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
        let Some(rest) = trimmed.split_once("\"error\":").map(|(_, r)| r) else {
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

    assert!(
        bare.is_empty() || !KNOWN_BARE_STRING_TOOLS.is_empty(),
        "an `ok:false` envelope answers with a bare string instead of \
         `error_object(..)`, so the caller has no `code` to branch on and no \
         `disposition` to decide a retry from (ADR-0055). Either build the \
         object, or add the tool to KNOWN_BARE_STRING_TOOLS with a reason in \
         docs/ERRORS.md §3:\n  {}",
        bare.join("\n  ")
    );
    assert!(
        bare.is_empty(),
        "docs/ERRORS.md §3 says this set can shrink but not grow; these are \
         not in KNOWN_BARE_STRING_TOOLS:\n  {}",
        bare.join("\n  ")
    );
}

/// The exemption list is a liability, not a feature: if it is empty the
/// document must not still be telling readers four tools are exempt.
#[test]
fn the_exemption_list_and_the_document_agree() {
    let errors_md = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/ERRORS.md");
    let doc = std::fs::read_to_string(&errors_md).expect("docs/ERRORS.md is readable");
    let doc_claims_exemptions = doc.contains("do not yet emit that object at all");
    assert_eq!(
        doc_claims_exemptions,
        !KNOWN_BARE_STRING_TOOLS.is_empty(),
        "docs/ERRORS.md §3 and KNOWN_BARE_STRING_TOOLS disagree about whether \
         any tool still answers with a bare string (doc says {doc_claims_exemptions}, \
         list has {} entries)",
        KNOWN_BARE_STRING_TOOLS.len()
    );
}
