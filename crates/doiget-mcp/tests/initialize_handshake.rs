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
    // Slice 2: fetch_paper + batch_fetch advertised.
    assert!(
        names.contains(&"doiget_fetch_paper"),
        "tools/list must include doiget_fetch_paper; got: {names:?}"
    );
    assert!(
        names.contains(&"doiget_batch_fetch"),
        "tools/list must include doiget_batch_fetch; got: {names:?}"
    );
    // Slice 7: doiget_resolve_paper advertised — metadata resolution with
    // no local persistence (audit log row only).
    assert!(
        names.contains(&"doiget_resolve_paper"),
        "tools/list must include doiget_resolve_paper; got: {names:?}"
    );
    // Slice 8: 4x read-path tools advertised.
    assert!(
        names.contains(&"doiget_info"),
        "tools/list must include doiget_info; got: {names:?}"
    );
    assert!(
        names.contains(&"doiget_search_local"),
        "tools/list must include doiget_search_local; got: {names:?}"
    );
    assert!(
        names.contains(&"doiget_list_recent"),
        "tools/list must include doiget_list_recent; got: {names:?}"
    );
    assert!(
        names.contains(&"doiget_paper_pdf_path"),
        "tools/list must include doiget_paper_pdf_path; got: {names:?}"
    );
    // Slice 15: doiget_expand_citation_graph is always advertised
    // (the tool method is always present; body returns NOT_IMPLEMENTED
    // when this binary was built without --features citation).
    assert!(
        names.contains(&"doiget_expand_citation_graph"),
        "tools/list must include doiget_expand_citation_graph; got: {names:?}"
    );
    // Slice 15b: BibTeX / CSL export tools.
    assert!(
        names.contains(&"doiget_bibtex_export"),
        "tools/list must include doiget_bibtex_export; got: {names:?}"
    );
    assert!(
        names.contains(&"doiget_csl_export"),
        "tools/list must include doiget_csl_export; got: {names:?}"
    );

    // -- 2b. negative scope guard (#152) -------------------------------
    //
    // `docs/SCOPE.md` permanent non-goals: these tool names must NEVER
    // be advertised. A future refactor that reintroduces one (e.g. an
    // SSRF-prone `doiget_fetch_url`, a credential sink, a destructive
    // store op, or a generic shell/exec escape) must fail here rather
    // than ship silently. See also the dedicated
    // `tools_list_excludes_scope_nongoals` test below.
    assert_forbidden_tools_absent(&names);

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
    // NORMATIVE `docs/MCP_TOOLS.md` §7 contract fields (#141).
    assert_eq!(cap_struct["oa_enabled"], serde_json::json!(true));
    assert!(
        cap_struct["metadata_sources"].is_array(),
        "§7 requires metadata_sources: string[]; got: {cap_struct:?}"
    );
    // Clean (Tier-1-only) env: no Tier-2 metadata sources enabled.
    assert_eq!(
        cap_struct["metadata_sources"],
        serde_json::json!([]),
        "clean env must report empty metadata_sources; got: {cap_struct:?}"
    );
    assert_eq!(cap_struct["tdm_enabled"], serde_json::json!(false));
    assert_eq!(cap_struct["tdm_elsevier"], serde_json::json!(false));
    assert_eq!(cap_struct["tdm_aps"], serde_json::json!(false));
    assert_eq!(cap_struct["tdm_springer"], serde_json::json!(false));
    assert_eq!(cap_struct["rate_limit_per_sec"], serde_json::json!(5.0));
    // Additive back-compat fields (not part of the §7 contract).
    assert_eq!(cap_struct["ok"], serde_json::json!(true));
    assert_eq!(
        cap_struct["tier_1"],
        serde_json::json!(["arxiv", "crossref", "unpaywall"])
    );

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

/// `docs/SCOPE.md` permanent non-goals (#152). Any tool name in this set
/// — or any name that *looks* like a generic shell / exec / eval escape
/// — must never appear in `tools/list`. Reintroducing one (even behind a
/// feature flag) is a hard scope violation, not a regression to triage.
const FORBIDDEN_TOOL_NAMES: &[&str] = &[
    "doiget_fetch_url",
    "doiget_set_credentials",
    "doiget_delete_paper",
];

/// Substrings that betray a generic command/eval escape hatch. Matched
/// case-insensitively against each advertised tool name so a future
/// `doiget_run_shell`, `doiget_exec`, `doiget_eval`, `doiget_system`,
/// etc. trips the guard without needing an exact-name entry.
const FORBIDDEN_TOOL_SUBSTRINGS: &[&str] = &[
    "shell",
    "exec",
    "eval",
    "spawn",
    "system",
    "command",
    "subprocess",
];

/// Assert no `docs/SCOPE.md` non-goal tool is advertised. Shared by the
/// roundtrip smoke test and the dedicated negative test below.
fn assert_forbidden_tools_absent(names: &[&str]) {
    for forbidden in FORBIDDEN_TOOL_NAMES {
        assert!(
            !names.contains(forbidden),
            "SCOPE.md non-goal tool `{forbidden}` must NEVER be in tools/list; got: {names:?}"
        );
    }
    for name in names {
        let lower = name.to_ascii_lowercase();
        for needle in FORBIDDEN_TOOL_SUBSTRINGS {
            assert!(
                !lower.contains(needle),
                "tool `{name}` matches forbidden generic-escape substring `{needle}` \
                 (SCOPE.md non-goal); got: {names:?}"
            );
        }
    }
}

/// #152: dedicated negative scope guard. The roundtrip test also calls
/// `assert_forbidden_tools_absent`, but this standalone test localises a
/// scope-violation failure cleanly and documents the invariant on its
/// own.
#[tokio::test]
async fn tools_list_excludes_scope_nongoals() -> anyhow::Result<()> {
    let (client, server_handle) = boot_in_memory_server().await?;
    let tools = client.peer().list_all_tools().await?;
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();

    assert_forbidden_tools_absent(&names);

    client.cancel().await?;
    server_handle.await??;
    Ok(())
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
    // Issue #123: docs/MCP_TOOLS.md §5 mandates `ref` on every
    // ok:false envelope (was previously omitted by
    // metadata_only_error_envelope).
    assert_eq!(
        structured["ref"],
        serde_json::json!("not a doi"),
        "ok:false envelope must carry the request ref (§5); got: {structured:?}"
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

// ---------------------------------------------------------------------------
// doiget_metadata_only — Slice 1 non-dry-run wiring (A.3)
//
// The stub-era `doiget_metadata_only_default_dry_run_false_returns_not_implemented_stub`
// test was DELETED here: the NOT_IMPLEMENTED branch no longer exists.
// The tests below exercise the live orchestrator end-to-end through
// the MCP envelope (wiremock-driven; no real network).
//
// These tests mutate process-global env vars (`DOIGET_*_BASE`,
// `DOIGET_LOG_PATH`) so they are serialized via `serial_test::serial`.
// ---------------------------------------------------------------------------

/// RAII helper to scope env-var mutations across a test. Mirrors the
/// `EnvGuard` in `crates/doiget-cli/tests/fetch_arxiv_e2e.rs`.
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

/// Synthetic Atom payload (B.3 from the Slice 1 spec). Do not hit
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

#[tokio::test]
#[serial_test::serial]
async fn doiget_metadata_only_arxiv_happy_path_returns_metadata_envelope() -> anyhow::Result<()> {
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
        .join("mcp-meta.jsonl");

    let env = EnvGuard::new(&[
        "DOIGET_ARXIV_BASE",
        "DOIGET_CROSSREF_BASE",
        "DOIGET_UNPAYWALL_BASE",
        "DOIGET_LOG_PATH",
    ]);
    env.set("DOIGET_ARXIV_BASE", &server.uri());
    env.set("DOIGET_LOG_PATH", log_path.as_str());

    let (client, server_handle) = boot_in_memory_server().await?;

    let mut args = serde_json::Map::new();
    args.insert("ref".to_string(), serde_json::json!("2401.12345"));

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

    assert_eq!(
        structured["ok"],
        serde_json::json!(true),
        "envelope: {structured:?}"
    );
    assert_eq!(structured["source"], serde_json::json!("arxiv"));
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

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    drop(td);
    Ok(())
}

#[tokio::test]
#[serial_test::serial]
async fn doiget_metadata_only_doi_crossref_happy_path_returns_metadata_envelope(
) -> anyhow::Result<()> {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/works/10.1234/example"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"{"status":"ok","message":{"title":["Example Paper"],"link":[{"URL":"https://example.org/oa.pdf"}]}}"#,
        ))
        .mount(&server)
        .await;

    let td = tempfile::TempDir::new().expect("tempdir");
    let log_path = camino::Utf8Path::from_path(td.path())
        .expect("tempdir is utf-8")
        .join("mcp-meta.jsonl");

    let env = EnvGuard::new(&[
        "DOIGET_ARXIV_BASE",
        "DOIGET_CROSSREF_BASE",
        "DOIGET_UNPAYWALL_BASE",
        "DOIGET_LOG_PATH",
    ]);
    env.set("DOIGET_CROSSREF_BASE", &server.uri());
    env.set("DOIGET_LOG_PATH", log_path.as_str());

    let (client, server_handle) = boot_in_memory_server().await?;

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
        .expect("doiget_metadata_only uses CallToolResult::structured");

    assert_eq!(
        structured["ok"],
        serde_json::json!(true),
        "envelope: {structured:?}"
    );
    assert_eq!(structured["source"], serde_json::json!("crossref"));
    assert_eq!(structured["ref"], serde_json::json!("10.1234/example"));
    // Crossref does not surface a license directly (Phase 1; future
    // slices will chain Unpaywall for license enrichment).
    assert_eq!(structured["license"], serde_json::Value::Null);
    // OA URL discovered via `message.link[]`. Surfaced but never
    // followed — the test does not mount a mock for it.
    assert_eq!(
        structured["oa_url"],
        serde_json::json!("https://example.org/oa.pdf")
    );
    assert_eq!(
        structured["metadata"]["title"],
        serde_json::json!(["Example Paper"])
    );

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    drop(td);
    Ok(())
}

/// #139 literal acceptance + the headline regression guard: after
/// `doiget_metadata_only`, `doiget_info` on the same ref MUST return a
/// non-null `metadata` (i.e. the §11 store-write actually happened).
/// This is the ONE test that fails if the mcp handler is ever rewired
/// back to the pure `metadata_only` (no store-write) instead of
/// `metadata_only_to_store` (PR #199 2nd-pass review).
#[tokio::test]
#[serial_test::serial]
async fn doiget_metadata_only_then_doiget_info_returns_non_null_metadata() -> anyhow::Result<()> {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/works/10.1234/example"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"{"status":"ok","message":{"title":["Example Paper"]}}"#),
        )
        .mount(&server)
        .await;

    let td = tempfile::TempDir::new().expect("tempdir");
    let base = camino::Utf8Path::from_path(td.path()).expect("tempdir is utf-8");
    let log_path = base.join("mcp-meta.jsonl");
    let store_root = base.join("papers");

    let env = EnvGuard::new(&[
        "DOIGET_ARXIV_BASE",
        "DOIGET_CROSSREF_BASE",
        "DOIGET_UNPAYWALL_BASE",
        "DOIGET_LOG_PATH",
        "DOIGET_STORE_ROOT",
    ]);
    env.set("DOIGET_CROSSREF_BASE", &server.uri());
    env.set("DOIGET_LOG_PATH", log_path.as_str());
    // Pin the store to the tempdir so the §11 write does NOT land in the
    // developer's real ~/papers/, and so doiget_info reads it back here.
    env.set("DOIGET_STORE_ROOT", store_root.as_str());

    let (client, server_handle) = boot_in_memory_server().await?;

    let mut meta_args = serde_json::Map::new();
    meta_args.insert("ref".to_string(), serde_json::json!("10.1234/example"));
    let meta = client
        .peer()
        .call_tool(
            rmcp::model::CallToolRequestParams::new("doiget_metadata_only")
                .with_arguments(meta_args),
        )
        .await?;
    let meta_s = meta
        .structured_content
        .as_ref()
        .expect("doiget_metadata_only structured");
    assert_eq!(
        meta_s["ok"],
        serde_json::json!(true),
        "metadata_only envelope: {meta_s:?}"
    );

    let mut info_args = serde_json::Map::new();
    info_args.insert("ref".to_string(), serde_json::json!("10.1234/example"));
    let info = client
        .peer()
        .call_tool(rmcp::model::CallToolRequestParams::new("doiget_info").with_arguments(info_args))
        .await?;
    let info_s = info
        .structured_content
        .as_ref()
        .expect("doiget_info structured");
    assert_eq!(info_s["ok"], serde_json::json!(true), "info: {info_s:?}");
    assert!(
        !info_s["metadata"].is_null(),
        "doiget_info MUST return non-null metadata after doiget_metadata_only \
         (the §11 store-write SIDE EFFECT, #139); got: {info_s:?}"
    );
    assert_eq!(
        info_s["metadata"]["title"],
        serde_json::json!("Example Paper"),
        "persisted title round-trips through doiget_info; got: {info_s:?}"
    );

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    drop(td);
    Ok(())
}

#[tokio::test]
#[serial_test::serial]
async fn doiget_metadata_only_network_failure_returns_network_error_envelope() -> anyhow::Result<()>
{
    // Point Crossref and Unpaywall at a wiremock that returns 500 on
    // every call. The orchestrator will try Crossref first, then fall
    // back to Unpaywall (also pointed at the same broken origin), and
    // surface a NETWORK_ERROR envelope when both legs fail.
    // `denial_context` is absent — transport errors are not denials
    // per ADR-0023 §4.
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let td = tempfile::TempDir::new().expect("tempdir");
    let log_path = camino::Utf8Path::from_path(td.path())
        .expect("tempdir is utf-8")
        .join("mcp-meta.jsonl");

    let env = EnvGuard::new(&[
        "DOIGET_ARXIV_BASE",
        "DOIGET_CROSSREF_BASE",
        "DOIGET_UNPAYWALL_BASE",
        "DOIGET_LOG_PATH",
    ]);
    env.set("DOIGET_CROSSREF_BASE", &server.uri());
    env.set("DOIGET_UNPAYWALL_BASE", &server.uri());
    env.set("DOIGET_LOG_PATH", log_path.as_str());

    let (client, server_handle) = boot_in_memory_server().await?;

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
        .expect("doiget_metadata_only uses CallToolResult::structured");

    assert_eq!(
        structured["ok"],
        serde_json::json!(false),
        "envelope: {structured:?}"
    );
    assert_eq!(
        structured["error"]["code"],
        serde_json::json!("NETWORK_ERROR"),
        "envelope: {structured:?}"
    );
    // Transport-level errors do NOT produce a denial_context (ADR-0023
    // §4 mapping table: only RedirectDenied / InsecureRedirect /
    // OversizedBody / NotAPdf map to denial reasons).
    assert!(
        structured["error"].get("denial_context").is_none()
            || structured["error"]["denial_context"].is_null(),
        "NETWORK_ERROR envelope must omit denial_context; got: {structured:?}"
    );

    client.cancel().await?;
    server_handle.await??;
    drop(env);
    drop(td);
    Ok(())
}
