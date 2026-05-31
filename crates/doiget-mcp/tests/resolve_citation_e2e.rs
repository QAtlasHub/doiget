//! E2E coverage for `doiget_resolve_citation` and `doiget_batch_resolve_citations`.

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
    "DOIGET_CROSSREF_BASE",
    "DOIGET_CONTACT_EMAIL",
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

#[tokio::test]
#[serial_test::serial]
async fn test_doiget_resolve_citation_e2e() -> anyhow::Result<()> {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let mock_body = serde_json::json!({
        "status": "ok",
        "message": {
            "items": [
                {
                    "DOI": "10.1000/xyz123",
                    "title": ["Lars Onsager, Crystal Statistics. I. A Two-Dimensional Model with an Order-Disorder Transition"],
                    "author": [
                        {"family": "Onsager", "given": "Lars"}
                    ],
                    "issued": {
                        "date-parts": [[1944, 2, 1]]
                    },
                    "container-title": ["Physical Review"]
                }
            ]
        }
    });

    Mock::given(method("GET"))
        .and(path("/works"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_body))
        .mount(&server)
        .await;

    let td = tempfile::TempDir::new().expect("tempdir");
    let log_path = camino::Utf8Path::from_path(td.path())
        .expect("tempdir is utf-8")
        .join("mcp-resolve-citation.jsonl");

    let env = EnvGuard::new(ENV_KEYS);
    env.set("DOIGET_CROSSREF_BASE", &server.uri());
    env.set("DOIGET_LOG_PATH", log_path.as_str());
    env.set("DOIGET_CONTACT_EMAIL", "test@example.org");
    env.set(
        "DOIGET_STORE_ROOT",
        td.path().to_str().expect("utf-8 tempdir"),
    );

    let (client, server_handle) = boot_in_memory_server().await?;

    let mut args = serde_json::Map::new();
    args.insert("query".to_string(), serde_json::json!("Onsager 1944"));

    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_resolve_citation").with_arguments(args))
        .await?;
    let structured = result
        .structured_content
        .as_ref()
        .expect("doiget_resolve_citation uses CallToolResult::structured");

    assert_eq!(structured["ok"], serde_json::json!(true));
    assert_eq!(structured["query"], serde_json::json!("Onsager 1944"));
    let candidates = &structured["candidates"];
    assert_eq!(candidates.as_array().unwrap().len(), 1);
    assert_eq!(candidates[0]["doi"], "10.1000/xyz123");
    assert_eq!(candidates[0]["score"], 1.0);

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    drop(td);
    Ok(())
}

#[tokio::test]
#[serial_test::serial]
async fn test_doiget_batch_resolve_citations_e2e() -> anyhow::Result<()> {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let mock_body = serde_json::json!({
        "status": "ok",
        "message": {
            "items": [
                {
                    "DOI": "10.1000/xyz123",
                    "title": ["Lars Onsager, Crystal Statistics. I. A Two-Dimensional Model with an Order-Disorder Transition"],
                    "author": [
                        {"family": "Onsager", "given": "Lars"}
                    ],
                    "issued": {
                        "date-parts": [[1944, 2, 1]]
                    },
                    "container-title": ["Physical Review"]
                }
            ]
        }
    });

    Mock::given(method("GET"))
        .and(path("/works"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mock_body))
        .mount(&server)
        .await;

    let td = tempfile::TempDir::new().expect("tempdir");
    let log_path = camino::Utf8Path::from_path(td.path())
        .expect("tempdir is utf-8")
        .join("mcp-batch-resolve-citation.jsonl");

    let env = EnvGuard::new(ENV_KEYS);
    env.set("DOIGET_CROSSREF_BASE", &server.uri());
    env.set("DOIGET_LOG_PATH", log_path.as_str());
    env.set("DOIGET_CONTACT_EMAIL", "test@example.org");
    env.set(
        "DOIGET_STORE_ROOT",
        td.path().to_str().expect("utf-8 tempdir"),
    );

    let (client, server_handle) = boot_in_memory_server().await?;

    let mut args = serde_json::Map::new();
    args.insert("queries".to_string(), serde_json::json!(["Onsager 1944"]));

    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_batch_resolve_citations").with_arguments(args))
        .await?;
    let structured = result
        .structured_content
        .as_ref()
        .expect("doiget_batch_resolve_citations uses CallToolResult::structured");

    assert_eq!(structured["ok"], serde_json::json!(true));
    let results = &structured["results"];
    assert_eq!(results.as_array().unwrap().len(), 1);
    assert_eq!(results[0]["query"], serde_json::json!("Onsager 1944"));
    let candidates = &results[0]["candidates"];
    assert_eq!(candidates.as_array().unwrap().len(), 1);
    assert_eq!(candidates[0]["doi"], "10.1000/xyz123");

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    drop(td);
    Ok(())
}
