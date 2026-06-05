//! End-to-end coverage for the `doiget_paper_search` MCP tool (external
//! OpenAlex discovery; ADR-0031). Unlike the citation-graph tool this is
//! NOT feature-gated — discovery is Tier-1, always-on.
//!
//! All HTTP terminates at a `wiremock::MockServer` reached via
//! `DOIGET_OPENALEX_BASE`; no outbound network.
// allow: outbound-network

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use doiget_core::CapabilityProfile;
use doiget_mcp::Server;
use rmcp::{model::CallToolRequestParams, ServiceExt};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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
    fn set(&self, k: &str, v: &str) {
        std::env::set_var(k, v);
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
    "DOIGET_OPENALEX_BASE",
    "DOIGET_LOG_PATH",
    "DOIGET_CONTACT_EMAIL",
];

async fn boot_in_memory_server() -> anyhow::Result<(
    rmcp::service::RunningService<rmcp::RoleClient, ()>,
    tokio::task::JoinHandle<anyhow::Result<()>>,
)> {
    let profile = CapabilityProfile::from_env().expect("clean env never errors");
    let server = Server::new(profile);
    let (server_tx, client_tx) = tokio::io::duplex(64 * 1024);
    let server_handle = tokio::spawn(async move {
        let svc = server.serve(server_tx).await?;
        svc.waiting().await?;
        anyhow::Ok(())
    });
    let client = ().serve(client_tx).await?;
    Ok((client, server_handle))
}

const SAMPLE_SEARCH: &str = r#"{
    "meta": { "count": 4012 },
    "results": [
        {
            "id": "https://openalex.org/W123",
            "doi": "https://doi.org/10.1234/example",
            "title": "Tropical Tensor Networks",
            "publication_year": 2021,
            "cited_by_count": 42,
            "abstract_inverted_index": { "An": [0], "abstract": [1] },
            "authorships": [ { "author": { "display_name": "Ada Lovelace" } } ],
            "open_access": { "oa_status": "green" }
        }
    ]
}"#;

#[tokio::test]
#[serial_test::serial]
async fn paper_search_returns_external_envelope() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/works"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_SEARCH))
        .mount(&server)
        .await;

    let td = tempfile::TempDir::new().expect("tempdir");
    let log_path = camino::Utf8Path::from_path(td.path())
        .expect("utf-8 tempdir")
        .join("mcp-search.jsonl");

    let env = EnvGuard::new(ENV_KEYS);
    env.set("DOIGET_OPENALEX_BASE", &server.uri());
    env.set("DOIGET_LOG_PATH", log_path.as_str());

    let (client, server_handle) = boot_in_memory_server().await?;

    let mut args = serde_json::Map::new();
    args.insert(
        "query".to_string(),
        serde_json::json!("tropical tensor networks"),
    );
    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_paper_search").with_arguments(args))
        .await?;
    let s = result
        .structured_content
        .as_ref()
        .expect("doiget_paper_search uses CallToolResult::structured");

    assert_eq!(s["ok"], serde_json::json!(true), "envelope: {s:?}");
    assert_eq!(s["scope"], serde_json::json!("external"));
    assert_eq!(s["total_results"], serde_json::json!(4012));
    assert_eq!(s["count"], serde_json::json!(1));
    assert_eq!(s["results"][0]["openalex_id"], serde_json::json!("W123"));
    assert_eq!(
        s["results"][0]["abstract"],
        serde_json::json!("An abstract")
    );
    assert_eq!(s["results"][0]["source"], serde_json::json!("openalex"));

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    drop(td);
    Ok(())
}

#[tokio::test]
#[serial_test::serial]
async fn paper_search_ambiguous_author_maps_to_ambiguous_wire_code() -> anyhow::Result<()> {
    // Two close, non-exact author matches → the resolver returns
    // FetchError::Ambiguous, which the MCP error envelope must surface as
    // the AMBIGUOUS wire code (NOT INTERNAL_ERROR).
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/authors"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{ "results": [
                { "id": "https://openalex.org/A1", "display_name": "John Smith", "works_count": 300, "relevance_score": 50.0 },
                { "id": "https://openalex.org/A2", "display_name": "Jane Smith", "works_count": 280, "relevance_score": 45.0 }
            ] }"#,
        ))
        .mount(&server)
        .await;

    let td = tempfile::TempDir::new().expect("tempdir");
    let log_path = camino::Utf8Path::from_path(td.path())
        .expect("utf-8 tempdir")
        .join("mcp-search-ambig.jsonl");

    let env = EnvGuard::new(ENV_KEYS);
    env.set("DOIGET_OPENALEX_BASE", &server.uri());
    env.set("DOIGET_LOG_PATH", log_path.as_str());

    let (client, server_handle) = boot_in_memory_server().await?;

    let mut args = serde_json::Map::new();
    args.insert("query".to_string(), serde_json::json!("electrons"));
    args.insert("author".to_string(), serde_json::json!("Smith"));
    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_paper_search").with_arguments(args))
        .await?;
    let s = result
        .structured_content
        .as_ref()
        .expect("doiget_paper_search uses CallToolResult::structured");

    assert_eq!(s["ok"], serde_json::json!(false), "envelope: {s:?}");
    assert_eq!(s["error"]["code"], serde_json::json!("AMBIGUOUS"));
    assert!(
        s["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("John Smith"),
        "ambiguity message should list candidates: {s:?}"
    );

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    drop(td);
    Ok(())
}
