//! Slice 15 — e2e for `doiget_expand_citation_graph` MCP tool.
//!
//! Whole file gated by `#[cfg(feature = "citation")]` because the
//! tool body is a NOT_IMPLEMENTED stub without the feature.
//
// allow: outbound-network

#![cfg(feature = "citation")]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use doiget_core::CapabilityProfile;
use doiget_mcp::Server;
use rmcp::{model::CallToolRequestParams, ServiceExt};

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
    "DOIGET_ENABLE_OPENALEX",
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

// Synthetic OpenAlex Work fixtures.
const SEED_WORK: &str = r#"{
    "id": "https://openalex.org/W0001",
    "doi": "https://doi.org/10.1234/seed",
    "display_name": "Seed Paper",
    "referenced_works": [
        "https://openalex.org/W0002",
        "https://openalex.org/W0003"
    ]
}"#;
const LEAF_W0002: &str = r#"{"id":"https://openalex.org/W0002","referenced_works":[]}"#;
const LEAF_W0003: &str = r#"{"id":"https://openalex.org/W0003","referenced_works":[]}"#;

#[tokio::test]
#[serial_test::serial]
async fn expand_citation_graph_returns_graph_envelope() -> anyhow::Result<()> {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/works/doi:10.1234/seed"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SEED_WORK))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/works/W0002"))
        .respond_with(ResponseTemplate::new(200).set_body_string(LEAF_W0002))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/works/W0003"))
        .respond_with(ResponseTemplate::new(200).set_body_string(LEAF_W0003))
        .mount(&server)
        .await;

    let td = tempfile::TempDir::new().expect("tempdir");
    let log_path = camino::Utf8Path::from_path(td.path())
        .expect("utf-8 tempdir")
        .join("mcp-graph.jsonl");

    let env = EnvGuard::new(ENV_KEYS);
    env.set("DOIGET_OPENALEX_BASE", &server.uri());
    env.set("DOIGET_LOG_PATH", log_path.as_str());
    env.set("DOIGET_ENABLE_OPENALEX", "1");

    let (client, server_handle) = boot_in_memory_server().await?;

    let mut args = serde_json::Map::new();
    args.insert("ref".to_string(), serde_json::json!("10.1234/seed"));
    args.insert("depth".to_string(), serde_json::json!(2));

    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_expand_citation_graph").with_arguments(args))
        .await?;
    let structured = result
        .structured_content
        .as_ref()
        .expect("doiget_expand_citation_graph uses CallToolResult::structured");

    assert_eq!(
        structured["ok"],
        serde_json::json!(true),
        "envelope: {structured:?}"
    );
    assert_eq!(structured["ref"], serde_json::json!("10.1234/seed"));
    assert_eq!(structured["seed_work_id"], serde_json::json!("W0001"));
    assert_eq!(structured["total_visited"], serde_json::json!(3));
    assert_eq!(
        structured["nodes"].as_array().expect("nodes array").len(),
        3
    );
    assert_eq!(
        structured["edges"].as_array().expect("edges array").len(),
        2
    );

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    drop(td);
    Ok(())
}

#[tokio::test]
async fn expand_citation_graph_invalid_ref_returns_invalid_ref_envelope() -> anyhow::Result<()> {
    let (client, server_handle) = boot_in_memory_server().await?;
    let mut args = serde_json::Map::new();
    args.insert("ref".to_string(), serde_json::json!("not a doi"));

    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_expand_citation_graph").with_arguments(args))
        .await?;
    let structured = result
        .structured_content
        .as_ref()
        .expect("doiget_expand_citation_graph uses CallToolResult::structured");
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
async fn expand_citation_graph_arxiv_seed_rejected() -> anyhow::Result<()> {
    let (client, server_handle) = boot_in_memory_server().await?;
    let mut args = serde_json::Map::new();
    args.insert("ref".to_string(), serde_json::json!("2401.12345"));

    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_expand_citation_graph").with_arguments(args))
        .await?;
    let structured = result
        .structured_content
        .as_ref()
        .expect("doiget_expand_citation_graph uses CallToolResult::structured");
    assert_eq!(structured["ok"], serde_json::json!(false));
    assert_eq!(
        structured["error"]["code"],
        serde_json::json!("INVALID_REF")
    );

    client.cancel().await?;
    server_handle.await??;
    Ok(())
}
