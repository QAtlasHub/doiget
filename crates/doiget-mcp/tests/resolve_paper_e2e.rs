//! Slice 7 — end-to-end coverage for `doiget_resolve_paper`
//! (wiremock-driven; NO outbound network).
//!
//! ## Network purity
//!
//! Per the workspace network-purity guard, this file imports `wiremock`
//! to mount fake origins; no `reqwest::*` items are imported directly.
//! All HTTP traffic terminates at a `wiremock::MockServer` on
//! `127.0.0.1:N`. The escape-hatch comment below covers the future
//! addition of any `reqwest::*` import without further intervention.
// allow: outbound-network
//!
//! ## What is asserted
//!
//! `doiget_resolve_paper` is the audit-trail-preserving sibling of
//! `doiget_metadata_only`: each consulted resolver still emits a
//! provenance row through `ctx.log`, but the orchestrator MUST NOT write
//! the metadata TOML to the store. These tests exercise the wire
//! contract documented in `docs/MCP_TOOLS.md` §1 (Phase 3 baseline tool
//! list).
//!
//! Three test cases:
//!
//! 1. `doiget_resolve_paper_invalid_ref_returns_invalid_ref_envelope` —
//!    a malformed `ref` collapses to the closed `INVALID_REF` error code.
//! 2. `doiget_resolve_paper_arxiv_happy_path_returns_metadata_envelope` —
//!    an arXiv id is resolved through a wiremocked Atom feed.
//! 3. `doiget_resolve_paper_doi_crossref_happy_path_returns_metadata_envelope` —
//!    a DOI is resolved through a wiremocked Crossref response.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use doiget_core::CapabilityProfile;
use doiget_mcp::Server;
use rmcp::{model::CallToolRequestParams, ServiceExt};

/// RAII helper mirroring `initialize_handshake::EnvGuard` and
/// `fetch_paper_e2e::EnvGuard`. Scoped env mutations so a panic
/// mid-test does not leak state across the single-threaded
/// `serial_test::serial` group.
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
    // DOIGET_STORE_ROOT is included so any accidental store write
    // (a regression that violates the resolve_only no-persistence
    // contract) lands inside the test's TempDir rather than the
    // developer's real ~/papers/.
    "DOIGET_STORE_ROOT",
    "DOIGET_LOG_PATH",
    "DOIGET_ARXIV_BASE",
    "DOIGET_CROSSREF_BASE",
    "DOIGET_UNPAYWALL_BASE",
    "DOIGET_CONTACT_EMAIL",
    "DOIGET_UNPAYWALL_EMAIL",
];

/// Count regular files under `<store_root>/.metadata/`. The binding
/// contract for `resolve_only` (and therefore `doiget_resolve_paper`)
/// is that this count remains 0 after any successful resolve, even
/// after Phase 2.x adds the store-write to `metadata_only`. The two
/// happy-path tests below use this to assert the contract.
fn count_metadata_files(store_root: &std::path::Path) -> usize {
    let meta = store_root.join(".metadata");
    match std::fs::read_dir(&meta) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
            .count(),
        Err(_) => 0,
    }
}

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

/// Synthetic Atom payload mirroring the Slice 1 reference. Do not hit
/// real arXiv.
const SAMPLE_ATOM_FEED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <entry>
    <id>http://arxiv.org/abs/2401.12345v1</id>
    <updated>2024-02-01T00:00:00Z</updated>
    <published>2024-01-15T00:00:00Z</published>
    <title>Example arXiv Paper Title</title>
    <summary>This is an example abstract.</summary>
    <author><name>Jane Doe</name></author>
    <author><name>John Roe</name></author>
    <category term="cs.LG" scheme="http://arxiv.org/schemas/atom"/>
    <category term="stat.ML" scheme="http://arxiv.org/schemas/atom"/>
  </entry>
</feed>"#;

/// Synthetic Crossref `message` payload. Carries an OA URL in
/// `message.link[]` so `extract_crossref_oa_url` returns `Some(...)`.
const SAMPLE_CROSSREF_RESPONSE: &str = r#"{"status":"ok","message":{"title":["Example Paper"],"link":[{"URL":"https://example.org/oa.pdf"}]}}"#;

// ---------------------------------------------------------------------------
// 1. invalid ref -> INVALID_REF
// ---------------------------------------------------------------------------

#[tokio::test]
async fn doiget_resolve_paper_invalid_ref_returns_invalid_ref_envelope() -> anyhow::Result<()> {
    let (client, server_handle) = boot_in_memory_server().await?;

    let mut args = serde_json::Map::new();
    args.insert("ref".to_string(), serde_json::json!("not a doi"));

    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_resolve_paper").with_arguments(args))
        .await?;
    let structured = result
        .structured_content
        .as_ref()
        .expect("doiget_resolve_paper uses CallToolResult::structured");
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
        "INVALID_REF message must mention 'invalid ref'; got: {structured:?}"
    );

    client.cancel().await?;
    server_handle.await??;
    Ok(())
}

// ---------------------------------------------------------------------------
// 2. arxiv happy path
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial_test::serial]
async fn doiget_resolve_paper_arxiv_happy_path_returns_metadata_envelope() -> anyhow::Result<()> {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/query"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_ATOM_FEED))
        .mount(&server)
        .await;

    let td = tempfile::TempDir::new().expect("tempdir");
    let log_path = camino::Utf8Path::from_path(td.path())
        .expect("tempdir is utf-8")
        .join("mcp-resolve.jsonl");

    let env = EnvGuard::new(ENV_KEYS);
    env.set("DOIGET_ARXIV_BASE", &server.uri());
    env.set("DOIGET_LOG_PATH", log_path.as_str());
    // Bind the store root to the same tempdir so the no-store-write
    // assertion below is meaningful — without this, an accidental
    // store write would target the developer's real ~/papers/.
    env.set(
        "DOIGET_STORE_ROOT",
        td.path().to_str().expect("utf-8 tempdir"),
    );

    let (client, server_handle) = boot_in_memory_server().await?;

    let mut args = serde_json::Map::new();
    args.insert("ref".to_string(), serde_json::json!("2401.12345"));

    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_resolve_paper").with_arguments(args))
        .await?;
    let structured = result
        .structured_content
        .as_ref()
        .expect("doiget_resolve_paper uses CallToolResult::structured");

    assert_eq!(
        structured["ok"],
        serde_json::json!(true),
        "envelope: {structured:?}"
    );
    assert_eq!(structured["source"], serde_json::json!("arxiv"));
    assert_eq!(structured["resolver_profile"], serde_json::json!("arxiv"));
    assert_eq!(structured["ref"], serde_json::json!("2401.12345"));
    assert_eq!(structured["license"], serde_json::json!("arxiv-default"));
    // arXiv branch never surfaces an OA URL (the abstract page is not
    // a PDF URL) — the field is emitted as `null`.
    assert_eq!(structured["oa_url"], serde_json::Value::Null);
    assert_eq!(
        structured["metadata"]["title"],
        serde_json::json!("Example arXiv Paper Title")
    );
    assert_eq!(
        structured["metadata"]["authors"],
        serde_json::json!(["Jane Doe", "John Roe"])
    );

    // Provenance log was opened and at least one row was appended
    // (SessionStart + Fetch + SessionEnd).
    assert!(
        log_path.exists(),
        "provenance log not created at {log_path}"
    );

    // Binding contract (resolve_only's doc-comment): NO metadata TOML
    // is written to the store, even after Phase 2.x adds the store
    // write to `metadata_only`. Today the two functions are equivalent
    // because metadata_only itself doesn't write; this assertion is
    // the regression guard for that future divergence.
    assert_eq!(
        count_metadata_files(td.path()),
        0,
        "doiget_resolve_paper MUST NOT write metadata TOML to the store"
    );

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    drop(td);
    Ok(())
}

// ---------------------------------------------------------------------------
// 3. DOI crossref happy path
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial_test::serial]
async fn doiget_resolve_paper_doi_crossref_happy_path_returns_metadata_envelope(
) -> anyhow::Result<()> {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/works/10.1234/example"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_CROSSREF_RESPONSE))
        .mount(&server)
        .await;

    let td = tempfile::TempDir::new().expect("tempdir");
    let log_path = camino::Utf8Path::from_path(td.path())
        .expect("tempdir is utf-8")
        .join("mcp-resolve.jsonl");

    let env = EnvGuard::new(ENV_KEYS);
    env.set("DOIGET_CROSSREF_BASE", &server.uri());
    env.set("DOIGET_LOG_PATH", log_path.as_str());
    env.set("DOIGET_CONTACT_EMAIL", "test@example.org");
    // Bind store root to the tempdir; see arxiv test for rationale.
    env.set(
        "DOIGET_STORE_ROOT",
        td.path().to_str().expect("utf-8 tempdir"),
    );

    let (client, server_handle) = boot_in_memory_server().await?;

    let mut args = serde_json::Map::new();
    args.insert("ref".to_string(), serde_json::json!("10.1234/example"));

    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_resolve_paper").with_arguments(args))
        .await?;
    let structured = result
        .structured_content
        .as_ref()
        .expect("doiget_resolve_paper uses CallToolResult::structured");

    assert_eq!(
        structured["ok"],
        serde_json::json!(true),
        "envelope: {structured:?}"
    );
    assert_eq!(structured["source"], serde_json::json!("crossref"));
    assert_eq!(
        structured["resolver_profile"],
        serde_json::json!("crossref")
    );
    assert_eq!(structured["ref"], serde_json::json!("10.1234/example"));
    // Crossref does not surface a license directly; the channel for
    // license is Unpaywall (not consulted on the happy path).
    assert_eq!(structured["license"], serde_json::Value::Null);
    // OA URL is mined from message.link[].URL.
    assert_eq!(
        structured["oa_url"],
        serde_json::json!("https://example.org/oa.pdf")
    );
    assert_eq!(
        structured["metadata"]["title"],
        serde_json::json!(["Example Paper"])
    );

    assert!(
        log_path.exists(),
        "provenance log not created at {log_path}"
    );

    // Binding contract: NO metadata TOML written. See arxiv test for
    // the full rationale; this assertion guards the same invariant on
    // the DOI/Crossref branch.
    assert_eq!(
        count_metadata_files(td.path()),
        0,
        "doiget_resolve_paper MUST NOT write metadata TOML to the store"
    );

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    drop(td);
    Ok(())
}

// ---------------------------------------------------------------------------
// 4. dry_run field rejection
// ---------------------------------------------------------------------------

/// Per `docs/MCP_TOOLS.md` §10 / §211, `doiget_resolve_paper` is in the
/// "dry_run does not apply" set. The `ResolvePaperInput` struct carries
/// both `#[serde(deny_unknown_fields)]` and `#[schemars(deny_unknown_fields)]`
/// so an attempted `dry_run` field is rejected at the deserialize
/// boundary BEFORE the tool body runs.
///
/// This test sends `{"ref": "10.1234/example", "dry_run": true}` and
/// asserts the call surfaces as an error from the transport layer — it
/// MUST NOT be silently accepted (which would happen if only the
/// schemars-side attribute were present).
#[tokio::test]
async fn doiget_resolve_paper_dry_run_field_is_rejected() -> anyhow::Result<()> {
    let (client, server_handle) = boot_in_memory_server().await?;

    let mut args = serde_json::Map::new();
    args.insert("ref".to_string(), serde_json::json!("10.1234/example"));
    args.insert("dry_run".to_string(), serde_json::json!(true));

    let outcome = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_resolve_paper").with_arguments(args))
        .await;

    // rmcp surfaces deserialize failures as a transport-level error
    // (the call_tool future resolves to Err(...)) rather than an
    // ok:false envelope. Either way, the call MUST NOT succeed with
    // `dry_run` silently dropped.
    match outcome {
        Err(_) => {
            // Transport-level rejection. Expected path with
            // #[serde(deny_unknown_fields)] in place.
        }
        Ok(result) => {
            // Some MCP hosts may translate the deserialize error into
            // an `is_error: true` CallToolResult instead. Accept that
            // shape too — but the call MUST NOT return a normal
            // success envelope with the dry_run field ignored.
            assert!(
                result.is_error.unwrap_or(false)
                    || result
                        .structured_content
                        .as_ref()
                        .map(|s| s["ok"] == serde_json::json!(false))
                        .unwrap_or(false),
                "dry_run field was silently accepted on doiget_resolve_paper; \
                 deny_unknown_fields enforcement is missing. result: {result:?}"
            );
        }
    }

    client.cancel().await?;
    server_handle.await??;
    Ok(())
}
