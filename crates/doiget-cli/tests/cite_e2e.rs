// allow: outbound-network
//! End-to-end tests for `doiget cite <ref>`.
//!
//! `cite` resolves a reference live and prints a clean BibTeX entry on
//! stdout (a `doi2bib`-style helper). Both the DOI (Crossref) and arXiv
//! paths are exercised against wiremock origins via `DOIGET_*_BASE`, so
//! no outbound call is made. The malformed-id case needs no network —
//! `Ref::parse` rejects it before any resolver runs.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use assert_cmd::Command;
use predicates::str::contains;
use serde_json::json;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// DOI whose Crossref `/works/<doi>` mock is mounted below. Crossref's
/// URL builder uses `Url::join("/works/<doi>")`, which does NOT
/// percent-encode the `/` in the suffix, so the mock path is the literal
/// `/works/10.1234/cite.test`.
const TEST_DOI: &str = "10.1234/cite.test";

/// Crossref `message` envelope (CrossrefSource returns `envelope.message`,
/// so the body the orchestrator stores is this object, with the outer
/// `{status, message}` wrapper added by the mock response).
fn crossref_body() -> serde_json::Value {
    json!({
        "status": "ok",
        "message": {
            "title": ["A Synthetic Result on <i>Spin</i> Chains"],
            "author": [
                { "family": "Doe", "given": "Jane" },
                { "family": "Roe", "given": "Richard" },
            ],
            "issued": { "date-parts": [[2026, 1, 1]] },
            "container-title": ["Synthetic Journal of Physics"],
            "publisher": "Synthetic Society",
            "ISSN": ["1234-5678"],
            "volume": "42",
            "issue": "7",
            "page": "100-115",
            "type": "journal-article",
        }
    })
}

/// Reusable synthetic Atom feed for the arXiv path.
const SAMPLE_ATOM_FEED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <entry>
    <id>http://arxiv.org/abs/2401.12345v1</id>
    <title>Example arXiv Paper Title</title>
    <summary>This is an example abstract.</summary>
    <author><name>Jane Doe</name></author>
    <published>2024-01-15T00:00:00Z</published>
    <category term="cs.LG" scheme="http://arxiv.org/schemas/atom"/>
    <category term="stat.ML" scheme="http://arxiv.org/schemas/atom"/>
  </entry>
</feed>"#;

/// Build a `doiget` command with a temp HOME / store / log / cache so the
/// developer's real environment is never touched.
fn doiget(dir: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("doiget").expect("locate doiget binary");
    let p = dir.path().to_str().expect("tempdir path utf-8");
    let log_path = format!("{p}/log.jsonl");
    cmd.env("HOME", p)
        .env("USERPROFILE", p)
        .env("XDG_CONFIG_HOME", p)
        .env("APPDATA", p)
        .env("DOIGET_STORE_ROOT", p)
        .env("DOIGET_LOG_PATH", log_path)
        .env("DOIGET_CACHE_ROOT", p);
    cmd
}

#[test]
fn cite_typo_doi_is_rejected() {
    // `1O.1234` uses letter O — `Ref::parse` rejects it before any
    // network call, so cite exits non-zero with no BibTeX on stdout.
    let dir = TempDir::new().expect("tempdir");
    doiget(&dir)
        .args(["cite", "1O.1234/typo"])
        .assert()
        .failure();
}

#[tokio::test]
async fn cite_doi_emits_enriched_bibtex() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/works/{TEST_DOI}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(crossref_body()))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("tempdir");
    doiget(&dir)
        .args(["cite", TEST_DOI])
        .env("DOIGET_CROSSREF_BASE", server.uri())
        .assert()
        .success()
        // journal-article → @article, with the Crossref-enriched fields.
        .stdout(contains("@article{"))
        .stdout(contains("A Synthetic Result on Spin Chains")) // <i> stripped
        .stdout(contains("author     = {Doe, Jane and Roe, Richard},"))
        .stdout(contains("year       = {2026},"))
        .stdout(contains("journal    = {Synthetic Journal of Physics},"))
        .stdout(contains("volume     = {42},"))
        .stdout(contains("number     = {7},"))
        // Crossref single hyphen normalized to a BibTeX en-dash.
        .stdout(contains("pages      = {100--115},"))
        .stdout(contains("publisher  = {Synthetic Society},"))
        .stdout(contains("issn       = {1234-5678},"))
        .stdout(contains("doi        = {10.1234/cite.test},"));
}

#[tokio::test]
async fn cite_arxiv_emits_bibtex() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/query"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_ATOM_FEED))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("tempdir");
    doiget(&dir)
        .args(["cite", "arxiv:2401.12345"])
        .env("DOIGET_ARXIV_BASE", server.uri())
        .assert()
        .success()
        // arXiv has no Crossref `type`, so it renders as @misc.
        .stdout(contains("@misc{"))
        .stdout(contains("Example arXiv Paper Title"))
        // issue #303: a complete arXiv entry, not a title+author stub —
        // year from the Atom `published`, the preprint identity, and the
        // primary subject class from the first `<category>`.
        .stdout(contains("year       = {2024},"))
        .stdout(contains("= {2401.12345},")) // eprint value (short key padding)
        .stdout(contains("archivePrefix = {arXiv},"))
        .stdout(contains("primaryClass = {cs.LG},"));
}

#[tokio::test]
async fn cite_arxiv_with_published_doi_merges_to_article() {
    // Issue #303 published-version merge: an arXiv Atom feed that
    // cross-references a published journal DOI (`<arxiv:doi>`) cites as the
    // rich `@article` (journal / volume / doi from Crossref) with the arXiv
    // preprint identity retained (eprint / archivePrefix / primaryClass).
    let atom = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom" xmlns:arxiv="http://arxiv.org/schemas/atom">
  <entry>
    <id>http://arxiv.org/abs/2401.12345v1</id>
    <title>Preprint Title</title>
    <author><name>Jane Doe</name></author>
    <published>2024-01-15T00:00:00Z</published>
    <category term="cond-mat.str-el" scheme="http://arxiv.org/schemas/atom"/>
    <arxiv:doi>{TEST_DOI}</arxiv:doi>
  </entry>
</feed>"#
    );

    let arxiv = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/query"))
        .respond_with(ResponseTemplate::new(200).set_body_string(atom))
        .mount(&arxiv)
        .await;
    let crossref = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/works/{TEST_DOI}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(crossref_body()))
        .mount(&crossref)
        .await;

    let dir = TempDir::new().expect("tempdir");
    doiget(&dir)
        .args(["cite", "arxiv:2401.12345"])
        .env("DOIGET_ARXIV_BASE", arxiv.uri())
        .env("DOIGET_CROSSREF_BASE", crossref.uri())
        .assert()
        .success()
        // Merged to the published @article (Crossref fields win) ...
        .stdout(contains("@article{"))
        .stdout(contains("journal    = {Synthetic Journal of Physics},"))
        .stdout(contains("volume     = {42},"))
        .stdout(contains("doi        = {10.1234/cite.test},"))
        // ... with the arXiv preprint identity retained.
        .stdout(contains("archivePrefix = {arXiv},"))
        .stdout(contains("= {2401.12345},")) // eprint
        .stdout(contains("primaryClass = {cond-mat.str-el},"));
}

#[tokio::test]
async fn cite_arxiv_shaped_cross_ref_is_ignored_no_second_resolve() {
    // Fix #318-D: the published-version merge must trigger ONLY on a bare
    // DOI cross-ref. An `<arxiv:doi>` carrying an arXiv-shaped value (a
    // malformed feed) parses as `Ref::Arxiv` and must hit the `=> None`
    // guard arm — NOT be resolved as if it were a published DOI — so cite
    // keeps the `@misc` preprint and makes no spurious second arXiv call.
    let atom = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom" xmlns:arxiv="http://arxiv.org/schemas/atom">
  <entry>
    <id>http://arxiv.org/abs/2401.12345v1</id>
    <title>Preprint Title</title>
    <author><name>Jane Doe</name></author>
    <published>2024-01-15T00:00:00Z</published>
    <category term="cond-mat.str-el" scheme="http://arxiv.org/schemas/atom"/>
    <arxiv:doi>arxiv:2401.99999</arxiv:doi>
  </entry>
</feed>"#;

    let arxiv = MockServer::start().await;
    // `expect(1)`: the feed is fetched exactly once. A broken guard would
    // resolve the arXiv-shaped cross-ref and hit `/api/query` a second time,
    // failing this expectation on server drop.
    Mock::given(method("GET"))
        .and(path("/api/query"))
        .respond_with(ResponseTemplate::new(200).set_body_string(atom))
        .expect(1)
        .mount(&arxiv)
        .await;

    let dir = TempDir::new().expect("tempdir");
    doiget(&dir)
        .args(["cite", "arxiv:2401.12345"])
        .env("DOIGET_ARXIV_BASE", arxiv.uri())
        // No Crossref base: a correct guard never reaches a DOI resolver.
        .assert()
        .success()
        // Kept the preprint @misc, with its primary class from the feed.
        .stdout(contains("@misc{"))
        .stdout(contains("primaryClass = {cond-mat.str-el},"));
}

#[tokio::test]
async fn cite_unresolved_doi_fails() {
    let server = MockServer::start().await;
    // 404 for the works endpoint and the unpaywall fallback → unresolved.
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let dir = TempDir::new().expect("tempdir");
    doiget(&dir)
        .args(["cite", TEST_DOI])
        .env("DOIGET_CROSSREF_BASE", server.uri())
        .env("DOIGET_UNPAYWALL_BASE", server.uri())
        .assert()
        .failure();
}

#[tokio::test]
async fn cite_arxiv_published_doi_resolve_failure_notes_and_falls_back() {
    // Fix #318-C: when the cross-referenced published DOI fails to resolve,
    // cite emits a VISIBLE note on stderr and falls back to the @misc
    // preprint, rather than silently dropping the published-version upgrade.
    let atom = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom" xmlns:arxiv="http://arxiv.org/schemas/atom">
  <entry>
    <id>http://arxiv.org/abs/2401.12345v1</id>
    <title>Preprint Title</title>
    <author><name>Jane Doe</name></author>
    <published>2024-01-15T00:00:00Z</published>
    <arxiv:doi>{TEST_DOI}</arxiv:doi>
  </entry>
</feed>"#
    );
    let arxiv = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/query"))
        .respond_with(ResponseTemplate::new(200).set_body_string(atom))
        .mount(&arxiv)
        .await;

    let dir = TempDir::new().expect("tempdir");
    let assert = doiget(&dir)
        .args(["cite", "arxiv:2401.12345"])
        .env("DOIGET_ARXIV_BASE", arxiv.uri())
        // Crossref closed → the cross-referenced DOI resolve fails.
        .env("DOIGET_CROSSREF_BASE", "http://127.0.0.1:1/")
        .env("DOIGET_UNPAYWALL_BASE", "http://127.0.0.1:1/")
        .assert()
        .success();
    let out = assert.get_output();
    let stdout = String::from_utf8(out.stdout.clone()).expect("stdout utf-8");
    let stderr = String::from_utf8(out.stderr.clone()).expect("stderr utf-8");
    assert!(stdout.contains("@misc{"), "fell back to preprint: {stdout}");
    assert!(
        stderr.contains("published-version DOI resolve failed"),
        "the degradation must be visible: {stderr}"
    );
}
