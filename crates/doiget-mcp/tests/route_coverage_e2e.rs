//! #462: which route produced the outcome, asserted per route.
//!
//! Four "unreachable source" bugs shipped with green unit tests -- #413, #442,
//! #454, #458 -- and they share one shape: a source was implemented, gated,
//! allowlisted and unit-tested, and was never reached. The unit test drove the
//! `Source` impl directly, or asserted a builder returned the right value, and
//! the production entry point was never in the picture. #454's own guard
//! carried a doc comment describing the failure it could not catch.
//!
//! The thing a unit test cannot do is notice that a correct component is never
//! reached. Only an assertion about WHICH ROUTE ran can do that.
//!
//! Measured before writing this file: of the five `PdfLegStatus` routes,
//! exactly **one** (`blocked`) was asserted anywhere in the e2e suites.
//! `tdm_fetched` had none -- which is why #458, "the Tier-3 chain is skipped
//! whenever Crossref answers", could ship.
//!
//! ## What this file is
//!
//! Not more tests. A **registry**: every route, and either the test that
//! asserts it or a stated reason it is not asserted yet. A gap recorded with a
//! reason is a gap someone can close; a gap that is merely absent is the one
//! that ships.
//!
//! Two checks keep it honest:
//!
//! * the named covering test must exist AND contain the route string, so a
//!   claim of coverage cannot outlive the assertion it names;
//! * a posture-lint step compares this list against the `PdfLegStatus`
//!   variants in `doiget-core`, so a new route cannot be added without landing
//!   here first.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use camino::Utf8PathBuf;

/// How a route is covered.
#[derive(Debug)]
enum Coverage {
    /// Asserted by `fn` in the given test file.
    By {
        file: &'static str,
        test_fn: &'static str,
    },
    /// Not asserted yet, and why. Deliberately not `None`: the reason is the
    /// difference between a known gap and an oversight.
    Gap { why: &'static str },
}

/// Every `PdfLegStatus` route, by its wire name.
///
/// Kept in the order the enum declares them so a reviewer can diff the two by
/// eye; the posture-lint does it mechanically.
const ROUTES: &[(&str, Coverage)] = &[
    (
        "fetched",
        Coverage::By {
            file: "fetch_paper_e2e.rs",
            test_fn: "fetch_paper_arxiv_happy_path_writes_pdf_and_returns_envelope",
        },
    ),
    (
        "no_oa_url",
        Coverage::Gap {
            why: "no e2e drives a DOI that resolves with no OA location anywhere; \
                  the CLI-side found-nothing trace (#505) exercises the reporting \
                  but not the route",
        },
    ),
    (
        "blocked",
        Coverage::By {
            file: "fetch_paper_e2e.rs",
            test_fn: "fetch_paper_doi_blocked_pdf_includes_suggested_arxiv_id",
        },
    ),
    (
        "preprint_fallback",
        Coverage::Gap {
            why: "the #325 automatic fallback. The existing blocked-leg test asserts \
                  `suggested_arxiv_id`, which is the SUGGESTION, not the fallback \
                  having run -- two different routes one letter apart in the envelope",
        },
    ),
    (
        "tdm_fetched",
        Coverage::Gap {
            why: "needs a tdm-* feature build plus credentials, so it cannot run in \
                  the oa-only job. This is the route #458 broke, and it had no \
                  assertion anywhere, which is why #458 shipped",
        },
    ),
];

fn tests_dir() -> Utf8PathBuf {
    Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests")
}

/// A claim of coverage must name a test that exists and that mentions the
/// route. Without this the registry decays into a list of intentions: the
/// covering test can be renamed or its assertion deleted, and nothing notices.
#[test]
fn every_claimed_covering_test_exists_and_asserts_its_route() {
    let mut problems = Vec::new();
    for (route, cov) in ROUTES {
        let Coverage::By { file, test_fn } = cov else {
            continue;
        };
        let path = tests_dir().join(file);
        let Ok(src) = std::fs::read_to_string(&path) else {
            problems.push(format!("{route}: {file} does not exist"));
            continue;
        };
        if !src.contains(test_fn) {
            problems.push(format!("{route}: {file} has no `{test_fn}`"));
            continue;
        }
        if !src.contains(&format!("\"{route}\"")) {
            problems.push(format!(
                "{route}: {file} names `{test_fn}` but never asserts the string \"{route}\""
            ));
        }
    }
    assert!(
        problems.is_empty(),
        "route coverage claims that are not backed by an assertion:\n  {}",
        problems.join("\n  ")
    );
}

/// The registry must name EVERY route, so a new one cannot be added without
/// deciding how it is covered -- the step all four bugs in the header skipped.
///
/// Reads `PdfLegStatus` out of `doiget-core` and compares. Deliberately here
/// rather than as a shell step in posture-lint: the comparison needs
/// CamelCase-to-snake_case, that needs a regex backreference, and threading one
/// through YAML into bash is how the first attempt at this put a literal
/// control character into the workflow file. A test can just do it.
#[test]
fn the_registry_names_every_pdf_leg_route() {
    let src = std::fs::read_to_string(
        Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../doiget-core/src/orchestrator.rs"),
    )
    .expect("read orchestrator.rs");

    let body = src
        .split_once("pub enum PdfLegStatus")
        .and_then(|(_, rest)| {
            rest.split_once(
                "
}",
            )
        })
        .map(|(body, _)| body)
        .expect("PdfLegStatus enum not found -- did it move or get renamed?");

    let mut variants: Vec<String> = Vec::new();
    for line in body.lines() {
        // A variant is `    Name,` or `    Name {`; anything more indented is
        // a field, and anything less is not in the enum.
        let Some(rest) = line.strip_prefix("    ") else {
            continue;
        };
        if rest.starts_with(' ') || !rest.starts_with(char::is_uppercase) {
            continue;
        }
        let name: String = rest.chars().take_while(|c| c.is_alphanumeric()).collect();
        if name.is_empty() {
            continue;
        }
        let mut snake = String::new();
        for (i, c) in name.chars().enumerate() {
            if c.is_uppercase() && i > 0 {
                snake.push('_');
            }
            snake.extend(c.to_lowercase());
        }
        variants.push(snake);
    }
    variants.sort();
    variants.dedup();

    // The guard the assertion below cannot be: a parser that silently matched
    // nothing would agree with an empty registry.
    assert!(
        variants.len() >= 5,
        "parsed {} variants from PdfLegStatus; the enum has at least five, so          this parser has stopped seeing them: {variants:?}",
        variants.len()
    );

    let mut named: Vec<String> = ROUTES.iter().map(|(r, _)| (*r).to_string()).collect();
    named.sort();

    assert_eq!(
        named, variants,
        "the coverage registry and PdfLegStatus disagree. A route with no entry          is a route nobody decided how to test, which is exactly how #413, #442,          #454 and #458 shipped."
    );
}

/// The gaps are the point of the file, so they are printed rather than hidden.
/// This test does not fail on a gap -- it fails if a gap has no reason, because
/// an unexplained gap is indistinguishable from an oversight, which is the
/// thing #462 is about.
#[test]
fn every_route_is_either_covered_or_has_a_stated_reason() {
    let mut uncovered = Vec::new();
    for (route, cov) in ROUTES {
        match cov {
            Coverage::By { .. } => {}
            Coverage::Gap { why } => {
                assert!(
                    why.len() > 30,
                    "{route}: a gap needs a reason someone can act on, got {why:?}"
                );
                uncovered.push(*route);
            }
        }
    }
    // `eprintln!` is banned in this crate -- stdout is the JSON-RPC channel and
    // the lint does not distinguish the two streams. Asserting the count is
    // better than printing it anyway: it turns "2 of 5" from a line someone
    // might read into a number that has to be updated when it changes.
    assert_eq!(
        uncovered.len(),
        3,
        "known gaps changed: {uncovered:?}. If a route just gained an assertion,          move it from `Gap` to `By` and drop this count by one -- the point is          that closing a gap is a visible edit, not a silent improvement."
    );
}
