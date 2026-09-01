//! Slice 2 — end-to-end coverage for `doiget_fetch_paper` and
//! `doiget_batch_fetch` (wiremock-driven; NO outbound network).
//!
//! ## Network purity
//!
//! Per the workspace network-purity guard, this file imports `wiremock`
//! to mount fake origins; no `reqwest::*` items are imported directly.
//! All HTTP traffic terminates at a `wiremock::MockServer` on
//! `127.0.0.1:N`. The first-line escape hatch below covers the future
//! addition of any `reqwest::*` import without further intervention.
// allow: outbound-network

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use doiget_core::CapabilityProfile;
use doiget_mcp::Server;
use rmcp::{model::CallToolRequestParams, ServiceExt};

/// RAII helper mirroring `initialize_handshake::EnvGuard`. Scoped env
/// mutations so a panic mid-test does not leak state across the
/// single-threaded `serial_test::serial` group.
struct EnvGuard {
    keys: Vec<&'static str>,
}

impl EnvGuard {
    fn new(keys: &[&'static str]) -> Self {
        for k in keys {
            std::env::remove_var(k);
        }
        Self {
            keys: keys.to_vec(),
        }
    }
    fn set(&self, key: &str, val: &str) {
        std::env::set_var(key, val);
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for k in &self.keys {
            std::env::remove_var(k);
        }
    }
}

const ENV_KEYS: &[&str] = &[
    "DOIGET_STORE_ROOT",
    "DOIGET_LOG_PATH",
    "DOIGET_ARXIV_BASE",
    "DOIGET_CROSSREF_BASE",
    "DOIGET_UNPAYWALL_BASE",
    "DOIGET_OA_PUBLISHER_BASE",
    "DOIGET_CONTACT_EMAIL",
    "DOIGET_UNPAYWALL_EMAIL",
    // #462: the Tier-3 route. Cleared for every test so an APS grant can
    // never leak from one into another.
    "DOIGET_APS_BASE",
    "DOIGET_KEY_APS",
    "DOIGET_AGREE_TDM_APS",
];

async fn boot_in_memory_server() -> anyhow::Result<(
    rmcp::service::RunningService<rmcp::RoleClient, ()>,
    tokio::task::JoinHandle<anyhow::Result<()>>,
)> {
    let profile = CapabilityProfile::from_env().expect("clean env never errors");
    let server = Server::new(profile);
    let (server_transport, client_transport) = tokio::io::duplex(64 * 1024);
    let server_handle = tokio::spawn(async move {
        let service = server.serve(server_transport).await?;
        service.waiting().await?;
        anyhow::Ok(())
    });
    let client = ().serve(client_transport).await?;
    Ok((client, server_handle))
}

// ---------------------------------------------------------------------------
// doiget_fetch_paper
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial_test::serial]
async fn fetch_paper_invalid_ref_returns_invalid_ref_envelope() -> anyhow::Result<()> {
    let (client, server_handle) = boot_in_memory_server().await?;

    let mut args = serde_json::Map::new();
    args.insert("ref".to_string(), serde_json::json!("not a doi"));

    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_fetch_paper").with_arguments(args))
        .await?;
    let structured = result
        .structured_content
        .as_ref()
        .expect("doiget_fetch_paper uses CallToolResult::structured");
    assert_eq!(structured["ok"], serde_json::json!(false));
    assert_eq!(
        structured["error"]["code"],
        serde_json::json!("INVALID_REF"),
        "envelope: {structured:?}"
    );

    client.cancel().await?;
    server_handle.await??;
    Ok(())
}

#[tokio::test]
#[serial_test::serial]
async fn fetch_paper_dry_run_returns_fetch_plan_envelope() -> anyhow::Result<()> {
    let (client, server_handle) = boot_in_memory_server().await?;

    let mut args = serde_json::Map::new();
    args.insert("ref".to_string(), serde_json::json!("10.1234/example"));
    args.insert("dry_run".to_string(), serde_json::json!(true));

    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_fetch_paper").with_arguments(args))
        .await?;
    let structured = result
        .structured_content
        .as_ref()
        .expect("dry_run uses structured content");
    assert_eq!(structured["ok"], serde_json::json!(true));
    assert_eq!(structured["dry_run"], serde_json::json!(true));
    assert_eq!(
        structured["ref"],
        serde_json::json!({"doi": "10.1234/example"})
    );
    // ADR-0022 §4 marker: the candidate_hosts list is an upper bound,
    // not a prediction. Posture surfaces this as a machine-parseable
    // boolean inside `plan`.
    assert_eq!(
        structured["plan"]["candidate_hosts_are_upper_bound"],
        serde_json::json!(true)
    );

    client.cancel().await?;
    server_handle.await??;
    Ok(())
}

const SAMPLE_PDF_BODY: &[u8] = b"%PDF-fake-bytes\n";

#[tokio::test]
#[serial_test::serial]
async fn fetch_paper_arxiv_happy_path_writes_pdf_and_returns_envelope() -> anyhow::Result<()> {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/pdf/2401.12345.pdf"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(SAMPLE_PDF_BODY.to_vec()))
        .mount(&mock)
        .await;

    let td = tempfile::TempDir::new().expect("tempdir");
    let temp_root = camino::Utf8Path::from_path(td.path())
        .expect("tempdir is utf-8")
        .to_path_buf();
    let store_root = temp_root.join("papers");
    let log_path = temp_root.join("log.jsonl");

    let env = EnvGuard::new(ENV_KEYS);
    env.set("DOIGET_STORE_ROOT", store_root.as_str());
    env.set("DOIGET_LOG_PATH", log_path.as_str());
    env.set("DOIGET_ARXIV_BASE", &mock.uri());

    let (client, server_handle) = boot_in_memory_server().await?;

    let mut args = serde_json::Map::new();
    args.insert("ref".to_string(), serde_json::json!("2401.12345"));

    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_fetch_paper").with_arguments(args))
        .await?;
    let structured = result
        .structured_content
        .as_ref()
        .expect("structured content");
    assert_eq!(
        structured["ok"],
        serde_json::json!(true),
        "envelope: {structured:?}"
    );
    // #462: WHICH ROUTE produced this, not merely that something did.
    // Four "unreachable source" bugs shipped with green unit tests
    // because nothing asserted the route, and a measurement over this
    // suite found only one of the five `PdfLegStatus` routes asserted
    // anywhere. See `route_coverage_e2e.rs`.
    assert_eq!(
        structured["pdf"]["status"],
        serde_json::json!("fetched"),
        "the arXiv happy path must report the `fetched` route: {structured:?}"
    );
    assert_eq!(structured["source"], serde_json::json!("arxiv"));
    assert_eq!(structured["ref"], serde_json::json!("2401.12345"));
    assert_eq!(structured["license"], serde_json::json!("arxiv-default"));
    // OA transparency (#281 item 4): arXiv is green OA.
    assert_eq!(structured["oa_status"], serde_json::json!("green"));
    assert_eq!(
        structured["size_bytes"],
        serde_json::json!(SAMPLE_PDF_BODY.len())
    );
    assert_eq!(structured["schema_version"], serde_json::json!("1.0"));
    // On-disk PDF MUST exist at the path the envelope advertises.
    let pdf_path = structured["path"].as_str().expect("path field is a string");
    let on_disk = std::path::Path::new(pdf_path);
    assert!(
        on_disk.exists(),
        "PDF written by orchestrator must exist on disk: {pdf_path}"
    );
    let bytes = std::fs::read(on_disk).expect("read PDF");
    assert_eq!(bytes, SAMPLE_PDF_BODY);

    // #344 (Slice 1): identity fields are surfaced on the success envelope so
    // an agent can confirm the RIGHT paper in one call (no follow-up
    // doiget_info). Values depend on the resolver's metadata (no Atom mock
    // here), so assert the keys are present and well-typed.
    assert!(
        structured.get("title").is_some(),
        "title key present: {structured:?}"
    );
    assert!(
        structured["authors"].is_array(),
        "authors is an array: {structured:?}"
    );
    assert!(
        structured.get("year").is_some(),
        "year key present (may be null): {structured:?}"
    );

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    drop(td);
    Ok(())
}

// ---------------------------------------------------------------------------
// doiget_batch_fetch
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial_test::serial]
async fn batch_fetch_too_many_refs_returns_invalid_ref_envelope() -> anyhow::Result<()> {
    let (client, server_handle) = boot_in_memory_server().await?;

    // 101 refs (one over the MAX_BATCH_REFS cap of 100).
    let refs: Vec<serde_json::Value> = (0..101)
        .map(|i| serde_json::Value::String(format!("10.1234/n{}", i)))
        .collect();
    let mut args = serde_json::Map::new();
    args.insert("refs".to_string(), serde_json::Value::Array(refs));

    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_batch_fetch").with_arguments(args))
        .await?;
    let structured = result
        .structured_content
        .as_ref()
        .expect("structured content");
    assert_eq!(structured["ok"], serde_json::json!(false));
    assert_eq!(
        structured["error"]["code"],
        serde_json::json!("INVALID_REF")
    );
    let message = structured["error"]["message"]
        .as_str()
        .expect("message is string");
    assert!(
        message.contains("too many refs"),
        "TOO_MANY_REFS message must surface the cap; got: {message}"
    );

    client.cancel().await?;
    server_handle.await??;
    Ok(())
}

#[tokio::test]
#[serial_test::serial]
async fn batch_fetch_one_invalid_ref_aborts_with_invalid_ref_envelope() -> anyhow::Result<()> {
    let (client, server_handle) = boot_in_memory_server().await?;

    let refs = serde_json::json!(["2401.12345", "not-a-doi-at-all"]);
    let mut args = serde_json::Map::new();
    args.insert("refs".to_string(), refs);

    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_batch_fetch").with_arguments(args))
        .await?;
    let structured = result
        .structured_content
        .as_ref()
        .expect("structured content");
    assert_eq!(structured["ok"], serde_json::json!(false));
    assert_eq!(
        structured["error"]["code"],
        serde_json::json!("INVALID_REF")
    );

    client.cancel().await?;
    server_handle.await??;
    Ok(())
}

#[tokio::test]
#[serial_test::serial]
async fn batch_fetch_dry_run_returns_plans_array() -> anyhow::Result<()> {
    let (client, server_handle) = boot_in_memory_server().await?;

    let refs = serde_json::json!(["2401.12345", "10.1234/foo"]);
    let mut args = serde_json::Map::new();
    args.insert("refs".to_string(), refs);
    args.insert("dry_run".to_string(), serde_json::json!(true));

    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_batch_fetch").with_arguments(args))
        .await?;
    let structured = result
        .structured_content
        .as_ref()
        .expect("structured content");
    assert_eq!(structured["ok"], serde_json::json!(true));
    assert_eq!(structured["dry_run"], serde_json::json!(true));
    let plans = structured["plans"].as_array().expect("plans is an array");
    assert_eq!(plans.len(), 2);
    assert_eq!(plans[0]["ref"], serde_json::json!({"arxiv": "2401.12345"}));
    assert_eq!(plans[1]["ref"], serde_json::json!({"doi": "10.1234/foo"}));
    // ADR-0022 §4 marker propagates into each per-ref plan.
    assert_eq!(
        plans[0]["plan"]["candidate_hosts_are_upper_bound"],
        serde_json::json!(true)
    );
    // Rate-limit budget present per row.
    assert_eq!(
        plans[0]["rate_limit_budget"]["global_per_sec"],
        serde_json::json!(5.0)
    );

    client.cancel().await?;
    server_handle.await??;
    Ok(())
}

#[tokio::test]
#[serial_test::serial]
async fn batch_fetch_three_arxiv_refs_succeed_each_with_ok_true() -> anyhow::Result<()> {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock = MockServer::start().await;
    for id in ["2401.10001", "2401.10002", "2401.10003"] {
        Mock::given(method("GET"))
            .and(path(format!("/pdf/{}.pdf", id)))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(SAMPLE_PDF_BODY.to_vec()))
            .mount(&mock)
            .await;
    }

    let td = tempfile::TempDir::new().expect("tempdir");
    let temp_root = camino::Utf8Path::from_path(td.path())
        .expect("tempdir is utf-8")
        .to_path_buf();
    let store_root = temp_root.join("papers");
    let log_path = temp_root.join("log.jsonl");

    let env = EnvGuard::new(ENV_KEYS);
    env.set("DOIGET_STORE_ROOT", store_root.as_str());
    env.set("DOIGET_LOG_PATH", log_path.as_str());
    env.set("DOIGET_ARXIV_BASE", &mock.uri());

    let (client, server_handle) = boot_in_memory_server().await?;
    let refs = serde_json::json!(["2401.10001", "2401.10002", "2401.10003"]);
    let mut args = serde_json::Map::new();
    args.insert("refs".to_string(), refs);

    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_batch_fetch").with_arguments(args))
        .await?;
    let structured = result
        .structured_content
        .as_ref()
        .expect("structured content");
    assert_eq!(
        structured["ok"],
        serde_json::json!(true),
        "envelope: {structured:?}"
    );
    let results = structured["results"]
        .as_array()
        .expect("results is an array");
    assert_eq!(results.len(), 3);
    for (i, entry) in results.iter().enumerate() {
        assert_eq!(
            entry["ok"],
            serde_json::json!(true),
            "row {i} must report ok:true; entry: {entry:?}"
        );
        assert_eq!(entry["source"], serde_json::json!("arxiv"));
        assert_eq!(
            entry["size_bytes"],
            serde_json::json!(SAMPLE_PDF_BODY.len())
        );
    }

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    drop(td);
    Ok(())
}

#[tokio::test]
#[serial_test::serial]
async fn batch_fetch_partial_failure_emits_per_ref_outcomes() -> anyhow::Result<()> {
    // Two refs that should succeed, plus one that points at an id with
    // no mounted mock — the PDF leg returns 404 and the orchestrator
    // surfaces a per-ref `Err`. The whole-call envelope MUST remain
    // `ok:true` (per-ref errors are independent).
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock = MockServer::start().await;
    for id in ["2401.20001", "2401.20002"] {
        Mock::given(method("GET"))
            .and(path(format!("/pdf/{}.pdf", id)))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(SAMPLE_PDF_BODY.to_vec()))
            .mount(&mock)
            .await;
    }
    // No mock mounted for `/pdf/2401.99999.pdf` — wiremock returns 404
    // by default, which the orchestrator surfaces as a NETWORK_ERROR.

    let td = tempfile::TempDir::new().expect("tempdir");
    let temp_root = camino::Utf8Path::from_path(td.path())
        .expect("tempdir is utf-8")
        .to_path_buf();
    let store_root = temp_root.join("papers");
    let log_path = temp_root.join("log.jsonl");

    let env = EnvGuard::new(ENV_KEYS);
    env.set("DOIGET_STORE_ROOT", store_root.as_str());
    env.set("DOIGET_LOG_PATH", log_path.as_str());
    env.set("DOIGET_ARXIV_BASE", &mock.uri());

    let (client, server_handle) = boot_in_memory_server().await?;
    let refs = serde_json::json!(["2401.20001", "2401.99999", "2401.20002"]);
    let mut args = serde_json::Map::new();
    args.insert("refs".to_string(), refs);

    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_batch_fetch").with_arguments(args))
        .await?;
    let structured = result
        .structured_content
        .as_ref()
        .expect("structured content");
    // Whole-call still ok=true; per-ref errors live inside results[].
    assert_eq!(
        structured["ok"],
        serde_json::json!(true),
        "envelope: {structured:?}"
    );
    let results = structured["results"]
        .as_array()
        .expect("results is an array");
    assert_eq!(results.len(), 3);
    assert_eq!(results[0]["ok"], serde_json::json!(true));
    assert_eq!(results[1]["ok"], serde_json::json!(false));
    assert_eq!(results[2]["ok"], serde_json::json!(true));
    // The failing per-ref row carries `denial_context: null` for
    // transport errors (ADR-0023 §4 — NETWORK_ERROR has no denial
    // channel).
    assert!(
        results[1]["error"].get("denial_context").is_some(),
        "transport per-ref error must surface denial_context (as null) per Slice 2 spec; got: {:?}",
        results[1],
    );
    // #506: this envelope is one of the five assembled by hand, and review
    // found `disposition` was inserted at six sites and asserted at two --
    // deleting this one's insert failed no test. The error object is already
    // in hand here, so the assertion costs nothing.
    assert!(
        results[1]["error"]["disposition"].is_string(),
        "every failure envelope carries a disposition, including this one: {:?}",
        results[1]["error"]
    );
    assert!(
        results[1]["error"]["denial_context"].is_null(),
        "denial_context must be null for NETWORK_ERROR; got: {:?}",
        results[1]["error"]["denial_context"]
    );

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    drop(td);
    Ok(())
}

// ---------------------------------------------------------------------------
// suggested_arxiv_id — issue #243
// ---------------------------------------------------------------------------

/// When all OA-chain PDF candidates fail and at least one candidate URL is
/// hosted on arxiv.org, `doiget_fetch_paper` MUST include `suggested_arxiv_id`
/// in the `pdf` object of the success envelope (the PDF leg is `blocked`).
/// The version suffix (e.g. `v2`) must be stripped so the suggestion points
/// to the latest version rather than a pinned one.
#[tokio::test]
#[serial_test::serial]
async fn fetch_paper_doi_blocked_pdf_includes_suggested_arxiv_id() -> anyhow::Result<()> {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;

    // Crossref metadata — minimal envelope.
    // Crossref uses `Url::join("/works/<doi>")` which does NOT percent-encode
    // the `/` inside the DOI suffix, so wiremock matches the raw path.
    Mock::given(method("GET"))
        .and(path("/works/10.1234/suggest-test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "ok",
            "message": {
                "title": ["Suggestion Test Paper"],
                "author": [{"family": "Doe", "given": "Jane"}],
                "issued": {"date-parts": [[2024, 1, 1]]}
            }
        })))
        .mount(&server)
        .await;

    // Unpaywall metadata — `best_oa_location` points to a versioned arXiv URL.
    // The arXiv host is off the `oa-publisher` allowlist (which only permits
    // the wiremock host), so the PDF leg will be denied at the pre-fetch
    // allowlist check, triggering PdfLegStatus::Blocked with a suggestion.
    // Unpaywall uses `path_segments_mut().push()` which percent-encodes `/`.
    Mock::given(method("GET"))
        .and(path("/v2/10.1234%2Fsuggest-test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "doi": "10.1234/suggest-test",
            "is_oa": true,
            // `is_oa:true` + `oa_status:"closed"` is deliberately
            // contradictory (real Unpaywall never pairs these); the
            // orchestrator does not cross-validate the two, so this isolates
            // pure oa_status passthrough onto the fetch envelope.
            "oa_status": "closed",
            "best_oa_location": {
                "url_for_pdf": "https://arxiv.org/pdf/2401.99999v2.pdf",
                "url": "https://arxiv.org/abs/2401.99999v2",
                "license": "cc-by"
            },
            "oa_locations": [
                {
                    "url_for_pdf": "https://arxiv.org/pdf/2401.99999v2.pdf",
                    "url": "https://arxiv.org/abs/2401.99999v2"
                }
            ]
        })))
        .mount(&server)
        .await;

    let td = tempfile::TempDir::new().expect("tempdir");
    let temp_root = camino::Utf8Path::from_path(td.path())
        .expect("tempdir is utf-8")
        .to_path_buf();
    let store_root = temp_root.join("papers");
    let log_path = temp_root.join("log.jsonl");

    let env = EnvGuard::new(ENV_KEYS);
    env.set("DOIGET_STORE_ROOT", store_root.as_str());
    env.set("DOIGET_LOG_PATH", log_path.as_str());
    env.set("DOIGET_CROSSREF_BASE", &server.uri());
    env.set("DOIGET_UNPAYWALL_BASE", &format!("{}/v2", server.uri()));
    // Register only the wiremock host for oa-publisher. arxiv.org is absent
    // so the arXiv OA candidate is denied → PdfLegStatus::Blocked.
    env.set("DOIGET_OA_PUBLISHER_BASE", &server.uri());

    let (client, server_handle) = boot_in_memory_server().await?;

    let mut args = serde_json::Map::new();
    args.insert("ref".to_string(), serde_json::json!("10.1234/suggest-test"));

    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_fetch_paper").with_arguments(args))
        .await?;
    let structured = result
        .structured_content
        .as_ref()
        .expect("doiget_fetch_paper uses CallToolResult::structured");

    // Metadata fetch succeeds (ok:true) but PDF leg is blocked.
    assert_eq!(
        structured["ok"],
        serde_json::json!(true),
        "envelope should be ok:true (metadata was written); got: {structured:?}"
    );
    assert_eq!(
        structured["pdf"]["status"],
        serde_json::json!("blocked"),
        "pdf leg must be blocked; got: {:?}",
        structured["pdf"]
    );
    // Version suffix `v2` must be stripped — suggestion points to latest version.
    assert_eq!(
        structured["pdf"]["suggested_arxiv_id"],
        serde_json::json!("2401.99999"),
        "suggested_arxiv_id must be present and version-stripped; got: {:?}",
        structured["pdf"]["suggested_arxiv_id"]
    );
    // OA transparency (#281 item 4): the work's oa_status (from Unpaywall)
    // is surfaced even though the PDF leg was blocked — `closed` + a
    // blocked/no-OA leg reads as "paywalled", distinct from a transient
    // failure. This is the headline DOI oa_status path (review #284).
    assert_eq!(
        structured["oa_status"],
        serde_json::json!("closed"),
        "oa_status must surface on the DOI fetch envelope; got: {structured:?}"
    );

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    drop(td);
    Ok(())
}

#[tokio::test]
#[serial_test::serial]
async fn fetch_paper_doi_falls_back_to_the_arxiv_preprint() -> anyhow::Result<()> {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;

    // Crossref metadata — minimal envelope.
    // Crossref uses `Url::join("/works/<doi>")` which does NOT percent-encode
    // the `/` inside the DOI suffix, so wiremock matches the raw path.
    Mock::given(method("GET"))
        .and(path("/works/10.1234/suggest-test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "ok",
            "message": {
                "title": ["Suggestion Test Paper"],
                "author": [{"family": "Doe", "given": "Jane"}],
                "issued": {"date-parts": [[2024, 1, 1]]}
            }
        })))
        .mount(&server)
        .await;

    // Unpaywall metadata — `best_oa_location` points to a versioned arXiv URL.
    // The arXiv host is off the `oa-publisher` allowlist (which only permits
    // the wiremock host), so the PDF leg will be denied at the pre-fetch
    // allowlist check, triggering PdfLegStatus::Blocked with a suggestion.
    // Unpaywall uses `path_segments_mut().push()` which percent-encodes `/`.
    Mock::given(method("GET"))
        .and(path("/v2/10.1234%2Fsuggest-test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "doi": "10.1234/suggest-test",
            "is_oa": true,
            // `is_oa:true` + `oa_status:"closed"` is deliberately
            // contradictory (real Unpaywall never pairs these); the
            // orchestrator does not cross-validate the two, so this isolates
            // pure oa_status passthrough onto the fetch envelope.
            "oa_status": "closed",
            "best_oa_location": {
                "url_for_pdf": "https://arxiv.org/pdf/2401.99999v2.pdf",
                "url": "https://arxiv.org/abs/2401.99999v2",
                "license": "cc-by"
            },
            "oa_locations": [
                {
                    "url_for_pdf": "https://arxiv.org/pdf/2401.99999v2.pdf",
                    "url": "https://arxiv.org/abs/2401.99999v2"
                }
            ]
        })))
        .mount(&server)
        .await;

    // The preprint the suggestion points at, actually served.
    let arxiv = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(SAMPLE_PDF_BODY.to_vec()))
        .mount(&arxiv)
        .await;

    let td = tempfile::TempDir::new().expect("tempdir");
    let temp_root = camino::Utf8Path::from_path(td.path())
        .expect("tempdir is utf-8")
        .to_path_buf();
    let store_root = temp_root.join("papers");
    let log_path = temp_root.join("log.jsonl");

    let env = EnvGuard::new(ENV_KEYS);
    env.set("DOIGET_STORE_ROOT", store_root.as_str());
    env.set("DOIGET_LOG_PATH", log_path.as_str());
    env.set("DOIGET_CROSSREF_BASE", &server.uri());
    env.set("DOIGET_UNPAYWALL_BASE", &format!("{}/v2", server.uri()));
    // Register only the wiremock host for oa-publisher. arxiv.org is absent
    // so the arXiv OA candidate is denied → PdfLegStatus::Blocked.
    env.set("DOIGET_OA_PUBLISHER_BASE", &server.uri());
    // The one difference from the blocked test: the #325 fallback fetches
    // through the arXiv SOURCE, not oa-publisher, so it needs its own base.
    env.set("DOIGET_ARXIV_BASE", &arxiv.uri());

    let (client, server_handle) = boot_in_memory_server().await?;

    let mut args = serde_json::Map::new();
    args.insert("ref".to_string(), serde_json::json!("10.1234/suggest-test"));

    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_fetch_paper").with_arguments(args))
        .await?;
    let structured = result
        .structured_content
        .as_ref()
        .expect("doiget_fetch_paper uses CallToolResult::structured");

    // Metadata fetch succeeds (ok:true) but PDF leg is blocked.
    assert_eq!(
        structured["ok"],
        serde_json::json!(true),
        "envelope should be ok:true (metadata was written); got: {structured:?}"
    );
    assert_eq!(
        structured["pdf"]["status"],
        serde_json::json!("preprint_fallback"),
        "#462: the SUGGESTION and the FALLBACK are different routes, one field          apart in the envelope, and only the first had ever been asserted:          {structured:?}"
    );
    assert_eq!(
        structured["source"],
        serde_json::json!("arxiv"),
        "the bytes came from arXiv, and `source` has to say so: {structured:?}"
    );
    assert!(
        structured["size_bytes"].as_u64().unwrap_or(0) > 0,
        "a fallback that reports success must have written bytes: {structured:?}"
    );

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    drop(td);
    Ok(())
}

#[tokio::test]
#[serial_test::serial]
async fn fetch_paper_doi_with_no_oa_anywhere_reports_the_no_oa_url_route() -> anyhow::Result<()> {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;

    // Crossref metadata — minimal envelope.
    // Crossref uses `Url::join("/works/<doi>")` which does NOT percent-encode
    // the `/` inside the DOI suffix, so wiremock matches the raw path.
    Mock::given(method("GET"))
        .and(path("/works/10.1234/suggest-test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "ok",
            "message": {
                "title": ["Suggestion Test Paper"],
                "author": [{"family": "Doe", "given": "Jane"}],
                "issued": {"date-parts": [[2024, 1, 1]]}
            }
        })))
        .mount(&server)
        .await;

    // Unpaywall metadata — `best_oa_location` points to a versioned arXiv URL.
    // The arXiv host is off the `oa-publisher` allowlist (which only permits
    // the wiremock host), so the PDF leg will be denied at the pre-fetch
    // allowlist check, triggering PdfLegStatus::Blocked with a suggestion.
    // Unpaywall uses `path_segments_mut().push()` which percent-encodes `/`.
    Mock::given(method("GET"))
        .and(path("/v2/10.1234%2Fsuggest-test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "doi": "10.1234/suggest-test",
            "is_oa": false,
            "oa_status": "closed",
            // No `best_oa_location` and no `oa_locations`: Unpaywall knows the
            // work and has nothing free for it.
            "best_oa_location": serde_json::Value::Null,
            "oa_locations": []
        })))
        .mount(&server)
        .await;

    let td = tempfile::TempDir::new().expect("tempdir");
    let temp_root = camino::Utf8Path::from_path(td.path())
        .expect("tempdir is utf-8")
        .to_path_buf();
    let store_root = temp_root.join("papers");
    let log_path = temp_root.join("log.jsonl");

    let env = EnvGuard::new(ENV_KEYS);
    env.set("DOIGET_STORE_ROOT", store_root.as_str());
    env.set("DOIGET_LOG_PATH", log_path.as_str());
    env.set("DOIGET_CROSSREF_BASE", &server.uri());
    env.set("DOIGET_UNPAYWALL_BASE", &format!("{}/v2", server.uri()));
    // Register only the wiremock host for oa-publisher. arxiv.org is absent
    // so the arXiv OA candidate is denied → PdfLegStatus::Blocked.
    env.set("DOIGET_OA_PUBLISHER_BASE", &server.uri());

    let (client, server_handle) = boot_in_memory_server().await?;

    let mut args = serde_json::Map::new();
    args.insert("ref".to_string(), serde_json::json!("10.1234/suggest-test"));

    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_fetch_paper").with_arguments(args))
        .await?;
    let structured = result
        .structured_content
        .as_ref()
        .expect("doiget_fetch_paper uses CallToolResult::structured");

    // Metadata fetch succeeds (ok:true) but PDF leg is blocked.
    assert_eq!(
        structured["ok"],
        serde_json::json!(true),
        "envelope should be ok:true (metadata was written); got: {structured:?}"
    );
    assert_eq!(
        structured["pdf"]["status"],
        serde_json::json!("no_oa_url"),
        "#462: nowhere to fetch FROM is a different route than being refused          AT somewhere, and this one had no assertion anywhere: {structured:?}"
    );
    assert_eq!(
        structured["ok"],
        serde_json::json!(true),
        "metadata-only is a success, not a failure: {structured:?}"
    );
    assert_eq!(
        structured["oa_status"],
        serde_json::json!("closed"),
        "and the envelope says WHY there was nowhere to go: {structured:?}"
    );

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    drop(td);
    Ok(())
}

/// The Tier-3 TDM-fetched route, asserted end to end over MCP.
///
/// Written as an `#[ignore]`d reproduction first, because it failed with:
///
/// ```text
/// "detail": "network error: no allowlist registered for source tdm-aps"
/// ```
///
/// -- `HttpError::UnknownSource`, the source key absent from the client's map.
/// That was diagnosed as "`tier_3_allowlists()` is `#[cfg]`-gated and the MCP
/// server does extend its allowlists with it, so the two disagree somewhere
/// between construction and use", i.e. #454's shape reachable again, and it was
/// raised as something to decide before cutting a release.
///
/// The diagnosis was wrong, and wrong in this file's own subject matter: a
/// statement accurate about the code and false about the world. Both client
/// builders have two branches. The production branch does extend with
/// `tier_3_allowlists()` and was correct throughout. The test-override branch
/// -- taken whenever ANY `DOIGET_*_BASE` is set, which every wiremock test
/// does -- built its allowlist from a fixed table of Tier-1/2 keys with no
/// Tier-3 entry, so no e2e on either surface could reach this route. The
/// defect was in the harness. Registering the Tier-3 keys there is what this
/// test now proves, by passing.
///
/// What survives from that diagnosis, and is still true: `fetch_content` is
/// implemented by APS alone. Elsevier, Springer and IEEE inherit the default
/// `Ok(None)` and are metadata-only, so three of the four Tier-3 sources
/// cannot reach the route the tier exists for. Read this as APS coverage, not
/// as Tier-3 coverage.
///
/// #462: the Tier-3 route, which had no assertion anywhere -- which is how
/// #458, "the Tier-3 chain is skipped whenever Crossref answers", shipped.
#[cfg(feature = "tdm-aps")]
#[tokio::test]
#[serial_test::serial]
async fn fetch_paper_doi_served_by_the_publisher_reports_the_tdm_fetched_route(
) -> anyhow::Result<()> {
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const APS_DOI: &str = "10.1103/PhysRevX.10.011001";
    const KEY: &str = "test-aps-key";

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/works/{APS_DOI}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "ok",
            "message": { "title": ["An APS article"], "DOI": APS_DOI }
        })))
        .mount(&server)
        .await;
    // An OA location that the allowlist refuses, so the CONTENT leg is blocked
    // -- which is the trigger #458 gave the Tier-3 chain.
    Mock::given(method("GET"))
        .and(path("/v2/10.1103%2FPhysRevX.10.011001"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "doi": APS_DOI,
            "is_oa": true,
            "oa_status": "closed",
            "best_oa_location": { "url_for_pdf": "https://not-allowlisted.example/x.pdf" }
        })))
        .mount(&server)
        .await;

    // The publisher's own copy, under the agreement.
    let aps = MockServer::start().await;
    Mock::given(method("GET"))
        .and(header("x-api-key", KEY))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(SAMPLE_PDF_BODY.to_vec()))
        .mount(&aps)
        .await;

    let td = tempfile::TempDir::new().expect("tempdir");
    let temp_root = camino::Utf8Path::from_path(td.path())
        .expect("tempdir is utf-8")
        .to_path_buf();

    let env = EnvGuard::new(ENV_KEYS);
    env.set("DOIGET_STORE_ROOT", temp_root.join("papers").as_str());
    env.set("DOIGET_LOG_PATH", temp_root.join("log.jsonl").as_str());
    env.set("DOIGET_CROSSREF_BASE", &server.uri());
    env.set("DOIGET_UNPAYWALL_BASE", &format!("{}/v2", server.uri()));
    env.set("DOIGET_OA_PUBLISHER_BASE", &server.uri());
    env.set("DOIGET_APS_BASE", &aps.uri());
    env.set("DOIGET_KEY_APS", KEY);
    env.set("DOIGET_AGREE_TDM_APS", "1");

    let (client, server_handle) = boot_in_memory_server().await?;

    let mut args = serde_json::Map::new();
    args.insert("ref".to_string(), serde_json::json!(APS_DOI));
    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_fetch_paper").with_arguments(args))
        .await?;
    let structured = result
        .structured_content
        .as_ref()
        .expect("doiget_fetch_paper uses CallToolResult::structured");

    assert_eq!(
        structured["pdf"]["status"],
        serde_json::json!("tdm_fetched"),
        "the route the whole Tier-3 feature exists for, and the one that had no          assertion anywhere: {structured:?}"
    );
    assert_eq!(
        structured["source"],
        serde_json::json!("tdm-aps"),
        "and it must name WHICH agreement was drawn on, because that one has          terms attached: {structured:?}"
    );
    assert!(
        structured["size_bytes"].as_u64().unwrap_or(0) > 0,
        "bytes, not a metadata-only stand-in: {structured:?}"
    );

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    drop(td);
    Ok(())
}
