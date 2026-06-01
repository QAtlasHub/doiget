// allow: outbound-network
//! End-to-end wiremock-driven test for the arXiv Atom-feed metadata
//! parsing (Slice 1 deliverable B.2).
//!
//! The `allow: outbound-network` escape hatch is set because this file
//! imports `wiremock` (which transitively touches the same crates the
//! posture-lint job greps for in unit-test sources). All HTTP traffic
//! terminates at a `wiremock::MockServer` on `127.0.0.1:N` — there is
//! NO real network call, mirroring the
//! `crates/doiget-cli/tests/fetch_arxiv_e2e.rs` convention.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use camino::Utf8PathBuf;
use doiget_core::http::HttpClient;
use doiget_core::provenance::ProvenanceLog;
use doiget_core::rate_limiter::RateLimiter;
use doiget_core::source::{FetchContext, Source};
use doiget_core::sources::arxiv::ArxivSource;
use doiget_core::{ArxivId, CapabilityProfile, RateLimits, Ref};
use tempfile::TempDir;
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Synthetic Atom payload from Slice 1 spec §B.3.
const SAMPLE_ATOM_FEED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <entry>
    <id>http://arxiv.org/abs/2401.12345v1</id>
    <updated>2024-02-01T00:00:00Z</updated>
    <published>2024-01-15T00:00:00Z</published>
    <title>Example arXiv Paper Title</title>
    <summary>This is an example abstract.</summary>
    <author><name>Jane Doe</name></author>
    <author><name>John Roe</name></author>
    <category term="cs.LG" scheme="http://arxiv.org/schemas/atom"/>
    <category term="stat.ML" scheme="http://arxiv.org/schemas/atom"/>
  </entry>
</feed>"#;

const TEST_SESSION_ID: &str = "01J0000000000000000000TEST";

/// Build a `FetchContext` against a wiremock origin under the `arxiv`
/// source key. Mirrors the per-source helpers in the in-crate unit
/// tests.
fn build_ctx(host: &str) -> (TempDir, FetchContext) {
    let td = TempDir::new().expect("tempdir");
    let log_dir =
        Utf8PathBuf::try_from(td.path().to_path_buf()).expect("temp dir path must be UTF-8");
    let log_path = log_dir.join("arxiv-meta.jsonl");
    let http = Arc::new(HttpClient::new_for_tests_allow_http("arxiv", host));
    let rate_limiter = Arc::new(RateLimiter::new(RateLimits::HARD_CODED));
    let session_id = TEST_SESSION_ID.to_string();
    let log =
        Arc::new(ProvenanceLog::open(log_path, session_id.clone()).expect("provenance log opens"));
    (
        td,
        FetchContext {
            http,
            rate_limiter,
            log,
            session_id,
            cache_root: None,
        },
    )
}

#[tokio::test]
async fn arxiv_source_fetch_populates_atom_metadata_via_wiremock() {
    // Both the Atom and PDF endpoints are mocked on the same wiremock
    // origin. After Slice 1, `Source::fetch` makes a best-effort Atom
    // call before the PDF, populating `FetchResult::metadata_json`.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/query"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_ATOM_FEED))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/pdf/2401.12345.pdf"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"%PDF-1.7\n%fix\n".to_vec()))
        .mount(&server)
        .await;

    let host = server
        .uri()
        .parse::<Url>()
        .unwrap()
        .host_str()
        .unwrap()
        .to_string();
    let (_td, ctx) = build_ctx(&host);
    let s = ArxivSource::with_base(server.uri().parse().unwrap());
    let id = ArxivId::parse("2401.12345").unwrap();
    let r = Ref::Arxiv(id);
    let profile = CapabilityProfile::from_env().expect("clean env");

    let res = s.fetch(&r, &profile, &ctx).await.expect("fetch ok");
    let meta = res
        .metadata_json
        .expect("metadata_json populated from Atom feed");
    assert_eq!(
        meta["title"],
        serde_json::json!("Example arXiv Paper Title")
    );
    assert_eq!(
        meta["abstract"],
        serde_json::json!("This is an example abstract.")
    );
    assert_eq!(meta["authors"], serde_json::json!(["Jane Doe", "John Roe"]));
    assert_eq!(meta["published"], serde_json::json!("2024-01-15T00:00:00Z"));
    assert_eq!(meta["updated"], serde_json::json!("2024-02-01T00:00:00Z"));
    assert_eq!(meta["categories"], serde_json::json!(["cs.LG", "stat.ML"]));
}

#[tokio::test]
async fn arxiv_source_fetch_metadata_only_via_wiremock() {
    // Atom endpoint only — `fetch_metadata_only` MUST NOT touch the
    // PDF endpoint (no mount for `/pdf/...`).
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/query"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_ATOM_FEED))
        .mount(&server)
        .await;

    let host = server
        .uri()
        .parse::<Url>()
        .unwrap()
        .host_str()
        .unwrap()
        .to_string();
    let (_td, ctx) = build_ctx(&host);
    let s = ArxivSource::with_base(server.uri().parse().unwrap());
    let id = ArxivId::parse("2401.12345").unwrap();

    let meta = s
        .fetch_metadata_only(&id, &ctx)
        .await
        .expect("metadata_only ok");
    assert_eq!(
        meta["title"],
        serde_json::json!("Example arXiv Paper Title")
    );
    assert_eq!(meta["authors"], serde_json::json!(["Jane Doe", "John Roe"]));
}
