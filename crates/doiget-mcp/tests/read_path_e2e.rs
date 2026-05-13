//! Slice 8 — end-to-end coverage for the 4 read-path MCP tools:
//! `doiget_info`, `doiget_search_local`, `doiget_list_recent`,
//! `doiget_paper_pdf_path`.
//!
//! These tools are 100% local: no network, no provenance row, just
//! reads through the `Store` trait. The tests below exercise the
//! "no entry / empty store" branches because they do not require
//! pre-populating the store with a hand-crafted metadata TOML —
//! that path is covered by `crates/doiget-cli/tests/*` and the
//! roundtrip e2e suite. Pre-populated-store cases are tracked for a
//! follow-up slice.
//!
//! ## Network purity
//!
//! No `reqwest::*` items are imported; no HTTP traffic of any kind.
//! The escape-hatch comment below covers the future addition of any
//! `reqwest::*` import without further intervention.
// allow: outbound-network

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

const ENV_KEYS: &[&str] = &["DOIGET_STORE_ROOT"];

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

fn temp_store_root() -> (tempfile::TempDir, camino::Utf8PathBuf) {
    let td = tempfile::TempDir::new().expect("tempdir");
    let root = camino::Utf8Path::from_path(td.path())
        .expect("tempdir is utf-8")
        .to_path_buf();
    // FsStore::new() will create the directory if missing, but we also
    // need the `.metadata/` subdirectory to exist for list_recent /
    // search to succeed against an empty store rather than erroring on
    // a missing dir.
    std::fs::create_dir_all(root.join(".metadata")).expect("create .metadata");
    (td, root)
}

// ---------------------------------------------------------------------------
// doiget_info
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial_test::serial]
async fn doiget_info_invalid_ref_returns_invalid_ref_envelope() -> anyhow::Result<()> {
    let (_td, root) = temp_store_root();
    let env = EnvGuard::new(ENV_KEYS);
    env.set("DOIGET_STORE_ROOT", root.as_str());

    let (client, server_handle) = boot_in_memory_server().await?;

    let mut args = serde_json::Map::new();
    args.insert("ref".to_string(), serde_json::json!("not a doi"));

    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_info").with_arguments(args))
        .await?;
    let structured = result
        .structured_content
        .as_ref()
        .expect("doiget_info uses CallToolResult::structured");
    assert_eq!(structured["ok"], serde_json::json!(false));
    assert_eq!(structured["error"]["code"], serde_json::json!("INVALID_REF"));

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    Ok(())
}

#[tokio::test]
#[serial_test::serial]
async fn doiget_info_no_entry_returns_null_metadata() -> anyhow::Result<()> {
    let (_td, root) = temp_store_root();
    let env = EnvGuard::new(ENV_KEYS);
    env.set("DOIGET_STORE_ROOT", root.as_str());

    let (client, server_handle) = boot_in_memory_server().await?;

    let mut args = serde_json::Map::new();
    args.insert("ref".to_string(), serde_json::json!("10.1234/example"));

    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_info").with_arguments(args))
        .await?;
    let structured = result
        .structured_content
        .as_ref()
        .expect("doiget_info uses CallToolResult::structured");
    assert_eq!(structured["ok"], serde_json::json!(true));
    assert_eq!(structured["ref"], serde_json::json!("10.1234/example"));
    assert!(structured["safekey"].is_string(), "envelope: {structured:?}");
    assert_eq!(
        structured["metadata"],
        serde_json::Value::Null,
        "metadata must be null when no entry exists; got: {structured:?}"
    );

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    Ok(())
}

// ---------------------------------------------------------------------------
// doiget_search_local
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial_test::serial]
async fn doiget_search_local_empty_store_returns_empty_entries() -> anyhow::Result<()> {
    let (_td, root) = temp_store_root();
    let env = EnvGuard::new(ENV_KEYS);
    env.set("DOIGET_STORE_ROOT", root.as_str());

    let (client, server_handle) = boot_in_memory_server().await?;

    let mut args = serde_json::Map::new();
    args.insert("query".to_string(), serde_json::json!("anything"));

    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_search_local").with_arguments(args))
        .await?;
    let structured = result
        .structured_content
        .as_ref()
        .expect("doiget_search_local uses CallToolResult::structured");
    assert_eq!(structured["ok"], serde_json::json!(true));
    assert_eq!(structured["query"], serde_json::json!("anything"));
    assert_eq!(
        structured["entries"],
        serde_json::json!([]),
        "empty store must return an empty entries array; got: {structured:?}"
    );

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    Ok(())
}

// ---------------------------------------------------------------------------
// doiget_list_recent
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial_test::serial]
async fn doiget_list_recent_empty_store_returns_empty_entries() -> anyhow::Result<()> {
    let (_td, root) = temp_store_root();
    let env = EnvGuard::new(ENV_KEYS);
    env.set("DOIGET_STORE_ROOT", root.as_str());

    let (client, server_handle) = boot_in_memory_server().await?;

    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_list_recent"))
        .await?;
    let structured = result
        .structured_content
        .as_ref()
        .expect("doiget_list_recent uses CallToolResult::structured");
    assert_eq!(structured["ok"], serde_json::json!(true));
    assert_eq!(
        structured["entries"],
        serde_json::json!([]),
        "empty store must return an empty entries array; got: {structured:?}"
    );

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    Ok(())
}

// ---------------------------------------------------------------------------
// doiget_paper_pdf_path
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial_test::serial]
async fn doiget_paper_pdf_path_invalid_ref_returns_invalid_ref_envelope() -> anyhow::Result<()> {
    let (_td, root) = temp_store_root();
    let env = EnvGuard::new(ENV_KEYS);
    env.set("DOIGET_STORE_ROOT", root.as_str());

    let (client, server_handle) = boot_in_memory_server().await?;

    let mut args = serde_json::Map::new();
    args.insert("ref".to_string(), serde_json::json!("not a doi"));

    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_paper_pdf_path").with_arguments(args))
        .await?;
    let structured = result
        .structured_content
        .as_ref()
        .expect("doiget_paper_pdf_path uses CallToolResult::structured");
    assert_eq!(structured["ok"], serde_json::json!(false));
    assert_eq!(structured["error"]["code"], serde_json::json!("INVALID_REF"));

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    Ok(())
}

#[tokio::test]
#[serial_test::serial]
async fn doiget_paper_pdf_path_no_entry_returns_null_path() -> anyhow::Result<()> {
    let (_td, root) = temp_store_root();
    let env = EnvGuard::new(ENV_KEYS);
    env.set("DOIGET_STORE_ROOT", root.as_str());

    let (client, server_handle) = boot_in_memory_server().await?;

    let mut args = serde_json::Map::new();
    args.insert("ref".to_string(), serde_json::json!("10.1234/example"));

    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_paper_pdf_path").with_arguments(args))
        .await?;
    let structured = result
        .structured_content
        .as_ref()
        .expect("doiget_paper_pdf_path uses CallToolResult::structured");
    assert_eq!(structured["ok"], serde_json::json!(true));
    assert_eq!(structured["ref"], serde_json::json!("10.1234/example"));
    assert!(structured["safekey"].is_string(), "envelope: {structured:?}");
    assert_eq!(
        structured["path"],
        serde_json::Value::Null,
        "path must be null when no entry exists; got: {structured:?}"
    );
    assert_eq!(structured["pdf_exists"], serde_json::json!(false));

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    Ok(())
}

// ---------------------------------------------------------------------------
// deny_unknown_fields enforcement
// ---------------------------------------------------------------------------

/// Per `docs/MCP_TOOLS.md` §10, the 4 read-path tools are in the
/// "dry_run does not apply" set. All four `*Input` structs carry both
/// `#[serde(deny_unknown_fields)]` and `#[schemars(deny_unknown_fields)]`
/// so an attempted `dry_run` field is rejected at the deserialize
/// boundary regardless of whether rmcp validates against the
/// advertised JSON schema.
///
/// This test sends `{"ref": "10.1234/example", "dry_run": true}` to
/// `doiget_info` and asserts the call surfaces as an error — it MUST
/// NOT be silently accepted (which would happen if only the
/// schemars-side attribute were present and rmcp didn't pre-validate).
#[tokio::test]
#[serial_test::serial]
async fn doiget_info_dry_run_field_is_rejected() -> anyhow::Result<()> {
    let (_td, root) = temp_store_root();
    let env = EnvGuard::new(ENV_KEYS);
    env.set("DOIGET_STORE_ROOT", root.as_str());

    let (client, server_handle) = boot_in_memory_server().await?;

    let mut args = serde_json::Map::new();
    args.insert("ref".to_string(), serde_json::json!("10.1234/example"));
    args.insert("dry_run".to_string(), serde_json::json!(true));

    let outcome = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_info").with_arguments(args))
        .await;

    match outcome {
        Err(_) => {
            // Transport-level rejection — expected with
            // #[serde(deny_unknown_fields)] in place.
        }
        Ok(result) => {
            // Some MCP hosts surface deserialize errors as
            // is_error=true CallToolResult; that's also acceptable.
            // The thing that MUST NOT happen is a success envelope
            // with the dry_run field silently dropped.
            assert!(
                result.is_error.unwrap_or(false)
                    || result
                        .structured_content
                        .as_ref()
                        .map(|s| s["ok"] == serde_json::json!(false))
                        .unwrap_or(false),
                "dry_run field was silently accepted on doiget_info; \
                 deny_unknown_fields enforcement is missing. result: {result:?}"
            );
        }
    }

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    Ok(())
}
