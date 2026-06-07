//! End-to-end coverage for the `doiget_link` MCP tool (DOI → arXiv
//! preprint linking; #281 item 5). Not feature-gated — Tier-1, always-on.
//!
//! All HTTP terminates at a `wiremock::MockServer` reached via
//! `DOIGET_OPENALEX_BASE`; no outbound network.
// allow: outbound-network

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use doiget_core::CapabilityProfile;
use doiget_mcp::Server;
use rmcp::{model::CallToolRequestParams, ServiceExt};
use wiremock::matchers::{method, path, query_param};
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

const SAMPLE_WORK: &str = r#"{
    "meta": { "count": 1 },
    "results": [ {
        "id": "https://openalex.org/W55",
        "doi": "https://doi.org/10.1103/PhysRevB.1",
        "title": "Published Version",
        "locations": [
            { "landing_page_url": "https://journals.aps.org/prb/abstract/x" },
            { "pdf_url": "https://arxiv.org/abs/2101.54321v2" }
        ]
    } ]
}"#;

fn log_path(td: &tempfile::TempDir, name: &str) -> camino::Utf8PathBuf {
    camino::Utf8Path::from_path(td.path())
        .expect("utf-8 tempdir")
        .join(name)
}

#[tokio::test]
#[serial_test::serial]
async fn link_resolves_doi_to_arxiv() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/works"))
        .and(query_param("filter", "doi:10.1103/physrevb.1"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_WORK))
        .mount(&server)
        .await;

    let td = tempfile::TempDir::new().expect("tempdir");
    let env = EnvGuard::new(ENV_KEYS);
    env.set("DOIGET_OPENALEX_BASE", &server.uri());
    env.set("DOIGET_LOG_PATH", log_path(&td, "mcp-link.jsonl").as_str());

    let (client, server_handle) = boot_in_memory_server().await?;

    let mut args = serde_json::Map::new();
    args.insert("ref".to_string(), serde_json::json!("10.1103/physrevb.1"));
    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_link").with_arguments(args))
        .await?;
    let s = result
        .structured_content
        .as_ref()
        .expect("doiget_link uses CallToolResult::structured");

    assert_eq!(s["ok"], serde_json::json!(true), "envelope: {s:?}");
    assert_eq!(s["arxiv"], serde_json::json!("2101.54321v2"));
    assert_eq!(s["openalex_id"], serde_json::json!("W55"));
    assert_eq!(s["title"], serde_json::json!("Published Version"));
    // The `doi` field echoes OpenAlex's recorded DOI (lower-cased).
    assert_eq!(s["doi"], serde_json::json!("10.1103/physrevb.1"));

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    drop(td);
    Ok(())
}

/// A work with only a journal landing page — NO arXiv location. This is the
/// dedup-negative outcome the tool exists to report.
const SAMPLE_WORK_NO_PREPRINT: &str = r#"{
    "meta": { "count": 1 },
    "results": [ {
        "id": "https://openalex.org/W7",
        "doi": "https://doi.org/10.1234/closed",
        "title": "No Preprint",
        "locations": [ { "landing_page_url": "https://example.com/article" } ]
    } ]
}"#;

#[tokio::test]
#[serial_test::serial]
async fn link_doi_without_preprint_returns_null_arxiv() -> anyhow::Result<()> {
    // The core reason the tool exists: report "this DOI has no free arXiv
    // preprint" as `ok:true, arxiv:null` (a dedup negative), not an error.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/works"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_WORK_NO_PREPRINT))
        .mount(&server)
        .await;

    let td = tempfile::TempDir::new().expect("tempdir");
    let env = EnvGuard::new(ENV_KEYS);
    env.set("DOIGET_OPENALEX_BASE", &server.uri());
    env.set(
        "DOIGET_LOG_PATH",
        log_path(&td, "mcp-link-np.jsonl").as_str(),
    );

    let (client, server_handle) = boot_in_memory_server().await?;

    let mut args = serde_json::Map::new();
    args.insert("ref".to_string(), serde_json::json!("10.1234/closed"));
    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_link").with_arguments(args))
        .await?;
    let s = result.structured_content.as_ref().expect("structured");

    assert_eq!(s["ok"], serde_json::json!(true), "envelope: {s:?}");
    assert_eq!(
        s["arxiv"],
        serde_json::Value::Null,
        "no preprint must surface as arxiv:null, not an error"
    );
    assert_eq!(s["openalex_id"], serde_json::json!("W7"));

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    drop(td);
    Ok(())
}

#[tokio::test]
#[serial_test::serial]
async fn link_arxiv_input_maps_to_invalid_ref() -> anyhow::Result<()> {
    // arXiv → DOI is a follow-up; an arXiv ref is the wrong direction for
    // this tool and must surface as INVALID_REF, no network touched.
    let env = EnvGuard::new(ENV_KEYS);

    let (client, server_handle) = boot_in_memory_server().await?;

    let mut args = serde_json::Map::new();
    args.insert("ref".to_string(), serde_json::json!("arxiv:2401.12345"));
    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_link").with_arguments(args))
        .await?;
    let s = result.structured_content.as_ref().expect("structured");

    assert_eq!(s["ok"], serde_json::json!(false), "envelope: {s:?}");
    assert_eq!(s["error"]["code"], serde_json::json!("INVALID_REF"));

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    Ok(())
}

#[tokio::test]
#[serial_test::serial]
async fn link_unknown_doi_maps_to_not_found() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/works"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"{ "meta": { "count": 0 }, "results": [] }"#),
        )
        .mount(&server)
        .await;

    let td = tempfile::TempDir::new().expect("tempdir");
    let env = EnvGuard::new(ENV_KEYS);
    env.set("DOIGET_OPENALEX_BASE", &server.uri());
    env.set(
        "DOIGET_LOG_PATH",
        log_path(&td, "mcp-link-nf.jsonl").as_str(),
    );

    let (client, server_handle) = boot_in_memory_server().await?;

    let mut args = serde_json::Map::new();
    args.insert("ref".to_string(), serde_json::json!("10.0000/nope"));
    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_link").with_arguments(args))
        .await?;
    let s = result.structured_content.as_ref().expect("structured");

    assert_eq!(s["ok"], serde_json::json!(false), "envelope: {s:?}");
    assert_eq!(s["error"]["code"], serde_json::json!("NOT_FOUND"));

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    drop(td);
    Ok(())
}
