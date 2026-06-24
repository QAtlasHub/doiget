//! Slice 15b — end-to-end coverage for `doiget_bibtex_export` /
//! `doiget_csl_export`.
//!
//! These tools are 100% local: no network, no provenance row, reads
//! through the `Store` trait + the shared `doiget_core::store::render`
//! helpers. Each test seeds a `FsStore` rooted at a per-test
//! `tempfile::TempDir`, points `DOIGET_STORE_ROOT` at it, then drives
//! the in-memory MCP server.
//!
//! ## Network purity
//!
//! No `reqwest::*` items are imported; no HTTP traffic of any kind.
// allow: outbound-network

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeMap;

use chrono::TimeZone;
use doiget_core::store::{DoigetExtension, FsStore, Metadata, Store};
use doiget_core::{CapabilityProfile, Doi, Ref, SCHEMA_VERSION};
use doiget_mcp::Server;
use rmcp::{model::CallToolRequestParams, ServiceExt};

struct EnvGuard;
impl EnvGuard {
    fn new() -> Self {
        std::env::remove_var("DOIGET_STORE_ROOT");
        Self
    }
}
impl Drop for EnvGuard {
    fn drop(&mut self) {
        std::env::remove_var("DOIGET_STORE_ROOT");
    }
}

async fn boot() -> anyhow::Result<(
    rmcp::service::RunningService<rmcp::RoleClient, ()>,
    tokio::task::JoinHandle<anyhow::Result<()>>,
)> {
    let profile = CapabilityProfile::from_env().expect("clean env never errors");
    let server = Server::new(profile);
    let (server_transport, client_transport) = tokio::io::duplex(64 * 1024);
    let handle = tokio::spawn(async move {
        let service = server.serve(server_transport).await?;
        service.waiting().await?;
        anyhow::Ok(())
    });
    let client = ().serve(client_transport).await?;
    Ok((client, handle))
}

fn seed_store() -> (tempfile::TempDir, camino::Utf8PathBuf) {
    let td = tempfile::TempDir::new().expect("tempdir");
    let root = camino::Utf8Path::from_path(td.path())
        .expect("tempdir is utf-8")
        .to_path_buf();
    std::fs::create_dir_all(root.join(".metadata")).expect("create .metadata");

    let store = FsStore::new(root.clone()).expect("FsStore::new");
    let ref_ = Ref::parse("10.1234/example").expect("valid DOI ref");
    let m = Metadata {
        schema_version: SCHEMA_VERSION.to_string(),
        title: "Quantum Stuff".to_string(),
        authors: vec!["Alice Researcher".to_string(), "Bob Coauthor".to_string()],
        year: Some(2026),
        doi: Some(Doi::parse("10.1234/example").expect("valid DOI")),
        arxiv_id: None,
        arxiv_categories: vec![],
        abstract_: None,
        venue: Some("Phys Rev X".to_string()),
        volume: None,
        issue: None,
        pages: None,
        publisher: Some("APS".to_string()),
        issn: Some("2160-3308".to_string()),
        isbn: None,
        type_: Some("journal-article".to_string()),
        keywords: vec![],
        url: None,
        pdf_path: None,
        doiget: Some(DoigetExtension {
            fetched_at: chrono::Utc
                .with_ymd_and_hms(2026, 5, 6, 12, 0, 0)
                .single()
                .expect("valid timestamp"),
            source: "unpaywall".to_string(),
            license: "CC-BY-4.0".to_string(),
            oa_status: None,
            size_bytes: 1234,
            mcp_call_id: None,
            tags: Vec::new(),
            collections: Vec::new(),
            annotation: None,
        }),
        other: BTreeMap::new(),
    };
    store
        .write(&ref_.safekey(), &m, None)
        .expect("seed store entry");
    (td, root)
}

#[tokio::test]
#[serial_test::serial]
async fn bibtex_export_mixed_batch() -> anyhow::Result<()> {
    let _g = EnvGuard::new();
    let (_td, root) = seed_store();
    std::env::set_var("DOIGET_STORE_ROOT", root.as_str());

    let (client, handle) = boot().await?;
    let mut args = serde_json::Map::new();
    args.insert(
        "refs".to_string(),
        serde_json::json!(["10.1234/example", "10.9999/missing", "not a doi"]),
    );
    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_bibtex_export").with_arguments(args))
        .await?;
    let s = result
        .structured_content
        .expect("doiget_bibtex_export uses CallToolResult::structured");

    assert_eq!(s["ok"], serde_json::json!(true));
    let entries = s["entries"].as_array().expect("entries array");
    assert_eq!(entries.len(), 3, "{s:?}");

    // [0] seeded -> BibTeX string.
    assert_eq!(entries[0]["ref"], "10.1234/example");
    let bib = entries[0]["bibtex"].as_str().expect("bibtex string");
    assert!(bib.starts_with("@article{"), "{bib}");
    assert!(bib.contains("title      = {Quantum Stuff},"), "{bib}");

    // [1] missing -> null payload, NOT an error.
    assert_eq!(entries[1]["ref"], "10.9999/missing");
    assert_eq!(entries[1]["bibtex"], serde_json::Value::Null);
    assert!(entries[1].get("error").is_none(), "{s:?}");

    // [2] invalid ref -> per-entry error.
    assert_eq!(entries[2]["ref"], "not a doi");
    assert_eq!(entries[2]["error"]["code"], "INVALID_REF");

    client.cancel().await?;
    handle.abort();
    Ok(())
}

#[tokio::test]
#[serial_test::serial]
async fn csl_export_found_and_missing() -> anyhow::Result<()> {
    let _g = EnvGuard::new();
    let (_td, root) = seed_store();
    std::env::set_var("DOIGET_STORE_ROOT", root.as_str());

    let (client, handle) = boot().await?;
    let mut args = serde_json::Map::new();
    args.insert(
        "refs".to_string(),
        serde_json::json!(["10.1234/example", "10.9999/missing"]),
    );
    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_csl_export").with_arguments(args))
        .await?;
    let s = result
        .structured_content
        .expect("doiget_csl_export uses CallToolResult::structured");

    assert_eq!(s["ok"], serde_json::json!(true));
    let entries = s["entries"].as_array().expect("entries array");
    assert_eq!(entries.len(), 2);

    let csl = entries[0]["csl"].as_array().expect("csl is an array");
    assert_eq!(csl.len(), 1, "single-element CSL array");
    assert_eq!(csl[0]["type"], "article-journal");
    assert_eq!(csl[0]["title"], "Quantum Stuff");
    assert_eq!(csl[0]["DOI"], "10.1234/example");

    assert_eq!(entries[1]["csl"], serde_json::Value::Null);

    client.cancel().await?;
    handle.abort();
    Ok(())
}

#[tokio::test]
#[serial_test::serial]
async fn bibtex_export_too_many_refs_is_invalid_ref() -> anyhow::Result<()> {
    let _g = EnvGuard::new();
    let (_td, root) = seed_store();
    std::env::set_var("DOIGET_STORE_ROOT", root.as_str());

    let (client, handle) = boot().await?;
    let refs: Vec<String> = (0..201).map(|i| format!("10.1234/n{i}")).collect();
    let mut args = serde_json::Map::new();
    args.insert("refs".to_string(), serde_json::json!(refs));
    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("doiget_bibtex_export").with_arguments(args))
        .await?;
    let s = result.structured_content.expect("structured envelope");
    assert_eq!(s["ok"], serde_json::json!(false));
    assert_eq!(s["error"]["code"], "INVALID_REF");

    client.cancel().await?;
    handle.abort();
    Ok(())
}
