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
    "DOIGET_NCBI_BASE",
    "DOIGET_BIORXIV_BASE",
    "DOIGET_ENABLE_BIORXIV",
    "DOIGET_INSPIRE_BASE",
    "DOIGET_ENABLE_INSPIRE",
    "DOIGET_ADS_BASE",
    "DOIGET_ADS_TOKEN",
    "DOIGET_UNPAYWALL_EMAIL",
    // #462: the Tier-3 route. Cleared for every test so an APS grant can
    // never leak from one into another.
    "DOIGET_APS_BASE",
    "DOIGET_KEY_APS",
    "DOIGET_AGREE_TDM_APS",
    // #587: the Tier-2 reachability case.
    "DOIGET_DATACITE_BASE",
    "DOIGET_ENABLE_DATACITE",
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

    // #507 / ADR-0057 D4: the repeat is a replay, and it is ok:false -- the
    // first call wrote metadata, the repeat does nothing at all.
    let sent = server.received_requests().await.unwrap_or_default().len();
    let mut args = serde_json::Map::new();
    args.insert("ref".to_string(), serde_json::json!("10.1234/suggest-test"));
    let again = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_fetch_paper").with_arguments(args))
        .await?;
    let again = again.structured_content.expect("structured");
    assert_eq!(again["ok"], serde_json::json!(false), "{again:?}");
    assert_eq!(
        again["error"]["replayed"],
        serde_json::json!(true),
        "{again:?}"
    );
    assert!(again["error"]["code"].is_string(), "{again:?}");
    assert_eq!(
        server.received_requests().await.unwrap_or_default().len(),
        sent,
        "a replay must not reach the network"
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
    assert_eq!(
        structured["pdf"]["found_by"],
        serde_json::json!("unpaywall"),
        "ADR-0062: who named the preprint: {structured:?}"
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
    // #608: always present, so an agent can rely on the keys; empty here.
    assert_eq!(structured["metadata_quality"], serde_json::json!([]));
    assert_eq!(structured["repaired_fields"], serde_json::json!({}));

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    drop(td);
    Ok(())
}

#[tokio::test]
#[serial_test::serial]
async fn fetch_paper_doi_flags_a_title_that_lost_characters_to_u_fffd() -> anyhow::Result<()> {
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
                "title": ["N\u{FFFD}herungsmethode zur L\u{FFFD}sung"],
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
    // #608: always present, so an agent can rely on the keys; empty here.
    // #608: no repair source is enabled, so the flag is the whole answer.
    assert_eq!(
        structured["metadata_quality"],
        serde_json::json!(["replacement_char:title"]),
        "{structured:?}"
    );
    assert_eq!(structured["repaired_fields"], serde_json::json!({}));

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

/// #587 (review of #621): a Tier-2 source is reached THROUGH the rewired
/// `build_http_client_for_fetch`, end to end. Before one table drove both
/// builders, `DOIGET_DATACITE_BASE` was absent from the override client, so
/// this request died at `UnknownSource` and DataCite could not be mocked.
///
/// Crossref has no record (a DataCite-registered DOI), so the optional chain
/// runs and DataCite answers.
#[cfg(feature = "citation")]
#[tokio::test]
#[serial_test::serial]
async fn fetch_paper_reaches_datacite_through_the_override_client() -> anyhow::Result<()> {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/works/10.5281/zenodo.22053902"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/dois/10.5281/zenodo.22053902"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": {
                "id": "10.5281/zenodo.22053902",
                "type": "dois",
                "attributes": {
                    "doi": "10.5281/zenodo.22053902",
                    "titles": [{"title": "An Example Deposit"}],
                    "creators": [{"name": "Researcher, Alice"}],
                    "publicationYear": 2024,
                    "publisher": "Zenodo",
                    "types": {"resourceTypeGeneral": "Dataset"}
                }
            }
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let td = tempfile::TempDir::new().expect("tempdir");
    let root = camino::Utf8Path::from_path(td.path())
        .expect("utf-8")
        .to_path_buf();
    let env = EnvGuard::new(ENV_KEYS);
    env.set("DOIGET_STORE_ROOT", root.join("papers").as_str());
    env.set("DOIGET_LOG_PATH", root.join("log.jsonl").as_str());
    env.set("DOIGET_CROSSREF_BASE", &server.uri());
    env.set("DOIGET_UNPAYWALL_BASE", &format!("{}/v2", server.uri()));
    env.set("DOIGET_DATACITE_BASE", &server.uri());
    env.set("DOIGET_ENABLE_DATACITE", "1");

    let (client, server_handle) = boot_in_memory_server().await?;
    let mut args = serde_json::Map::new();
    args.insert("ref".into(), serde_json::json!("10.5281/zenodo.22053902"));
    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_fetch_paper").with_arguments(args))
        .await?;
    let structured = result.structured_content.as_ref().expect("structured");

    let datacite_hits = server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| r.url.path().starts_with("/dois/"))
        .count();
    assert_eq!(datacite_hits, 1, "DataCite was never asked: {structured:?}");
    assert_eq!(structured["title"], "An Example Deposit", "{structured:?}");

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    drop(td);
    Ok(())
}

/// #507 over MCP: asking again about a DOI this server was just told does
/// not exist is answered as a replay (same code, `replayed: true`) without a
/// request, and `force: true` asks anyway.
#[tokio::test]
#[serial_test::serial]
async fn a_repeated_terminal_answer_is_a_replay_until_forced() -> anyhow::Result<()> {
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let td = tempfile::TempDir::new().expect("tempdir");
    let root = camino::Utf8Path::from_path(td.path())
        .expect("utf-8")
        .to_path_buf();
    let env = EnvGuard::new(ENV_KEYS);
    env.set("DOIGET_STORE_ROOT", root.join("papers").as_str());
    env.set("DOIGET_LOG_PATH", root.join("log.jsonl").as_str());
    env.set("DOIGET_CROSSREF_BASE", &server.uri());
    env.set("DOIGET_UNPAYWALL_BASE", &format!("{}/v2", server.uri()));

    let (client, server_handle) = boot_in_memory_server().await?;
    let call = |force: bool| {
        let mut args = serde_json::Map::new();
        args.insert("ref".into(), serde_json::json!("10.1234/nowhere"));
        if force {
            args.insert("force".into(), serde_json::json!(true));
        }
        client
            .peer()
            .call_tool(CallToolRequestParams::new("doiget_fetch_paper").with_arguments(args))
    };
    let requests = || async { server.received_requests().await.unwrap_or_default().len() };

    let first = call(false).await?;
    let first = first.structured_content.expect("structured");
    assert_eq!(first["error"]["code"], "NOT_FOUND", "{first:?}");
    assert!(first["error"].get("replayed").is_none());
    let sent = requests().await;

    let second = call(false).await?;
    let second = second.structured_content.expect("structured");
    assert_eq!(second["ok"], false);
    assert_eq!(
        second["error"]["code"], "NOT_FOUND",
        "same answer: {second:?}"
    );
    assert_eq!(second["error"]["replayed"], true, "{second:?}");
    assert_eq!(
        requests().await,
        sent,
        "a replay must not reach the network"
    );

    let forced = call(true).await?;
    let forced = forced.structured_content.expect("structured");
    assert!(forced["error"].get("replayed").is_none(), "{forced:?}");
    assert!(requests().await > sent, "force asks again");

    // The batch tools take force too; without it their entry is a replay.
    let refs_file = root.join("refs.txt");
    std::fs::write(&refs_file, "10.1234/nowhere\n").expect("refs file");
    for tool in ["doiget_batch_fetch", "doiget_batch_from_bibliography"] {
        let batch = |force: bool| {
            let mut args = serde_json::Map::new();
            if tool == "doiget_batch_fetch" {
                args.insert("refs".into(), serde_json::json!(["10.1234/nowhere"]));
            } else {
                args.insert("path".into(), serde_json::json!(refs_file.as_str()));
                args.insert("format".into(), serde_json::json!("refs"));
            }
            if force {
                args.insert("force".into(), serde_json::json!(true));
            }
            client
                .peer()
                .call_tool(CallToolRequestParams::new(tool).with_arguments(args))
        };
        let before = requests().await;
        let replayed = batch(false).await?.structured_content.expect("structured");
        let entry = &replayed["results"][0];
        assert_eq!(entry["error"]["replayed"], true, "{tool}: {replayed:?}");
        assert_eq!(requests().await, before, "{tool}: a replay asks nothing");
        let forced = batch(true).await?.structured_content.expect("structured");
        let entry = &forced["results"][0];
        assert!(
            entry["error"].get("replayed").is_none(),
            "{tool}: {forced:?}"
        );
        assert!(requests().await > before, "{tool}: force asks again");
    }

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    drop(td);
    Ok(())
}

/// #500 over MCP: a PMID entry is fetched under the DOI PubMed lists for it;
/// one whose record lists no DOI is a NOT_IMPLEMENTED row, or, under
/// `strict`, the whole call's error.
#[tokio::test]
#[serial_test::serial]
async fn batch_from_bibliography_resolves_pubmed_ids_500() -> anyhow::Result<()> {
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/esummary.fcgi"))
        .and(query_param("id", "9659853"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": {"uids": ["9659853"], "9659853": {"articleids": [
                {"idtype": "doi", "value": "10.1176/ajp.155.7.895"}]}}
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/esummary.fcgi"))
        .and(query_param("id", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": {"uids": ["1"], "1": {"articleids": [{"idtype": "pubmed", "value": "1"}]}}
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let td = tempfile::TempDir::new().expect("tempdir");
    let root = camino::Utf8Path::from_path(td.path())
        .expect("utf-8")
        .to_path_buf();
    let bib = root.join("refs.bib");
    std::fs::write(
        &bib,
        "@article{coryell, title={Lithium}, pmid={9659853}}\n@article{nodoi, title={Old}, pmid={1}}\n",
    )?;
    let env = EnvGuard::new(ENV_KEYS);
    env.set("DOIGET_STORE_ROOT", root.join("papers").as_str());
    env.set("DOIGET_LOG_PATH", root.join("log.jsonl").as_str());
    env.set("DOIGET_NCBI_BASE", &server.uri());
    env.set("DOIGET_CROSSREF_BASE", &server.uri());
    env.set("DOIGET_UNPAYWALL_BASE", &format!("{}/v2", server.uri()));

    let (client, server_handle) = boot_in_memory_server().await?;
    let call = |strict: bool| {
        let mut args = serde_json::Map::new();
        args.insert("path".into(), serde_json::json!(bib.as_str()));
        args.insert("format".into(), serde_json::json!("bibtex"));
        args.insert("strict".into(), serde_json::json!(strict));
        client.peer().call_tool(
            CallToolRequestParams::new("doiget_batch_from_bibliography").with_arguments(args),
        )
    };
    let lenient = call(false).await?.structured_content.expect("structured");
    let results = lenient["results"].as_array().expect("results");
    let by_key = |k: &str| {
        results
            .iter()
            .find(|r| r["entry_key"] == k)
            .unwrap_or_else(|| panic!("{k}: {lenient}"))
    };
    assert_eq!(
        by_key("coryell")["ref"],
        "10.1176/ajp.155.7.895",
        "{lenient}"
    );
    assert_eq!(
        by_key("nodoi")["error"]["code"],
        "NOT_IMPLEMENTED",
        "{lenient}"
    );
    assert_eq!(by_key("nodoi")["ref"], "PMID 1", "{lenient}");

    let strict = call(true).await?.structured_content.expect("structured");
    assert_eq!(strict["ok"], false, "{strict}");
    assert_eq!(strict["error"]["code"], "NOT_IMPLEMENTED", "{strict}");

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    drop(td);
    Ok(())
}

/// ADR-0062, the measured case: closed at the publisher, unknown to
/// Unpaywall, on arXiv all the same. Found by arXiv's title search, and
/// fetched as #325 fetches -- or, when Crossref's relation names it, found
/// with no search at all.
async fn preprint_discovery_case(
    relation: serde_json::Value,
) -> anyhow::Result<(serde_json::Value, Vec<String>)> {
    preprint_discovery_case_with(relation, None).await
}

/// `publisher_pdf`: an Unpaywall location on a host off the allowlist, so
/// the content leg is Blocked -- with no arXiv hint -- rather than NoOaUrl.
async fn preprint_discovery_case_with(
    relation: serde_json::Value,
    publisher_pdf: Option<&str>,
) -> anyhow::Result<(serde_json::Value, Vec<String>)> {
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const TITLE: &str = "Environment-matrix-product operator for boundary-free large-scale quantum many-body simulations";
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/works/10.1103/bbnt-brjz"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "ok",
            "message": {
                "title": [TITLE],
                "author": [{"family": "Shimozono", "given": "Souta"}, {"family": "Hotta", "given": "Chisa"}],
                "issued": {"date-parts": [[2026, 6, 5]]},
                "type": "journal-article",
                "relation": relation
            }
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v2/10.1103%2Fbbnt-brjz"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(match publisher_pdf {
                None => serde_json::json!({
                    "doi": "10.1103/bbnt-brjz", "is_oa": false, "oa_status": "closed",
                    "best_oa_location": null, "oa_locations": []
                }),
                Some(pdf) => serde_json::json!({
                    "doi": "10.1103/bbnt-brjz", "is_oa": true, "oa_status": "bronze",
                    "best_oa_location": {"url_for_pdf": pdf, "url": pdf},
                    "oa_locations": [{"url_for_pdf": pdf, "url": pdf}]
                }),
            }),
        )
        .mount(&server)
        .await;

    let arxiv = MockServer::start().await;
    let entry = format!(
        "<feed><entry><id>http://arxiv.org/abs/2512.07923v1</id><title>{TITLE}</title>\
         <published>2025-12-08T00:00:00Z</published>\
         <author><name>Souta Shimozono</name></author><author><name>Chisa Hotta</name></author>\
         </entry></feed>"
    );
    Mock::given(method("GET"))
        .and(path("/api/query"))
        .and(query_param("max_results", "5"))
        .respond_with(ResponseTemplate::new(200).set_body_string(entry.clone()))
        .mount(&arxiv)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/query"))
        .respond_with(ResponseTemplate::new(200).set_body_string(entry))
        .mount(&arxiv)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(SAMPLE_PDF_BODY.to_vec()))
        .mount(&arxiv)
        .await;

    let td = tempfile::TempDir::new().expect("tempdir");
    let root = camino::Utf8Path::from_path(td.path())
        .expect("utf-8")
        .to_path_buf();
    let env = EnvGuard::new(ENV_KEYS);
    env.set("DOIGET_STORE_ROOT", root.join("papers").as_str());
    env.set("DOIGET_LOG_PATH", root.join("log.jsonl").as_str());
    env.set("DOIGET_CROSSREF_BASE", &server.uri());
    env.set("DOIGET_UNPAYWALL_BASE", &format!("{}/v2", server.uri()));
    env.set("DOIGET_OA_PUBLISHER_BASE", &server.uri());
    env.set("DOIGET_ARXIV_BASE", &arxiv.uri());

    let (client, server_handle) = boot_in_memory_server().await?;
    let mut args = serde_json::Map::new();
    args.insert("ref".to_string(), serde_json::json!("10.1103/bbnt-brjz"));
    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_fetch_paper").with_arguments(args))
        .await?;
    let structured = result.structured_content.clone().expect("structured");
    let searches = arxiv
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter_map(|r| r.url.query().map(str::to_string))
        .filter(|q| q.contains("search_query"))
        .collect();
    client.cancel().await?;
    server_handle.await??;
    drop(env);
    drop(td);
    Ok((structured, searches))
}

#[tokio::test]
#[serial_test::serial]
async fn a_closed_doi_unknown_to_unpaywall_finds_its_preprint_by_arxiv_search() -> anyhow::Result<()>
{
    let (v, searches) = preprint_discovery_case(serde_json::json!({})).await?;
    assert_eq!(v["pdf"]["status"], "preprint_fallback", "{v}");
    assert_eq!(v["pdf"]["arxiv_id"], "2512.07923", "{v}");
    assert_eq!(v["pdf"]["found_by"], "arxiv_title_search", "{v}");
    assert_eq!(v["source"], "arxiv", "{v}");
    assert_eq!(searches.len(), 1, "one search: {searches:?}");
    assert!(searches[0].contains("Shimozono"), "{searches:?}");
    Ok(())
}

#[tokio::test]
#[serial_test::serial]
async fn a_crossref_has_preprint_relation_needs_no_search() -> anyhow::Result<()> {
    let relation = serde_json::json!({"has-preprint": [
        {"id-type": "doi", "id": "10.48550/arXiv.2512.07923", "asserted-by": "subject"}]});
    let (v, searches) = preprint_discovery_case(relation).await?;
    assert_eq!(v["pdf"]["status"], "preprint_fallback", "{v}");
    assert_eq!(v["pdf"]["found_by"], "crossref_relation", "{v}");
    assert!(
        searches.is_empty(),
        "the relation answered; no search: {searches:?}"
    );
    Ok(())
}

#[tokio::test]
#[serial_test::serial]
async fn a_blocked_copy_with_no_arxiv_hint_also_looks_for_the_preprint() -> anyhow::Result<()> {
    let (v, searches) = preprint_discovery_case_with(
        serde_json::json!({}),
        Some("https://journals.example-publisher.org/paper.pdf"),
    )
    .await?;
    assert_eq!(v["pdf"]["status"], "preprint_fallback", "{v}");
    assert_eq!(v["pdf"]["found_by"], "arxiv_title_search", "{v}");
    assert!(
        v["pdf"]["original_block"]
            .as_str()
            .unwrap_or("")
            .contains("journals.example-publisher.org"),
        "the block that triggered the search is kept: {v}"
    );
    assert_eq!(searches.len(), 1, "{searches:?}");
    Ok(())
}

/// #638: the MCP single-ref tools take a PubMed id the way the CLI does --
/// resolved to its DOI by the network tools, named (not called malformed)
/// by the local ones, and never looked up under dry_run.
#[tokio::test]
#[serial_test::serial]
async fn single_ref_tools_take_a_pubmed_id_638() -> anyhow::Result<()> {
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let esummary = |uid: &str, body: serde_json::Value| {
        ResponseTemplate::new(200)
            .set_body_json(serde_json::json!({"result": {"uids": [uid], uid: body}}))
    };
    Mock::given(method("GET"))
        .and(path("/esummary.fcgi"))
        .and(query_param("id", "9659853"))
        .respond_with(esummary(
            "9659853",
            serde_json::json!({"articleids": [{"idtype": "doi", "value": "10.1176/ajp.155.7.895"}]}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/esummary.fcgi"))
        .and(query_param("id", "1"))
        .respond_with(esummary(
            "1",
            serde_json::json!({"articleids": [{"idtype": "pubmed", "value": "1"}]}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/esummary.fcgi"))
        .and(query_param("id", "42"))
        .respond_with(esummary(
            "42",
            serde_json::json!({"error": "cannot get document summary"}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/esummary.fcgi"))
        .and(query_param("id", "777"))
        .respond_with(esummary(
            "777",
            serde_json::json!({"articleids": [{"idtype": "doi", "value": "10.9999/gone"}]}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/works/10.1176/ajp.155.7.895"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "ok",
            "message": {"title": ["Lithium discontinuation"], "type": "journal-article",
                        "author": [{"family": "Coryell", "given": "W"}]}
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let td = tempfile::TempDir::new().expect("tempdir");
    let root = camino::Utf8Path::from_path(td.path())
        .expect("utf-8")
        .to_path_buf();
    let env = EnvGuard::new(ENV_KEYS);
    env.set("DOIGET_STORE_ROOT", root.join("papers").as_str());
    env.set("DOIGET_LOG_PATH", root.join("log.jsonl").as_str());
    env.set("DOIGET_NCBI_BASE", &server.uri());
    env.set("DOIGET_CROSSREF_BASE", &server.uri());
    env.set("DOIGET_UNPAYWALL_BASE", &format!("{}/v2", server.uri()));

    let (client, server_handle) = boot_in_memory_server().await?;
    let call = |tool: &'static str, args: serde_json::Value| {
        let args = args.as_object().cloned().unwrap_or_default();
        client
            .peer()
            .call_tool(CallToolRequestParams::new(tool).with_arguments(args))
    };
    let ncbi_requests = || async {
        server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|r| r.url.path() == "/esummary.fcgi")
            .count()
    };

    // A dry run makes no lookup.
    let dry = call(
        "doiget_fetch_paper",
        serde_json::json!({"ref": "pmid:9659853", "dry_run": true}),
    )
    .await?
    .structured_content
    .expect("structured");
    assert_eq!(dry["error"]["code"], "INVALID_REF", "{dry}");
    assert!(
        dry["error"]["message"]
            .as_str()
            .unwrap()
            .contains("dry_run makes no request"),
        "{dry}"
    );
    assert_eq!(ncbi_requests().await, 0, "dry_run asked NCBI");

    // The network tools resolve it.
    let meta = call(
        "doiget_resolve_paper",
        serde_json::json!({"ref": "pmid:9659853"}),
    )
    .await?
    .structured_content
    .expect("structured");
    assert_ne!(meta["error"]["code"], "INVALID_REF", "{meta}");
    assert_eq!(ncbi_requests().await, 1);

    let unknown = call("doiget_resolve_paper", serde_json::json!({"ref": "PMC42"}))
        .await?
        .structured_content
        .expect("structured");
    assert_eq!(unknown["error"]["code"], "NOT_FOUND", "{unknown}");

    let batch = call(
        "doiget_batch_fetch",
        serde_json::json!({"refs": ["pmid:1"]}),
    )
    .await?
    .structured_content
    .expect("structured");
    assert_eq!(batch["error"]["code"], "NOT_IMPLEMENTED", "{batch}");
    assert!(
        batch["error"]["message"]
            .as_str()
            .unwrap()
            .contains("lists no DOI"),
        "{batch}"
    );

    // doiget_metadata_only: no lookup under dry_run, a resolution without.
    let meta_dry = call(
        "doiget_metadata_only",
        serde_json::json!({"ref": "pmid:9659853", "dry_run": true}),
    )
    .await?
    .structured_content
    .expect("structured");
    assert_eq!(meta_dry["error"]["code"], "INVALID_REF", "{meta_dry}");
    let meta_live = call(
        "doiget_metadata_only",
        serde_json::json!({"ref": "pmid:9659853"}),
    )
    .await?
    .structured_content
    .expect("structured");
    assert_ne!(meta_live["error"]["code"], "INVALID_REF", "{meta_live}");

    // Repeat suppression keys on the DOI the call ran under, so a PubMed id
    // whose DOI is terminally absent is replayed the second time (#638
    // review: the session_end row used to carry the raw `pmid:` input, a
    // key the replay check never looks up).
    let first = call("doiget_fetch_paper", serde_json::json!({"ref": "pmid:777"}))
        .await?
        .structured_content
        .expect("structured");
    assert_eq!(first["error"]["code"], "NOT_FOUND", "{first}");
    let second = call("doiget_fetch_paper", serde_json::json!({"ref": "pmid:777"}))
        .await?
        .structured_content
        .expect("structured");
    assert_eq!(second["error"]["replayed"], true, "{second}");

    // A local-only tool names it rather than calling it malformed.
    let info = call("doiget_info", serde_json::json!({"ref": "pmid:9659853"}))
        .await?
        .structured_content
        .expect("structured");
    assert_eq!(info["error"]["code"], "INVALID_REF", "{info}");
    assert!(
        info["error"]["message"]
            .as_str()
            .unwrap()
            .contains("is a PubMed id"),
        "{info}"
    );

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    drop(td);
    Ok(())
}

/// #640: a closed journal DOI whose Crossref record names a bioRxiv /
/// medRxiv preprint DOI (no arXiv one): the preprint is fetched through the
/// OA location Unpaywall reports for *its* DOI, and the entry says so.
#[tokio::test]
#[serial_test::serial]
async fn a_crossref_named_medrxiv_preprint_is_fetched_through_its_own_doi_640() -> anyhow::Result<()>
{
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/works/10.1371/journal.pone.0256482"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "ok",
            "message": {
                "title": ["Original antigenic sin responses"],
                "author": [{"family": "Lapp", "given": "S. A."}],
                "type": "journal-article",
                "relation": {"has-preprint": [
                    {"id-type": "doi", "id": "10.1101/2021.04.29.21256344", "asserted-by": "subject"}]}
            }
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v2/10.1371%2Fjournal.pone.0256482"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "doi": "10.1371/journal.pone.0256482", "is_oa": false, "oa_status": "closed",
            "best_oa_location": null, "oa_locations": []
        })))
        .mount(&server)
        .await;
    let preprint_pdf = format!(
        "{}/content/10.1101/2021.04.29.21256344v1.full.pdf",
        server.uri()
    );
    Mock::given(method("GET"))
        .and(path("/v2/10.1101%2F2021.04.29.21256344"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "doi": "10.1101/2021.04.29.21256344", "is_oa": true, "oa_status": "green",
            "best_oa_location": {"url_for_pdf": preprint_pdf, "url": preprint_pdf, "license": "cc-by-nc-nd"},
            "oa_locations": [{"url_for_pdf": preprint_pdf, "url": preprint_pdf, "license": "cc-by-nc-nd"}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/content/10.1101/2021.04.29.21256344v1.full.pdf"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(SAMPLE_PDF_BODY.to_vec()))
        .mount(&server)
        .await;
    // arXiv search finds nothing (an empty feed), so the non-arXiv route runs.
    let arxiv = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<feed></feed>"))
        .mount(&arxiv)
        .await;

    let td = tempfile::TempDir::new().expect("tempdir");
    let root = camino::Utf8Path::from_path(td.path())
        .expect("utf-8")
        .to_path_buf();
    let env = EnvGuard::new(ENV_KEYS);
    env.set("DOIGET_STORE_ROOT", root.join("papers").as_str());
    env.set("DOIGET_LOG_PATH", root.join("log.jsonl").as_str());
    env.set("DOIGET_CROSSREF_BASE", &server.uri());
    env.set("DOIGET_UNPAYWALL_BASE", &format!("{}/v2", server.uri()));
    env.set("DOIGET_OA_PUBLISHER_BASE", &server.uri());
    env.set("DOIGET_ARXIV_BASE", &arxiv.uri());

    let (client, server_handle) = boot_in_memory_server().await?;
    let mut args = serde_json::Map::new();
    args.insert(
        "ref".into(),
        serde_json::json!("10.1371/journal.pone.0256482"),
    );
    let v = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_fetch_paper").with_arguments(args))
        .await?
        .structured_content
        .expect("structured");
    assert_eq!(v["pdf"]["status"], "preprint_doi_fallback", "{v}");
    assert_eq!(
        v["pdf"]["preprint_doi"], "10.1101/2021.04.29.21256344",
        "{v}"
    );
    assert_eq!(v["pdf"]["found_by"], "crossref_relation", "{v}");
    assert_eq!(v["license"], "cc-by-nc-nd", "the preprint's licence: {v}");
    let toml = std::fs::read_to_string(
        root.join("papers/.metadata/doi_10.1371_journal.pone.0256482.toml"),
    )?;
    assert!(
        toml.contains("preprint_doi = \"10.1101/2021.04.29.21256344\""),
        "{toml}"
    );

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    drop(td);
    Ok(())
}

/// Options for the #640 non-arXiv preprint cases.
struct NonArxivCase {
    relation: serde_json::Value,
    /// An off-allowlist publisher PDF, so the leg is Blocked, not NoOaUrl.
    journal_pdf: Option<&'static str>,
    /// Whether Unpaywall reports an OA location for the preprint DOI.
    preprint_has_location: bool,
    /// Serve a bioRxiv `pubs` answer and enable it.
    biorxiv_pubs: bool,
    /// An arXiv search hit for the title.
    arxiv_hit: bool,
    /// Serve an INSPIRE record naming the arXiv id, and enable it (#642).
    inspire: bool,
    /// Serve an ADS answer naming the arXiv id, and set a token (#644).
    ads: bool,
}

async fn nonarxiv_case(c: NonArxivCase) -> anyhow::Result<(serde_json::Value, Vec<String>)> {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    const TITLE: &str =
        "Original antigenic sin responses to heterologous Betacoronavirus spike proteins";
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/works/10.1371/journal.pone.0256482"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "ok",
            "message": {"title": [TITLE], "author": [{"family": "Lapp", "given": "S. A."}],
                        "type": "journal-article", "relation": c.relation}
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v2/10.1371%2Fjournal.pone.0256482"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(match c.journal_pdf {
                None => serde_json::json!({"doi": "10.1371/journal.pone.0256482", "is_oa": false,
                "oa_status": "closed", "best_oa_location": null, "oa_locations": []}),
                Some(pdf) => {
                    serde_json::json!({"doi": "10.1371/journal.pone.0256482", "is_oa": true,
                "oa_status": "bronze", "best_oa_location": {"url_for_pdf": pdf, "url": pdf},
                "oa_locations": [{"url_for_pdf": pdf, "url": pdf}]})
                }
            }),
        )
        .mount(&server)
        .await;
    let preprint_pdf = format!(
        "{}/content/10.1101/2021.04.29.21256344v1.full.pdf",
        server.uri()
    );
    let locations = if c.preprint_has_location {
        serde_json::json!([{"url_for_pdf": preprint_pdf, "url": preprint_pdf, "license": "cc-by"}])
    } else {
        serde_json::json!([])
    };
    Mock::given(method("GET"))
        .and(path("/v2/10.1101%2F2021.04.29.21256344"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "doi": "10.1101/2021.04.29.21256344", "is_oa": c.preprint_has_location,
            "oa_status": if c.preprint_has_location { "green" } else { "closed" },
            "best_oa_location": locations.get(0), "oa_locations": locations
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/content/10.1101/2021.04.29.21256344v1.full.pdf"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(SAMPLE_PDF_BODY.to_vec()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/search/query"))
        .and(wiremock::matchers::header(
            "Authorization",
            "Bearer test-ads-token",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "response": {"docs": [{"identifier": [
                "10.1371/journal.pone.0256482", "arXiv:2105.00077", "2021arXiv210500077L"]}]}
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/doi/10.1371/journal.pone.0256482"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "metadata": {"arxiv_eprints": [{"value": "2105.00042", "categories": ["q-bio.PE"]}]}
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/pubs/biorxiv/10.1371/journal.pone.0256482/na/json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "collection": [{"preprint_doi": "10.1101/2021.04.29.21256344", "preprint_platform": "bioRxiv"}]
        })))
        .mount(&server)
        .await;
    let arxiv = MockServer::start().await;
    let feed = if c.arxiv_hit {
        format!(
            "<feed><entry><id>http://arxiv.org/abs/2105.00001v1</id><title>{TITLE}</title>\
             <author><name>S. A. Lapp</name></author></entry></feed>"
        )
    } else {
        "<feed></feed>".to_string()
    };
    Mock::given(method("GET"))
        .and(path("/api/query"))
        .respond_with(ResponseTemplate::new(200).set_body_string(feed))
        .mount(&arxiv)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(SAMPLE_PDF_BODY.to_vec()))
        .mount(&arxiv)
        .await;

    let td = tempfile::TempDir::new().expect("tempdir");
    let root = camino::Utf8Path::from_path(td.path())
        .expect("utf-8")
        .to_path_buf();
    let env = EnvGuard::new(ENV_KEYS);
    env.set("DOIGET_STORE_ROOT", root.join("papers").as_str());
    env.set("DOIGET_LOG_PATH", root.join("log.jsonl").as_str());
    env.set("DOIGET_CROSSREF_BASE", &server.uri());
    env.set("DOIGET_UNPAYWALL_BASE", &format!("{}/v2", server.uri()));
    env.set("DOIGET_OA_PUBLISHER_BASE", &server.uri());
    env.set("DOIGET_ARXIV_BASE", &arxiv.uri());
    if c.biorxiv_pubs {
        env.set("DOIGET_BIORXIV_BASE", &server.uri());
        env.set("DOIGET_ENABLE_BIORXIV", "1");
    }
    if c.inspire {
        env.set("DOIGET_INSPIRE_BASE", &server.uri());
        env.set("DOIGET_ENABLE_INSPIRE", "1");
    }
    if c.ads {
        env.set("DOIGET_ADS_BASE", &server.uri());
        env.set("DOIGET_ADS_TOKEN", "test-ads-token");
    }
    let (client, server_handle) = boot_in_memory_server().await?;
    let mut args = serde_json::Map::new();
    args.insert(
        "ref".into(),
        serde_json::json!("10.1371/journal.pone.0256482"),
    );
    let v = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_fetch_paper").with_arguments(args))
        .await?
        .structured_content
        .expect("structured");
    let paths = server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .map(|r| r.url.path().to_string())
        .collect();
    client.cancel().await?;
    server_handle.await??;
    drop(env);
    drop(td);
    Ok((v, paths))
}

const MEDRXIV_RELATION: &str =
    r#"{"has-preprint": [{"id-type": "doi", "id": "10.1101/2021.04.29.21256344"}]}"#;

#[tokio::test]
#[serial_test::serial]
async fn a_preprint_doi_with_no_open_location_leaves_the_leg_as_it_was_640() -> anyhow::Result<()> {
    let (v, _) = nonarxiv_case(NonArxivCase {
        relation: serde_json::from_str(MEDRXIV_RELATION)?,
        journal_pdf: None,
        preprint_has_location: false,
        biorxiv_pubs: false,
        arxiv_hit: false,
        inspire: false,
        ads: false,
    })
    .await?;
    assert_eq!(v["ok"], true, "not an error: {v}");
    assert_eq!(v["pdf"]["status"], "no_oa_url", "{v}");
    Ok(())
}

#[tokio::test]
#[serial_test::serial]
async fn an_arxiv_preprint_wins_over_a_sibling_non_arxiv_one_640() -> anyhow::Result<()> {
    let (v, paths) = nonarxiv_case(NonArxivCase {
        relation: serde_json::from_str(MEDRXIV_RELATION)?,
        journal_pdf: None,
        preprint_has_location: true,
        biorxiv_pubs: false,
        arxiv_hit: true,
        inspire: false,
        ads: false,
    })
    .await?;
    assert_eq!(v["pdf"]["status"], "preprint_fallback", "{v}");
    assert_eq!(v["pdf"]["arxiv_id"], "2105.00001", "{v}");
    assert!(
        !paths
            .iter()
            .any(|p| p == "/v2/10.1101%2F2021.04.29.21256344"),
        "the non-arXiv route was not needed: {paths:?}"
    );
    Ok(())
}

#[tokio::test]
#[serial_test::serial]
async fn a_blocked_copy_also_follows_a_non_arxiv_preprint_640() -> anyhow::Result<()> {
    let (v, _) = nonarxiv_case(NonArxivCase {
        relation: serde_json::from_str(MEDRXIV_RELATION)?,
        journal_pdf: Some("https://journals.example-publisher.org/paper.pdf"),
        preprint_has_location: true,
        biorxiv_pubs: false,
        arxiv_hit: false,
        inspire: false,
        ads: false,
    })
    .await?;
    assert_eq!(v["pdf"]["status"], "preprint_doi_fallback", "{v}");
    assert_eq!(
        v["pdf"]["preprint_doi"], "10.1101/2021.04.29.21256344",
        "{v}"
    );
    assert!(
        v["pdf"]["original_block"]
            .as_str()
            .unwrap_or("")
            .contains("journals.example-publisher.org"),
        "{v}"
    );
    Ok(())
}

/// The opt-in half: no Crossref relation, bioRxiv `pubs` enabled and
/// answering. Needs `metadata` (the flag is compiled only there).
#[cfg(feature = "citation")]
#[tokio::test]
#[serial_test::serial]
async fn biorxiv_pubs_names_the_preprint_end_to_end_640() -> anyhow::Result<()> {
    let (v, paths) = nonarxiv_case(NonArxivCase {
        relation: serde_json::json!({}),
        journal_pdf: None,
        preprint_has_location: true,
        biorxiv_pubs: true,
        arxiv_hit: false,
        inspire: false,
        ads: false,
    })
    .await?;
    assert_eq!(v["pdf"]["status"], "preprint_doi_fallback", "{v}");
    assert_eq!(v["pdf"]["found_by"], "biorxiv_pubs", "{v}");
    assert_eq!(v["pdf"]["platform"], "bioRxiv", "{v}");
    assert!(
        paths.iter().any(|p| p.starts_with("/pubs/biorxiv/")),
        "{paths:?}"
    );
    Ok(())
}

/// #642 end to end: env var -> CapabilityProfile.metadata.inspire ->
/// preprint::find -> the arXiv preprint INSPIRE names, reported as such.
#[cfg(feature = "citation")]
#[tokio::test]
#[serial_test::serial]
async fn inspire_names_the_arxiv_preprint_end_to_end_642() -> anyhow::Result<()> {
    let (v, paths) = nonarxiv_case(NonArxivCase {
        relation: serde_json::json!({}),
        journal_pdf: None,
        preprint_has_location: false,
        biorxiv_pubs: false,
        arxiv_hit: false,
        inspire: true,
        ads: false,
    })
    .await?;
    assert_eq!(v["pdf"]["status"], "preprint_fallback", "{v}");
    assert_eq!(v["pdf"]["arxiv_id"], "2105.00042", "{v}");
    assert_eq!(v["pdf"]["found_by"], "inspire", "{v}");
    assert!(
        paths.iter().any(|p| p.starts_with("/api/doi/")),
        "{paths:?}"
    );
    Ok(())
}

/// #644 end to end: the user's own ADS token is sent as a Bearer header
/// (the mock answers only with it), and ADS's `arXiv:` identifier is the
/// preprint fetched.
#[cfg(feature = "citation")]
#[tokio::test]
#[serial_test::serial]
async fn ads_names_the_arxiv_preprint_on_the_users_token_644() -> anyhow::Result<()> {
    let (v, paths) = nonarxiv_case(NonArxivCase {
        relation: serde_json::json!({}),
        journal_pdf: None,
        preprint_has_location: false,
        biorxiv_pubs: false,
        arxiv_hit: false,
        inspire: false,
        ads: true,
    })
    .await?;
    assert_eq!(v["pdf"]["status"], "preprint_fallback", "{v}");
    assert_eq!(v["pdf"]["arxiv_id"], "2105.00077", "{v}");
    assert_eq!(v["pdf"]["found_by"], "ads", "{v}");
    assert!(paths.iter().any(|p| p == "/v1/search/query"), "{paths:?}");
    Ok(())
}
