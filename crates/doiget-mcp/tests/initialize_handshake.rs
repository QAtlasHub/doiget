//! In-process MCP smoke test.
//!
//! Drives the rmcp server side via a `tokio::io::duplex` pipe so the same
//! handshake the external `tests/mcp/smoke.py` exercises also runs inside
//! the standard `cargo test` matrix. Failures here are caught at PR review
//! by `ci.yml` long before the dedicated `mcp-smoke.yml` workflow runs.
//!
//! The flow asserted:
//!
//! 1. `initialize` — server identifies itself as `name = "doiget"` and
//!    populates `instructions`.
//! 2. `tools/list` — at minimum `doiget_health` and
//!    `doiget_capability_profile` are advertised.
//! 3. `tools/call doiget_health` — returns `{ ok: true, ... }` in
//!    `structuredContent`.
//! 4. `tools/call doiget_capability_profile` — returns the Tier-1 set.
//!
//! Per `docs/MCP_TOOLS.md` §9, the workflow-level smoke test additionally
//! asserts no stray bytes appear on stdout outside JSON-RPC frames; that
//! check belongs in the subprocess-based `tests/mcp/smoke.py` because the
//! in-process duplex pipe never exercises the real stdout path.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use doiget_core::CapabilityProfile;
use doiget_mcp::Server;
use rmcp::{model::CallToolRequestParams, ServiceExt};

#[tokio::test]
async fn initialize_tools_list_health_roundtrip() -> anyhow::Result<()> {
    // Build the server with a clean capability profile (Tier 1 only).
    let profile = CapabilityProfile::from_env().expect("clean env never errors");
    let server = Server::new(profile);

    // In-memory bidirectional pipe. 64 KiB is generous for the small
    // handshake frames this test sends.
    let (server_transport, client_transport) = tokio::io::duplex(64 * 1024);

    // Spawn the server side. `serve` consumes `self`; `waiting()` blocks
    // until the peer closes the transport (i.e., when we drop the client
    // side at the end of this test via `client.cancel()`).
    let server_handle = tokio::spawn(async move {
        let service = server.serve(server_transport).await?;
        service.waiting().await?;
        anyhow::Ok(())
    });

    // The default `()` client handler is sufficient to drive the
    // initialize handshake and call tools.
    let client = ().serve(client_transport).await?;

    // -- 1. initialize --------------------------------------------------
    //
    // `serve(...).await` runs initialize internally. The peer's
    // `ServerInfo` is exposed via `peer_info()`.
    let server_info = client
        .peer_info()
        .expect("server_info populated after initialize");
    assert_eq!(server_info.server_info.name, "doiget");
    assert!(
        !server_info.server_info.version.is_empty(),
        "server version must not be empty"
    );
    // `instructions` is set by `Server::get_info`. Assert it mentions
    // capability discovery so a future regression that nukes the field
    // is caught here rather than at `cargo doc` time.
    let instructions = server_info
        .instructions
        .as_deref()
        .expect("instructions set by get_info");
    assert!(
        instructions.contains("doiget_capability_profile"),
        "instructions must mention doiget_capability_profile (got: {instructions:?})"
    );

    // -- 2. tools/list --------------------------------------------------
    let tools = client.peer().list_all_tools().await?;
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    assert!(
        names.contains(&"doiget_health"),
        "tools/list must include doiget_health; got: {names:?}"
    );
    assert!(
        names.contains(&"doiget_capability_profile"),
        "tools/list must include doiget_capability_profile; got: {names:?}"
    );
    assert!(
        names.contains(&"doiget_metadata_only"),
        "tools/list must include doiget_metadata_only; got: {names:?}"
    );

    // -- 3. tools/call doiget_health -----------------------------------
    let health = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_health"))
        .await?;
    assert_ne!(
        health.is_error,
        Some(true),
        "doiget_health returned is_error=true; result: {health:?}"
    );
    let structured = health
        .structured_content
        .as_ref()
        .expect("doiget_health uses CallToolResult::structured");
    assert_eq!(structured["ok"], serde_json::json!(true));
    assert!(
        structured["version"].is_string(),
        "doiget_health.version must be a string; got: {structured:?}"
    );
    assert_eq!(structured["schema_version"], serde_json::json!("1.0"));
    assert!(
        structured["store_writable"].is_boolean(),
        "doiget_health.store_writable must be a bool; got: {structured:?}"
    );

    // -- 4. tools/call doiget_capability_profile ----------------------
    let cap = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_capability_profile"))
        .await?;
    let cap_struct = cap
        .structured_content
        .as_ref()
        .expect("doiget_capability_profile uses CallToolResult::structured");
    assert_eq!(cap_struct["ok"], serde_json::json!(true));
    assert_eq!(cap_struct["oa_enabled"], serde_json::json!(true));
    assert_eq!(
        cap_struct["tier_1"],
        serde_json::json!(["arxiv", "crossref", "unpaywall"])
    );
    assert_eq!(cap_struct["rate_limit_per_sec"], serde_json::json!(5.0));

    // -- Shutdown -------------------------------------------------------
    //
    // Cancel the client; this closes the transport and the server's
    // `waiting()` future then resolves.
    client.cancel().await?;
    server_handle.await??;
    Ok(())
}

// ---------------------------------------------------------------------------
// doiget_metadata_only — per-branch coverage (I1 from PR #84 multi-agent
// review). Each test stands up its own in-memory duplex pipe so failures
// localise cleanly.
// ---------------------------------------------------------------------------

/// Boilerplate: spin up a server side over a duplex pipe with a clean
/// capability profile and return the connected client + server JoinHandle.
/// The caller cancels the client to drive `waiting()` to completion.
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
async fn doiget_metadata_only_invalid_ref_returns_invalid_ref_envelope() -> anyhow::Result<()> {
    let (client, server_handle) = boot_in_memory_server().await?;

    // Build the {"ref":"not a doi"} JsonObject (rmcp's `with_arguments`
    // takes a `serde_json::Map<String, Value>`).
    let mut args = serde_json::Map::new();
    args.insert("ref".to_string(), serde_json::json!("not a doi"));

    let result = client
        .peer()
        .call_tool(
            rmcp::model::CallToolRequestParams::new("doiget_metadata_only").with_arguments(args),
        )
        .await?;
    let structured = result
        .structured_content
        .as_ref()
        .expect("doiget_metadata_only uses CallToolResult::structured");
    assert_eq!(structured["ok"], serde_json::json!(false));
    assert_eq!(
        structured["error"]["code"],
        serde_json::json!("INVALID_REF"),
        "envelope: {structured:?}"
    );
    assert!(
        structured["error"]["message"]
            .as_str()
            .map(|s| s.contains("invalid ref"))
            .unwrap_or(false),
        "INVALID_REF message must mention 'invalid ref' for diagnosability; got: {structured:?}"
    );

    client.cancel().await?;
    server_handle.await??;
    Ok(())
}

#[tokio::test]
async fn doiget_metadata_only_dry_run_true_returns_fetch_plan_envelope() -> anyhow::Result<()> {
    let (client, server_handle) = boot_in_memory_server().await?;

    let mut args = serde_json::Map::new();
    args.insert("ref".to_string(), serde_json::json!("10.1234/example"));
    args.insert("dry_run".to_string(), serde_json::json!(true));

    let result = client
        .peer()
        .call_tool(
            rmcp::model::CallToolRequestParams::new("doiget_metadata_only").with_arguments(args),
        )
        .await?;
    let structured = result
        .structured_content
        .as_ref()
        .expect("doiget_metadata_only dry-run uses CallToolResult::structured");

    assert_eq!(structured["ok"], serde_json::json!(true));
    assert_eq!(structured["dry_run"], serde_json::json!(true));
    // ADR-0022 §1: ref shape is `{"doi":"..."}` for a DOI input.
    assert_eq!(
        structured["ref"],
        serde_json::json!({"doi": "10.1234/example"})
    );
    // ADR-0022 §1 NORMATIVE plan shape for DOI: metadata_sources =
    // ["crossref","unpaywall"]; pdf_sources is non-empty under the
    // `oa-publisher` synthetic key.
    assert_eq!(
        structured["plan"]["metadata_sources"],
        serde_json::json!(["crossref", "unpaywall"])
    );
    let pdf_sources = structured["plan"]["pdf_sources"]
        .as_array()
        .expect("plan.pdf_sources is an array");
    assert!(
        !pdf_sources.is_empty(),
        "plan.pdf_sources must be non-empty for a DOI ref; got: {structured:?}"
    );
    assert_eq!(pdf_sources[0]["key"], serde_json::json!("oa-publisher"));
    // Rate-limit budget is the static HARD_CODED snapshot.
    assert_eq!(
        structured["rate_limit_budget"]["global_per_sec"],
        serde_json::json!(5.0)
    );

    client.cancel().await?;
    server_handle.await??;
    Ok(())
}

#[tokio::test]
async fn doiget_metadata_only_default_dry_run_false_returns_internal_error_stub(
) -> anyhow::Result<()> {
    let (client, server_handle) = boot_in_memory_server().await?;

    // Omit `dry_run`; defaults to false via `#[serde(default)] dry_run: bool`.
    let mut args = serde_json::Map::new();
    args.insert("ref".to_string(), serde_json::json!("10.1234/example"));

    let result = client
        .peer()
        .call_tool(
            rmcp::model::CallToolRequestParams::new("doiget_metadata_only").with_arguments(args),
        )
        .await?;
    let structured = result
        .structured_content
        .as_ref()
        .expect("doiget_metadata_only stub uses CallToolResult::structured");

    // Phase 1: the non-dry-run branch is a documented stub returning
    // INTERNAL_ERROR with a clear message (see Server::doiget_metadata_only).
    assert_eq!(structured["ok"], serde_json::json!(false));
    assert_eq!(
        structured["error"]["code"],
        serde_json::json!("INTERNAL_ERROR"),
        "envelope: {structured:?}"
    );
    let msg = structured["error"]["message"]
        .as_str()
        .expect("error.message is a string");
    assert!(
        msg.contains("not yet wired") || msg.contains("dry_run"),
        "INTERNAL_ERROR stub message must mention the Phase 1 limitation; \
         got: {msg:?}"
    );

    client.cancel().await?;
    server_handle.await??;
    Ok(())
}
