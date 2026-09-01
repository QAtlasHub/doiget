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
#[allow(
    dead_code,
    reason = "no route is uncovered today; the variant is how the      next one gets recorded instead of silently omitted"
)]
enum Coverage {
    /// Asserted by `fn` in the given test file.
    By {
        file: &'static str,
        test_fn: &'static str,
        /// The Cargo feature the covering test is `#[cfg]`-gated behind, if
        /// any. `None` means it compiles in the default `oa-only` surface.
        ///
        /// This exists because the checks below read the test file as TEXT.
        /// Text cannot tell "this function is compiled into the binary" from
        /// "this function's source is present in the repository", so a
        /// feature-gated test was accepted as unconditional coverage -- and
        /// the two REQUIRED CI jobs run `--features oa-only`, where that
        /// function does not exist. The registry vouched for a route with no
        /// coverage in the builds that gate merges: this file's own stated
        /// failure class, inside this file.
        feature: Option<&'static str>,
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
            feature: None,
        },
    ),
    (
        "no_oa_url",
        Coverage::By {
            file: "fetch_paper_e2e.rs",
            test_fn: "fetch_paper_doi_with_no_oa_anywhere_reports_the_no_oa_url_route",
            feature: None,
        },
    ),
    (
        "blocked",
        Coverage::By {
            file: "fetch_paper_e2e.rs",
            test_fn: "fetch_paper_doi_blocked_pdf_includes_suggested_arxiv_id",
            feature: None,
        },
    ),
    (
        "preprint_fallback",
        Coverage::By {
            file: "fetch_paper_e2e.rs",
            test_fn: "fetch_paper_doi_falls_back_to_the_arxiv_preprint",
            feature: None,
        },
    ),
    (
        "tdm_fetched",
        // Was the file's one `Gap`, on a reproduction that could not pass
        // because the test harness had no way to register a Tier-3 allowlist
        // entry -- not, as the reason here claimed, because production had
        // regressed to #454's shape.
        Coverage::By {
            file: "fetch_paper_e2e.rs",
            test_fn: "fetch_paper_doi_served_by_the_publisher_reports_the_tdm_fetched_route",
            // Only the `test (tdm features)` CI job compiles this.
            feature: Some("tdm-aps"),
        },
    ),
];

fn tests_dir() -> Utf8PathBuf {
    Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests")
}

/// A claim of coverage must name a test that exists, is NOT `#[ignore]`d, and
/// asserts the route INSIDE ITS OWN BODY.
///
/// The first version was two whole-file string searches: does the file
/// contain the test's name, and does the file contain the route string
/// anywhere. Review pointed out that both are satisfiable without the claim
/// being true -- a comment mentioning the function, plus some unrelated test
/// in the same file containing the literal -- and, worse, that it had no
/// concept of `#[ignore]`. A `By` entry pointing at an ignored test would
/// have passed while CI ran nothing.
///
/// That is the same "correct component that nothing reaches" shape this file
/// exists to catch, reproduced inside the mechanism meant to catch it. So it
/// now extracts the named function's body and looks only there, and refuses a
/// covering test that CI skips.
#[test]
fn every_claimed_covering_test_exists_and_asserts_its_route() {
    let mut problems = Vec::new();
    for (route, cov) in ROUTES {
        let Coverage::By {
            file,
            test_fn,
            feature,
        } = cov
        else {
            continue;
        };
        let path = tests_dir().join(file);
        let Ok(src) = std::fs::read_to_string(&path) else {
            problems.push(format!("{route}: {file} does not exist"));
            continue;
        };

        // The definition, not a mention of the name in prose.
        let Some(def) = src.find(&format!("fn {test_fn}(")) else {
            problems.push(format!("{route}: {file} defines no `fn {test_fn}`"));
            continue;
        };

        // Attributes sit immediately above the fn line. Walk back over the
        // contiguous attribute block and refuse `#[ignore]`: a test CI does
        // not run cannot be evidence that a route is covered.
        let line_start = src[..def].rfind('\n').map_or(0, |i| i + 1);
        let attrs_from = src[..line_start].rfind("\n\n").map_or(0, |i| i + 2);
        // Line-by-line, and only lines that ARE an attribute. A substring search
        // over the whole block also matches a doc comment that DISCUSSES
        // `#[ignore]` -- which the corrected write-up of the TDM route now does,
        // and it made this checker report the very test it was reading about as
        // skipped. Prose is not an attribute; a checker that cannot tell the
        // difference is the defect it exists to catch.
        let is_ignored = src[attrs_from..line_start]
            .lines()
            .map(str::trim_start)
            .filter(|l| l.starts_with("#["))
            // `#[ignore` catches the plain attribute; `ignore)` catches
            // `#[cfg_attr(<cond>, ignore)]`, which cargo skips just as
            // completely and which the first version of this check waved
            // through -- it only compared the start of the line, so a
            // conditionally-ignored test could be claimed as coverage.
            // rustfmt leaves `cfg_attr` on one line, so `cargo fmt` does not
            // rescue us here the way it does for `#[test] #[ignore]`.
            .any(|l| l.starts_with("#[ignore") || l.contains("ignore)"));
        // The registry's feature claim must match the source. A test the
        // default build does not compile is not unconditional coverage, and
        // saying so here is the only place a reader learns it: `cargo test`
        // under `--features oa-only` cannot report a function it never built.
        let attrs = &src[attrs_from..line_start];
        let src_feature = attrs.lines().map(str::trim_start).find_map(|l| {
            let rest = l.strip_prefix("#[cfg(feature = \"")?;
            rest.split('"').next()
        });
        if src_feature != *feature {
            problems.push(format!(
                "{route}: registry says feature={feature:?} but `{test_fn}` is gated                  on {src_feature:?}. A feature-gated test is coverage only in a CI                  job that enables it"
            ));
            continue;
        }

        if is_ignored {
            problems.push(format!(
                "{route}: `{test_fn}` is #[ignore]d, so CI never runs it -- that is a                  Gap with a reason, not coverage"
            ));
            continue;
        }

        // The body only. A line that is exactly `}` ends a top-level fn.
        let body_end = src[def..].find("\n}\n").map_or(src.len(), |i| def + i);
        if !src[def..body_end].contains(&format!("\"{route}\"")) {
            problems.push(format!(
                "{route}: `{test_fn}` never asserts the string \"{route}\" in its own body"
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
        0,
        "known gaps changed: {uncovered:?}. If a route just gained an assertion,          move it from `Gap` to `By` and drop this count by one -- the point is          that closing a gap is a visible edit, not a silent improvement."
    );
}
