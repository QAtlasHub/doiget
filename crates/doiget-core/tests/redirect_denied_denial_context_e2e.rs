// allow: outbound-network
//! C1 fix coverage: production redirect denials surface a `DenialContext`
//! through the source-chain walk in `From<&HttpError> for
//! Option<DenialContext>`.
//!
//! `// allow: outbound-network` on line 1 opts this file out of the
//! `network-purity` posture-lint guard. All HTTP terminates at a local
//! `127.0.0.1:N` wiremock origin (see "Network purity" below); the opt-out
//! is for the `reqwest::Url` import that the wiremock client builder
//! requires, NOT for real network access.
//!
//! Why this lives here (not in `http.rs::tests`):
//!
//! `reqwest`'s `Policy::custom` wraps the custom error returned by
//! `attempt.error(HttpError::RedirectDenied{..})` inside a `reqwest::Error`,
//! which surfaces to callers as `HttpError::Network(reqwest_err)`. Callers
//! that produce `denial_context` MUST therefore walk the
//! `std::error::Error::source()` chain on the inner `reqwest::Error` and
//! downcast each link to `&HttpError`. Without that walk, the most
//! operationally important denial class (production redirect denial)
//! produces NO `DenialContext` — see ADR-0023 §4 and the pre-fix bug
//! discovered during the multi-agent review of PR #84.
//!
//! This integration test pins the post-fix behaviour end-to-end against a
//! `wiremock` origin that 302s to a host outside the allowlist, plus a
//! synthetic abstract-level test that pins the source-walking algorithm
//! itself (in case reqwest's wrapping topology changes).
//!
//! ## Network purity
//!
//! All HTTP traffic terminates at `127.0.0.1:N` via `wiremock::MockServer`;
//! no outbound DNS / TCP. Compatible with the workspace network-purity
//! guard.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use doiget_core::http::{HttpClient, HttpError};
use doiget_core::{DenialContext, DenialReason};
use reqwest::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn redirect_outside_allowlist_surfaces_denial_context_via_source_chain() {
    // Spin up a wiremock origin whose `/redir` 302s to an HTTPS host the
    // `crossref` allowlist does NOT contain. The redirect closure inside
    // the per-source `reqwest::Client` will call
    // `attempt.error(HttpError::RedirectDenied{..})`; reqwest then wraps
    // that into a `reqwest::Error`, which surfaces to us as
    // `HttpError::Network(reqwest_err)`. The fix walks the source chain
    // and downcasts back to `&HttpError`, so
    // `Option::<DenialContext>::from(&err)` MUST produce
    // `Some(DenialContext { reason: RedirectNotInAllowlist, .. })`.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/redir"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", "https://attacker.test/file"),
        )
        .mount(&server)
        .await;
    let host = server
        .uri()
        .parse::<Url>()
        .unwrap()
        .host_str()
        .unwrap()
        .to_string();

    // Allowlist contains only the wiremock origin; the redirect target
    // (`attacker.test`) is OUT.
    let client = HttpClient::new_for_tests_allow_http("crossref", &host);
    let url: Url = format!("{}/redir", server.uri()).parse().unwrap();
    let err = client
        .fetch_bytes("crossref", url)
        .await
        .expect_err("redirect to attacker rejected");

    // The outer error MUST be `Network(_)` — that is precisely what
    // production callers see and the surface where the pre-fix bug hid.
    match &err {
        HttpError::Network(_) => {}
        other => panic!("expected Network(RedirectDenied), got {:?}", other),
    }

    // The C1 fix: source-chain walking surfaces the wrapped HttpError.
    let dc: Option<DenialContext> = (&err).into();
    let dc = dc.expect(
        "Network-wrapped RedirectDenied MUST produce Some(DenialContext) \
         via the source-chain walk (C1 fix). If this is None, From<&HttpError> \
         for Option<DenialContext>'s Network arm regressed.",
    );

    assert_eq!(dc.reason, DenialReason::RedirectNotInAllowlist);
    assert_eq!(
        dc.source.as_deref(),
        Some("crossref"),
        "source key must be carried through the chain (got: {:?})",
        dc.source,
    );
    assert_eq!(
        dc.attempted.as_deref(),
        Some("attacker.test"),
        "attempted host must be the redirect target, not the initial leg \
         (got: {:?})",
        dc.attempted,
    );
    let expected_hosts = dc.expected.as_deref().expect(
        "RedirectNotInAllowlist must populate expected (post-refinement: Some(_), not None)",
    );
    assert!(
        !expected_hosts.is_empty(),
        "expected allowlist hosts must be carried through (the original \
         RedirectDenied snapshots them — see http.rs `expected_hosts` \
         field), got: {:?}",
        expected_hosts,
    );
    assert!(
        expected_hosts.iter().any(|h| h == &host),
        "expected allowlist hosts must include the wiremock host {:?}; got: {:?}",
        host,
        expected_hosts,
    );
}

/// Synthetic abstract-level coverage for the source-chain walk: even
/// without going through `reqwest`, wrapping an `HttpError::RedirectDenied`
/// in a custom `std::error::Error` chain whose `.source()` returns it must
/// surface a `DenialContext` via the same algorithmic walk used by the
/// `Network` arm of `From<&HttpError> for Option<DenialContext>`.
///
/// This pins the *mechanism* of the C1 fix in case reqwest ever changes
/// HOW it wraps custom redirect-policy errors (currently it goes through
/// hyper's error chain). The integration test above covers the production
/// path; this one covers the algorithmic invariant.
#[test]
fn source_chain_walk_recovers_wrapped_redirect_denied() {
    use std::fmt;

    /// Minimal error wrapper whose `source()` returns the wrapped
    /// `HttpError`. Stand-in for reqwest's internal wrapping behaviour.
    #[derive(Debug)]
    struct Wrapper {
        inner: HttpError,
    }
    impl fmt::Display for Wrapper {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "wrapped: {}", self.inner)
        }
    }
    impl std::error::Error for Wrapper {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.inner)
        }
    }

    // Construct the same shape the production redirect closure produces.
    let denied = HttpError::RedirectDenied {
        source_key: "crossref".to_string(),
        host: "evil.example.com".to_string(),
        expected_hosts: vec!["api.crossref.org".to_string(), "*.crossref.org".to_string()],
    };

    // Walk the chain manually using the same algorithm as the Network
    // arm of `From<&HttpError> for Option<DenialContext>`. If this
    // produces the expected `DenialContext`, the fix's algorithm is
    // sound regardless of reqwest's exact wrapping topology.
    let wrapper = Wrapper { inner: denied };
    let mut source: Option<&(dyn std::error::Error + 'static)> =
        std::error::Error::source(&wrapper);
    let mut found: Option<&HttpError> = None;
    while let Some(s) = source {
        if let Some(http_err) = s.downcast_ref::<HttpError>() {
            found = Some(http_err);
            break;
        }
        source = s.source();
    }
    let http_err = found.expect("source-chain walk must surface the wrapped HttpError");
    let dc: Option<DenialContext> = http_err.into();
    let dc = dc.expect("RedirectDenied -> Some(DenialContext)");
    assert_eq!(dc.reason, DenialReason::RedirectNotInAllowlist);
    assert_eq!(dc.source.as_deref(), Some("crossref"));
    assert_eq!(dc.attempted.as_deref(), Some("evil.example.com"));
    assert_eq!(
        dc.expected,
        Some(vec![
            "api.crossref.org".to_string(),
            "*.crossref.org".to_string(),
        ]),
    );
}
