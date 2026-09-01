//! E2E coverage for `doiget_tag` and `doiget_annotate`.
//!
//! These two tools had **no test of any kind** — not an error path, not even a
//! success path. That is how a wrong error code got into them and out again:
//! this release gave every one of their failure arms a structured `error`
//! object for the first time, and picked `NOT_FOUND` for "the ref is not in
//! the store yet". `docs/ERRORS.md` defines that code as *a metadata source
//! authoritatively reported the id does not exist*, which `doiget verify`
//! treats as a definite dead reference — so the tools would have told an agent
//! a perfectly good DOI was retracted. Nothing in the suite could notice.
//!
//! Every assertion below drives the real MCP tool over a real transport.

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

const ENV_KEYS: &[&str] = &["DOIGET_STORE_ROOT", "DOIGET_LOG_PATH"];

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

fn args(pairs: &[(&str, serde_json::Value)]) -> serde_json::Map<String, serde_json::Value> {
    let mut m = serde_json::Map::new();
    for (k, v) in pairs {
        m.insert((*k).to_string(), v.clone());
    }
    m
}

/// A ref nobody has fetched is not a dead reference.
///
/// The code must not be `NOT_FOUND`: `docs/ERRORS.md` reserves that for "a
/// metadata source authoritatively reported the id does not exist", and an
/// agent acting on it would drop a citation that is fine. The remedy here is
/// an action the caller can take, which is what `needs_config` means.
#[tokio::test]
#[serial_test::serial]
async fn tag_on_an_unfetched_ref_does_not_call_it_a_dead_reference() -> anyhow::Result<()> {
    let td = tempfile::TempDir::new().expect("tempdir");
    let root = camino::Utf8Path::from_path(td.path()).expect("utf-8 tempdir");

    let env = EnvGuard::new(ENV_KEYS);
    env.set("DOIGET_STORE_ROOT", root.join("papers").as_str());
    env.set("DOIGET_LOG_PATH", root.join("log.jsonl").as_str());

    let (client, server_handle) = boot_in_memory_server().await?;
    let result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("doiget_tag").with_arguments(args(&[
                ("ref", serde_json::json!("10.1234/never-fetched")),
                ("add", serde_json::json!(["to-read"])),
            ])),
        )
        .await?;
    let s = result.structured_content.as_ref().expect("structured");

    assert_eq!(s["ok"], serde_json::json!(false), "envelope: {s:?}");
    assert_ne!(
        s["error"]["code"],
        serde_json::json!("NOT_FOUND"),
        "a ref that has not been fetched is not a dead reference: {s:?}"
    );
    assert_eq!(
        s["error"]["code"],
        serde_json::json!("STORE_ERROR"),
        "{s:?}"
    );
    assert_eq!(
        s["error"]["disposition"],
        serde_json::json!("needs_config"),
        "the remedy is a named action (fetch it), not a retry: {s:?}"
    );
    let message = s["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("fetch"),
        "the message names the action: {message:?}"
    );

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    drop(td);
    Ok(())
}

/// Same rule on the sibling tool, which had the same code and the same
/// absence of tests.
#[tokio::test]
#[serial_test::serial]
async fn annotate_on_an_unfetched_ref_does_not_call_it_a_dead_reference() -> anyhow::Result<()> {
    let td = tempfile::TempDir::new().expect("tempdir");
    let root = camino::Utf8Path::from_path(td.path()).expect("utf-8 tempdir");

    let env = EnvGuard::new(ENV_KEYS);
    env.set("DOIGET_STORE_ROOT", root.join("papers").as_str());
    env.set("DOIGET_LOG_PATH", root.join("log.jsonl").as_str());

    let (client, server_handle) = boot_in_memory_server().await?;
    let result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("doiget_annotate").with_arguments(args(&[
                ("ref", serde_json::json!("10.1234/never-fetched")),
                ("text", serde_json::json!("read the appendix")),
            ])),
        )
        .await?;
    let s = result.structured_content.as_ref().expect("structured");

    assert_eq!(s["ok"], serde_json::json!(false), "envelope: {s:?}");
    assert_ne!(s["error"]["code"], serde_json::json!("NOT_FOUND"), "{s:?}");
    assert_eq!(
        s["error"]["code"],
        serde_json::json!("STORE_ERROR"),
        "{s:?}"
    );
    assert_eq!(
        s["error"]["disposition"],
        serde_json::json!("needs_config"),
        "{s:?}"
    );

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    drop(td);
    Ok(())
}

/// A malformed ref is the caller's input problem, and it must stay
/// distinguishable from the store miss above — same tool, two situations, two
/// codes. ADR-0055 exists so an agent can branch here.
#[tokio::test]
#[serial_test::serial]
async fn tag_on_a_malformed_ref_is_invalid_ref_not_a_store_problem() -> anyhow::Result<()> {
    let td = tempfile::TempDir::new().expect("tempdir");
    let root = camino::Utf8Path::from_path(td.path()).expect("utf-8 tempdir");

    let env = EnvGuard::new(ENV_KEYS);
    env.set("DOIGET_STORE_ROOT", root.join("papers").as_str());
    env.set("DOIGET_LOG_PATH", root.join("log.jsonl").as_str());

    let (client, server_handle) = boot_in_memory_server().await?;
    let result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("doiget_tag").with_arguments(args(&[
                ("ref", serde_json::json!("not-a-doi")),
                ("add", serde_json::json!(["x"])),
            ])),
        )
        .await?;
    let s = result.structured_content.as_ref().expect("structured");

    assert_eq!(s["ok"], serde_json::json!(false), "envelope: {s:?}");
    assert_eq!(
        s["error"]["code"],
        serde_json::json!("INVALID_REF"),
        "{s:?}"
    );
    assert_eq!(
        s["error"]["disposition"],
        serde_json::json!("terminal"),
        "{s:?}"
    );

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    drop(td);
    Ok(())
}

/// `doiget_annotate` with neither `text` nor `clear` is a request-shape
/// failure. Before this release it answered with a bare string in `error`, so
/// a caller had no code to branch on at all.
#[tokio::test]
#[serial_test::serial]
async fn annotate_with_no_text_and_no_clear_says_so_in_a_structured_error() -> anyhow::Result<()> {
    let td = tempfile::TempDir::new().expect("tempdir");
    let root = camino::Utf8Path::from_path(td.path()).expect("utf-8 tempdir");

    let env = EnvGuard::new(ENV_KEYS);
    env.set("DOIGET_STORE_ROOT", root.join("papers").as_str());
    env.set("DOIGET_LOG_PATH", root.join("log.jsonl").as_str());

    let (client, server_handle) = boot_in_memory_server().await?;
    let result = client
        .peer()
        .call_tool(
            CallToolRequestParams::new("doiget_annotate")
                .with_arguments(args(&[("ref", serde_json::json!("10.1234/never-fetched"))])),
        )
        .await?;
    let s = result.structured_content.as_ref().expect("structured");

    assert_eq!(s["ok"], serde_json::json!(false), "envelope: {s:?}");
    assert!(
        s["error"].is_object(),
        "an ok:false envelope carries an error OBJECT, not a bare string (ADR-0055): {s:?}"
    );
    assert!(
        s["error"]["disposition"].is_string(),
        "every failure envelope carries a disposition: {s:?}"
    );

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    drop(td);
    Ok(())
}
