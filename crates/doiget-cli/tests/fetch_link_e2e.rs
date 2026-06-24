//! End-to-end test for `doiget fetch --link <dir>` (#344 Slice 2).
//!
//! Drives `fetch::run_with_options(.., link = Some(dir), ..)` in-process (no
//! child spawn), with a wiremock-served arXiv PDF (no outbound network), and
//! asserts the artifact is materialised in the link dir — as a symlink, or a
//! copy where symlinks are unavailable; both resolve to the PDF bytes. The
//! linked filename is metadata-derived or the safekey, so the assertion checks
//! content (and that exactly one PDF landed), not the exact name. The slug
//! naming and the refuse-to-clobber behaviour are unit-tested in `fetch.rs`.
//!
//! ## Network purity
//! No outbound calls: all HTTP terminates at a `wiremock::MockServer`.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use camino::{Utf8Path, Utf8PathBuf};
use doiget_cli::commands::fetch;
use doiget_cli::commands::output::OutputMode;
use serial_test::serial;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod common;
use common::env_guard::EnvGuard;

#[tokio::test]
#[serial]
async fn fetch_link_materialises_pdf_in_link_dir() {
    let server = MockServer::start().await;
    let body = b"%PDF-1.7\n%link-fixture\n".to_vec();
    Mock::given(method("GET"))
        .and(path("/pdf/2401.12345.pdf"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
        .mount(&server)
        .await;

    let td = TempDir::new().expect("tempdir");
    let temp_root: Utf8PathBuf = Utf8Path::from_path(td.path())
        .expect("temp dir is utf-8")
        .to_path_buf();
    let store_root = temp_root.join("papers");
    let log_path = temp_root.join("log.jsonl");
    let link_dir = temp_root.join("workspace");

    let env = EnvGuard::new(&[
        "DOIGET_STORE_ROOT",
        "DOIGET_LOG_PATH",
        "DOIGET_ARXIV_BASE",
        "DOIGET_CROSSREF_BASE",
        "DOIGET_UNPAYWALL_BASE",
        "DOIGET_CONTACT_EMAIL",
        "DOIGET_UNPAYWALL_EMAIL",
    ]);
    env.set("DOIGET_STORE_ROOT", store_root.as_str());
    env.set("DOIGET_LOG_PATH", log_path.as_str());
    env.set("DOIGET_ARXIV_BASE", &server.uri());

    fetch::run_with_options(
        "arxiv:2401.12345".to_string(),
        false,
        Some(link_dir.clone()),
        OutputMode::Human,
    )
    .await
    .expect("fetch --link succeeds");

    // The store still holds the canonical PDF (single source of truth).
    let store_pdf = store_root.join("arxiv_2401.12345.pdf");
    assert!(store_pdf.exists(), "store PDF must exist: {store_pdf}");

    // The link dir holds exactly one .pdf (symlink or copy) resolving to the
    // same bytes. Name is metadata-derived or safekey → assert content, not name.
    let entries: Vec<Utf8PathBuf> = std::fs::read_dir(link_dir.as_std_path())
        .expect("link dir exists")
        .flatten()
        .filter_map(|e| Utf8PathBuf::from_path_buf(e.path()).ok())
        .filter(|p| p.extension() == Some("pdf"))
        .collect();
    assert_eq!(entries.len(), 1, "exactly one linked PDF; got {entries:?}");
    let linked_bytes = std::fs::read(entries[0].as_std_path()).expect("read linked pdf");
    assert_eq!(
        linked_bytes, body,
        "linked artifact (symlink or copy) must resolve to the PDF bytes"
    );

    drop(env);
    drop(td);
}
