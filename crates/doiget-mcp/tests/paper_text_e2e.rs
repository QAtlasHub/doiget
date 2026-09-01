//! End-to-end coverage for the `doiget_paper_text` MCP tool (full-text
//! extraction from ar5iv; ADR-0032). Not feature-gated — full-text
//! extraction is Tier-1, always-on.
//!
//! All HTTP terminates at a `wiremock::MockServer` reached via
//! `DOIGET_AR5IV_BASE`; no outbound network.
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

const ENV_KEYS: &[&str] = &["DOIGET_AR5IV_BASE", "DOIGET_LOG_PATH"];

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

const SAMPLE_AR5IV: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml">
<head><title>Tropical Tensor Networks</title></head>
<body>
  <p>Lead matter on spin glasses.</p>
  <section><h2>1 Introduction</h2><p>Intro body here.</p></section>
</body>
</html>"#;

fn log_path(td: &tempfile::TempDir, name: &str) -> camino::Utf8PathBuf {
    camino::Utf8Path::from_path(td.path())
        .expect("utf-8 tempdir")
        .join(name)
}

#[tokio::test]
#[serial_test::serial]
async fn paper_text_returns_sectioned_envelope() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/html/2401.12345"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_AR5IV))
        .mount(&server)
        .await;

    let td = tempfile::TempDir::new().expect("tempdir");
    let log = log_path(&td, "mcp-text.jsonl");
    let env = EnvGuard::new(ENV_KEYS);
    env.set("DOIGET_AR5IV_BASE", &server.uri());
    env.set("DOIGET_LOG_PATH", log.as_str());

    let (client, server_handle) = boot_in_memory_server().await?;

    let mut args = serde_json::Map::new();
    args.insert("ref".to_string(), serde_json::json!("arxiv:2401.12345"));
    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_paper_text").with_arguments(args))
        .await?;
    let s = result
        .structured_content
        .as_ref()
        .expect("doiget_paper_text uses CallToolResult::structured");

    assert_eq!(s["ok"], serde_json::json!(true), "envelope: {s:?}");
    assert_eq!(s["arxiv_id"], serde_json::json!("2401.12345"));
    assert_eq!(s["source"], serde_json::json!("ar5iv"));
    assert_eq!(s["title"], serde_json::json!("Tropical Tensor Networks"));
    assert_eq!(s["truncated"], serde_json::json!(false));
    // Lead section (no heading) + one headed section.
    assert_eq!(s["sections"].as_array().expect("sections array").len(), 2);
    assert_eq!(
        s["sections"][1]["heading"],
        serde_json::json!("1 Introduction")
    );

    // Provenance bookends: SessionStart, one ar5iv Fetch row, SessionEnd —
    // the reason the tool builds a FetchContext at all (a stated deliverable).
    // `LogEvent` serializes snake_case.
    let prov = std::fs::read_to_string(log.as_std_path()).expect("read provenance log");
    assert!(
        prov.contains("\"event\":\"session_start\""),
        "missing session_start in:\n{prov}"
    );
    assert!(
        prov.contains("\"event\":\"fetch\"") && prov.contains("\"source\":\"ar5iv\""),
        "missing ar5iv fetch row in:\n{prov}"
    );
    assert!(
        prov.contains("\"event\":\"session_end\""),
        "missing session_end in:\n{prov}"
    );

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    drop(td);
    Ok(())
}

#[tokio::test]
#[serial_test::serial]
async fn paper_text_max_chars_truncates() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/html/2401.12345"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_AR5IV))
        .mount(&server)
        .await;

    let td = tempfile::TempDir::new().expect("tempdir");
    let env = EnvGuard::new(ENV_KEYS);
    env.set("DOIGET_AR5IV_BASE", &server.uri());
    env.set(
        "DOIGET_LOG_PATH",
        log_path(&td, "mcp-text-trunc.jsonl").as_str(),
    );

    let (client, server_handle) = boot_in_memory_server().await?;

    let mut args = serde_json::Map::new();
    args.insert("ref".to_string(), serde_json::json!("arxiv:2401.12345"));
    args.insert("max_chars".to_string(), serde_json::json!(10));
    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_paper_text").with_arguments(args))
        .await?;
    let s = result.structured_content.as_ref().expect("structured");

    assert_eq!(s["ok"], serde_json::json!(true), "envelope: {s:?}");
    assert_eq!(s["truncated"], serde_json::json!(true));
    assert_eq!(s["char_count"], serde_json::json!(10));

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    drop(td);
    Ok(())
}

#[tokio::test]
#[serial_test::serial]
async fn paper_text_doi_is_terminal_not_a_config_problem() -> anyhow::Result<()> {
    // A DOI has no full-text source in this slice (ADR-0032 D5). The code says
    // WHICH kind of "no": `NO_OA_AVAILABLE` carries disposition `needs_config`
    // -- "a named change makes it" -- and sends an agent looking for a config
    // knob that does not exist, because the missing piece is DOI-to-arXiv
    // linking (#281 item 5), not a grant. `NOT_IMPLEMENTED` is terminal and
    // is the code `verify` / `batch_from_bibliography` already give the same
    // situation for PMIDs (#500): valid input, absent support.
    // No network touched either way.
    let env = EnvGuard::new(ENV_KEYS);

    let (client, server_handle) = boot_in_memory_server().await?;

    let mut args = serde_json::Map::new();
    args.insert("ref".to_string(), serde_json::json!("10.1234/example"));
    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_paper_text").with_arguments(args))
        .await?;
    let s = result.structured_content.as_ref().expect("structured");

    assert_eq!(s["ok"], serde_json::json!(false), "envelope: {s:?}");
    assert_eq!(s["error"]["code"], serde_json::json!("NOT_IMPLEMENTED"));
    // The code is only half the answer; the disposition is what an agent
    // branches on, and it is the half that was wrong.
    assert_eq!(s["error"]["disposition"], serde_json::json!("terminal"));
    // MCP_TOOLS.md §5: an ok:false envelope echoes the input `ref`.
    assert_eq!(s["ref"], serde_json::json!("10.1234/example"));

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    Ok(())
}

#[tokio::test]
#[serial_test::serial]
async fn paper_text_unconverted_paper_maps_to_text_unavailable() -> anyhow::Result<()> {
    // ar5iv returns a 200 with no extractable text → TEXT_UNAVAILABLE (the
    // paper is not converted to HTML), distinct from both a transport error
    // and a bad identifier. The MCP `text` tool must surface this so an
    // agent fetches the PDF instead of misreading it as a wrong DOI (#302).
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/html/2401.99999"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string("<html><head></head><body></body></html>"),
        )
        .mount(&server)
        .await;

    let td = tempfile::TempDir::new().expect("tempdir");
    let env = EnvGuard::new(ENV_KEYS);
    env.set("DOIGET_AR5IV_BASE", &server.uri());
    env.set(
        "DOIGET_LOG_PATH",
        log_path(&td, "mcp-text-nf.jsonl").as_str(),
    );

    let (client, server_handle) = boot_in_memory_server().await?;

    let mut args = serde_json::Map::new();
    args.insert("ref".to_string(), serde_json::json!("arxiv:2401.99999"));
    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_paper_text").with_arguments(args))
        .await?;
    let s = result.structured_content.as_ref().expect("structured");

    assert_eq!(s["ok"], serde_json::json!(false), "envelope: {s:?}");
    assert_eq!(s["error"]["code"], serde_json::json!("TEXT_UNAVAILABLE"));

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    drop(td);
    Ok(())
}
