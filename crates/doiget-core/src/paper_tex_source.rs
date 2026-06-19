//! Fetch the raw **LaTeX source** of an arXiv paper from the arXiv source API
//! (`https://export.arxiv.org/src/<id>`).
//!
//! This is the complement to [`crate::paper_text`] (ar5iv HTML extraction):
//! ar5iv renders only papers that have been through the LaTeXML pipeline; the
//! source API is always available for arXiv submissions that have a TeX source
//! (PDF-only submissions return `NO_OA_AVAILABLE`).
//!
//! ## Why TeX source instead of ar5iv HTML?
//!
//! ar5iv HTML extraction (`paper_text`) is best-effort and unavailable for
//! papers that were never processed by LaTeXML. The raw TeX source — when the
//! submission has one — is the authoritative structured text. LLMs handle
//! LaTeX well: `\section{}`, `\begin{equation}…\end{equation}`, etc. provide
//! explicit structure that is often more reliable than ar5iv's HTML rendering.
//!
//! ## Source (arXiv E-print API)
//!
//! arXiv serves submission sources at
//! `https://export.arxiv.org/src/<arxiv_id>`. The response is:
//!
//! - **Gzip'd tar** for multi-file submissions.
//! - **Gzip'd single file** for single-file submissions.
//! - **Raw PDF bytes** (`%PDF-` magic) for PDF-only submissions — no TeX
//!   source available; yields `TextUnavailable`.
//!
//! Detection is by magic bytes on the response body.
//!
//! ## Source key
//!
//! Uses the existing `"arxiv"` HTTP source key (registered in
//! [`crate::http::tier_1_allowlist`]), which covers `export.arxiv.org`.
//! The provenance `source` field is labelled `"arxiv-src"` to distinguish
//! TeX-source fetches from PDF fetches in the audit trail.
//!
//! ## Capability tier
//!
//! Tier 1 OA metadata, **always-on**: no env gate, no Cargo feature gate.
//! Read-only, open-access, same posture class as [`crate::paper_text`]
//! (ADR-0032 D2). TeX source is a structured text artifact, never a PDF
//! reinterpretation (ADR-0032 D1 carve).
//!
//! ## Caching
//!
//! Results are cached at `<cache_root>/tex-src/<safekey>.json`. Best-effort:
//! cache failures degrade to a re-fetch, never an error.

use camino::{Utf8Path, Utf8PathBuf};
use chrono::{DateTime, Duration, Utc};
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use std::io::Read;
use tar::Archive;
use url::Url;

use crate::provenance::{Capability, LogEvent, LogResult, RowInput};
use crate::source::{FetchContext, FetchError};
use crate::{ArxivId, Ref};

/// HTTP-client source key. Reuses `"arxiv"` (covers `export.arxiv.org`).
const HTTP_SOURCE_KEY: &str = "arxiv";

/// Provenance audit label for TeX-source fetches.
const PROV_SOURCE_LABEL: &str = "arxiv-src";

/// Production arXiv source API base. Overridable via
/// `DOIGET_ARXIV_SRC_BASE` for tests.
pub const ARXIV_SRC_DEFAULT_BASE: &str = "https://export.arxiv.org";

// arXiv sources can be revised (v2, v3 can appear within days of v1); 7 days
// balances freshness against re-fetch cost for stable papers.
const TEX_SRC_CACHE_TTL_DAYS: i64 = 7;
const TEX_SRC_CACHE_SCHEMA_VERSION: &str = "1.0";

#[derive(Debug, Serialize, Deserialize)]
struct CacheEntry {
    schema_version: String,
    /// RFC 3339 timestamp; matches `fetched_at` in `TextCacheEntry` and
    /// `resolver_cache::CacheEntry` for consistency across cache formats.
    fetched_at: String,
    /// Stored explicitly so future versions can adjust per-entry TTL on read
    /// without a code change (matches the pattern in `resolver_cache`).
    ttl_seconds: i64,
    inner: PaperTexSource,
}

/// Typed result from [`extract_tex`].
///
/// Using a named struct prevents accidental `(content, main_file)` swap bugs
/// at the destructuring site (both members are `String` / `Option<String>`
/// and would compile silently if swapped).
#[derive(Debug)]
pub(crate) struct ExtractedTex {
    /// Filename of the main `.tex` file within the source tarball.
    /// `None` when the submission was a single gzip'd file (no tar wrapper).
    pub main_file: Option<String>,
    /// Raw LaTeX content of the selected file.
    pub content: String,
}

/// The raw LaTeX source of an arXiv paper.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PaperTexSource {
    /// The arXiv id this source belongs to.
    pub arxiv_id: String,
    /// Filename of the main `.tex` file within the source tarball.
    /// `None` when the submission was a single gzip'd file (no tar).
    pub main_file: Option<String>,
    /// Raw LaTeX source of the main `.tex` file.
    pub tex_source: String,
    /// `char`s in `tex_source` (after any truncation).
    pub char_count: usize,
    /// `true` when `max_chars` truncated the source.
    pub truncated: bool,
    /// Final URL the source was retrieved from (after redirects).
    pub retrieved_from: String,
}

/// Fetch the LaTeX source of an arXiv paper.
///
/// `base` is the arXiv export base URL (production [`ARXIV_SRC_DEFAULT_BASE`];
/// tests inject a wiremock origin). `max_chars` caps the returned
/// `tex_source` character count (`None` = no cap); truncation is flagged,
/// never silent.
///
/// # Errors
///
/// - [`FetchError::Http`] — transport / status failure.
/// - [`FetchError::TextUnavailable`] — PDF-only submission or no `.tex` files
///   in the tarball.
/// - [`FetchError::SourceSchema`] — URL construction or gzip/tar parse error.
/// - [`FetchError::Log`] — provenance append failed (fail-closed).
pub async fn paper_tex_source(
    base: &Url,
    id: &ArxivId,
    max_chars: Option<usize>,
    ctx: &FetchContext,
) -> Result<PaperTexSource, FetchError> {
    if let Some(root) = &ctx.cache_root {
        if let Some(full) = cache_read(root, id) {
            return Ok(apply_max_chars(full, max_chars));
        }
    }

    let full = fetch_and_extract(base, id, ctx).await?;

    if let Some(root) = &ctx.cache_root {
        if !cache_write(root, id, &full) {
            tracing::warn!(
                cache_root = %root,
                arxiv_id = %id.as_str(),
                "tex-source cache write failed; next request will re-fetch"
            );
        }
    }

    Ok(apply_max_chars(full, max_chars))
}

async fn fetch_and_extract(
    base: &Url,
    id: &ArxivId,
    ctx: &FetchContext,
) -> Result<PaperTexSource, FetchError> {
    let _permit = ctx.rate_limiter.acquire(HTTP_SOURCE_KEY).await;

    let url = src_url(base, id)?;
    let (body, final_url) = ctx.http.fetch_bytes(HTTP_SOURCE_KEY, url).await?;

    let extracted = extract_tex(id, &body)?;
    let char_count = extracted.content.chars().count();

    let canonical = Ref::Arxiv(id.clone())
        .promote(PROV_SOURCE_LABEL, None)
        .digest_hex();
    ctx.log.append(RowInput {
        event: LogEvent::Fetch,
        result: LogResult::Ok,
        capability: Capability::Oa,
        ref_: Some(id.as_str()),
        source: Some(PROV_SOURCE_LABEL),
        error_code: None,
        size_bytes: Some(body.len() as u64),
        license: Some("arxiv-default"),
        store_path: None,
        canonical_digest: Some(&canonical),
    })?;

    Ok(PaperTexSource {
        arxiv_id: id.as_str().to_string(),
        main_file: extracted.main_file,
        tex_source: extracted.content,
        char_count,
        truncated: false,
        retrieved_from: final_url.to_string(),
    })
}

fn src_url(base: &Url, id: &ArxivId) -> Result<Url, FetchError> {
    base.join(&format!("/src/{}", id.as_str()))
        .map_err(|e| FetchError::SourceSchema {
            hint: format!("arXiv src URL construction failed: {e}"),
        })
}

/// Detect content type by magic bytes and extract the main LaTeX source.
///
/// Returns an [`ExtractedTex`] with `main_file` and `content`.
pub(crate) fn extract_tex(
    id: &ArxivId,
    bytes: &[u8],
) -> Result<ExtractedTex, FetchError> {
    // PDF-only submission — no TeX source available.
    if bytes.starts_with(b"%PDF-") {
        return Err(FetchError::TextUnavailable {
            arxiv_id: id.clone(),
        });
    }

    // arXiv occasionally serves a bare uncompressed .tex file for trivial
    // single-file submissions that were not gzip-compressed by the submitter.
    if bytes.len() < 2 || bytes[0..2] != [0x1f, 0x8b] {
        let text = String::from_utf8_lossy(bytes).into_owned();
        if text.trim().is_empty() {
            return Err(FetchError::TextUnavailable {
                arxiv_id: id.clone(),
            });
        }
        return Ok(ExtractedTex {
            main_file: None,
            content: text,
        });
    }

    // Decompress gzip.
    let mut gz = GzDecoder::new(std::io::Cursor::new(bytes));
    let mut decompressed = Vec::new();
    gz.read_to_end(&mut decompressed)
        .map_err(|e| FetchError::SourceSchema {
            hint: format!("gzip decompress of arXiv src failed: {e}"),
        })?;

    // UStar tar detection: POSIX.1-1988 tar header magic at byte offset 257.
    // A valid tar header is ≥ 512 bytes; the `> 262` guard is conservative
    // (only 262 bytes are needed for the magic slice) and avoids a panic.
    let is_tar = decompressed.len() > 262 && &decompressed[257..262] == b"ustar";

    if is_tar {
        extract_from_tar(id, &decompressed)
    } else {
        let text = String::from_utf8_lossy(&decompressed).into_owned();
        if text.trim().is_empty() {
            return Err(FetchError::TextUnavailable {
                arxiv_id: id.clone(),
            });
        }
        Ok(ExtractedTex {
            main_file: None,
            content: text,
        })
    }
}

/// Extract the main `.tex` file from an uncompressed tar archive using a
/// weighted scoring heuristic:
///
///   score = (1 if `\documentclass` present) × 1_000_000
///         + (1 if filename ends with `main.tex`) × 100_000
///         + byte_count_of_file
///
/// The weights are sized to dominate any realistic file size: a `.tex` file
/// with `\documentclass` always beats one without it unless the file exceeds
/// ~1 GB (byte count overflows `i64`), which is not a realistic `.tex` size.
/// Within tied `\documentclass` files, `main.tex` always wins unless the
/// competing file exceeds 100 KB — also not a realistic sub-file size.
fn extract_from_tar(
    id: &ArxivId,
    bytes: &[u8],
) -> Result<ExtractedTex, FetchError> {
    let mut archive = Archive::new(std::io::Cursor::new(bytes));
    let entries = archive.entries().map_err(|e| FetchError::SourceSchema {
        hint: format!("tar read failed: {e}"),
    })?;

    let mut tex_files: Vec<(String, String)> = Vec::new();
    // Track .tex entries attempted (even if read failed) so that a corrupt
    // archive is distinguishable from a PDF-only submission.
    let mut tex_attempted: usize = 0;
    for entry in entries {
        let Ok(mut entry) = entry else { continue };
        let path = match entry.path() {
            Ok(p) => p.to_string_lossy().to_string(),
            Err(_) => continue,
        };
        if !path.ends_with(".tex") {
            continue;
        }
        tex_attempted += 1;
        let mut content = String::new();
        if entry.read_to_string(&mut content).is_ok() && !content.trim().is_empty() {
            tex_files.push((path, content));
        }
    }

    if tex_files.is_empty() {
        // Distinguish "PDF-only" from "corrupt archive": if .tex entries were
        // present but none could be read, this is a schema/decode error, not a
        // missing-source condition (which would mislead agents into thinking
        // the paper has no TeX source).
        return Err(if tex_attempted > 0 {
            FetchError::SourceSchema {
                hint: format!(
                    "tar contained {tex_attempted} .tex entries but all failed to read"
                ),
            }
        } else {
            FetchError::TextUnavailable {
                arxiv_id: id.clone(),
            }
        });
    }

    let best = tex_files
        .into_iter()
        .max_by_key(|(name, content)| {
            let docclass = i64::from(content.contains(r"\documentclass")) * 1_000_000;
            let is_main =
                i64::from(name.ends_with("main.tex") || name == "main.tex") * 100_000;
            let size = i64::try_from(content.len()).unwrap_or(i64::MAX);
            docclass + is_main + size
        });

    match best {
        Some((name, content)) => Ok(ExtractedTex {
            main_file: Some(name),
            content,
        }),
        None => Err(FetchError::TextUnavailable {
            arxiv_id: id.clone(),
        }),
    }
}

fn apply_max_chars(mut full: PaperTexSource, max_chars: Option<usize>) -> PaperTexSource {
    let Some(max) = max_chars else {
        return full;
    };
    if full.char_count <= max {
        return full;
    }
    full.tex_source = full.tex_source.chars().take(max).collect();
    full.char_count = max;
    full.truncated = true;
    full
}

fn cache_file(cache_root: &Utf8Path, id: &ArxivId) -> Utf8PathBuf {
    let safekey = Ref::Arxiv(id.clone()).safekey();
    cache_root
        .join("tex-src")
        .join(format!("{}.json", safekey.as_str()))
}

fn cache_read(cache_root: &Utf8Path, id: &ArxivId) -> Option<PaperTexSource> {
    cache_read_at(cache_root, id, Utc::now())
}

fn cache_read_at(
    cache_root: &Utf8Path,
    id: &ArxivId,
    now: DateTime<Utc>,
) -> Option<PaperTexSource> {
    let path = cache_file(cache_root, id);
    let bytes = std::fs::read(&path).ok()?;
    let entry: CacheEntry = serde_json::from_slice(&bytes).ok()?;
    if entry.schema_version != TEX_SRC_CACHE_SCHEMA_VERSION {
        return None;
    }
    let fetched = DateTime::parse_from_rfc3339(&entry.fetched_at)
        .ok()?
        .with_timezone(&Utc);
    if now.signed_duration_since(fetched) > Duration::seconds(entry.ttl_seconds) {
        return None;
    }
    Some(entry.inner)
}

fn cache_write(cache_root: &Utf8Path, id: &ArxivId, full: &PaperTexSource) -> bool {
    cache_write_at(cache_root, id, full, Utc::now())
}

fn cache_write_at(
    cache_root: &Utf8Path,
    id: &ArxivId,
    full: &PaperTexSource,
    now: DateTime<Utc>,
) -> bool {
    let path = cache_file(cache_root, id);
    if let Some(dir) = path.parent() {
        if std::fs::create_dir_all(dir).is_err() {
            return false;
        }
    }
    let entry = CacheEntry {
        schema_version: TEX_SRC_CACHE_SCHEMA_VERSION.to_string(),
        fetched_at: now.to_rfc3339(),
        ttl_seconds: TEX_SRC_CACHE_TTL_DAYS * 86_400,
        inner: full.clone(),
    };
    match serde_json::to_vec(&entry) {
        Ok(bytes) => std::fs::write(&path, bytes).is_ok(),
        Err(_) => false,
    }
}

/// Resolve the arXiv source base URL.
pub fn resolve_arxiv_src_base() -> Result<Url, String> {
    let raw = std::env::var("DOIGET_ARXIV_SRC_BASE")
        .unwrap_or_else(|_| ARXIV_SRC_DEFAULT_BASE.to_string());
    Url::parse(&raw).map_err(|e| format!("DOIGET_ARXIV_SRC_BASE is not a valid URL: {e}"))
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    missing_docs
)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write as _;

    fn make_id(s: &str) -> ArxivId {
        match Ref::parse(s).expect("parse") {
            Ref::Arxiv(a) => a,
            _ => panic!("expected arxiv id"),
        }
    }

    fn gzip_bytes(data: &[u8]) -> Vec<u8> {
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(data).expect("gzip write");
        enc.finish().expect("gzip finish")
    }

    fn tar_gzip(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, data) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, name, std::io::Cursor::new(data))
                .expect("tar append");
        }
        gzip_bytes(&builder.into_inner().expect("tar finish"))
    }

    fn make_src(id: &ArxivId) -> PaperTexSource {
        PaperTexSource {
            arxiv_id: id.as_str().to_string(),
            main_file: Some("main.tex".into()),
            tex_source: "\\documentclass{article}".into(),
            char_count: 23,
            truncated: false,
            retrieved_from: "https://export.arxiv.org/src/2401.12345".into(),
        }
    }

    // ── apply_max_chars ───────────────────────────────────────────────────────

    #[test]
    fn apply_max_chars_no_cap_is_identity() {
        let id = make_id("2401.12345");
        let src = make_src(&id);
        let out = apply_max_chars(src.clone(), None);
        assert_eq!(out, src);
    }

    #[test]
    fn apply_max_chars_truncates() {
        let id = make_id("2401.12345");
        let src = PaperTexSource {
            arxiv_id: id.as_str().to_string(),
            main_file: None,
            tex_source: "abcdefghij".into(),
            char_count: 10,
            truncated: false,
            retrieved_from: "https://export.arxiv.org/src/2401.12345".into(),
        };
        let out = apply_max_chars(src, Some(4));
        assert_eq!(out.tex_source, "abcd");
        assert_eq!(out.char_count, 4);
        assert!(out.truncated);
    }

    // ── extract_tex: magic-byte paths ────────────────────────────────────────

    #[test]
    fn pdf_only_yields_text_unavailable() {
        let id = make_id("2401.12345");
        let result = extract_tex(&id, b"%PDF-1.4 fake");
        assert!(matches!(result, Err(FetchError::TextUnavailable { .. })));
    }

    #[test]
    fn raw_tex_passthrough() {
        let id = make_id("2401.12345");
        let tex = b"\\documentclass{article}\n\\begin{document}\nHello.\\end{document}";
        let ext = extract_tex(&id, tex).expect("extract");
        assert!(ext.main_file.is_none());
        assert!(ext.content.contains("\\documentclass"));
    }

    #[test]
    fn gzip_single_file_extracted() {
        let id = make_id("2401.12345");
        let tex = b"\\documentclass{article}\n\\begin{document}Hello\\end{document}";
        let gz = gzip_bytes(tex);
        let ext = extract_tex(&id, &gz).expect("extract");
        assert!(ext.main_file.is_none(), "single gzip has no tar filename");
        assert!(ext.content.contains("\\documentclass"));
    }

    // ── extract_from_tar: selection heuristic ────────────────────────────────

    #[test]
    fn tar_selects_documentclass_file_over_plain() {
        let id = make_id("2401.12345");
        let payload = tar_gzip(&[
            ("paper.tex", b"\\documentclass{article} main content"),
            ("macros.tex", b"\\newcommand{\\foo}{bar}"),
        ]);
        let ext = extract_tex(&id, &payload).expect("extract");
        assert_eq!(ext.main_file.as_deref(), Some("paper.tex"));
        assert!(ext.content.contains("\\documentclass"));
    }

    #[test]
    fn tar_prefers_main_tex_among_documentclass_files() {
        let id = make_id("2401.12345");
        let payload = tar_gzip(&[
            ("other.tex", b"\\documentclass{article} other content here"),
            ("main.tex", b"\\documentclass{article} main"),
        ]);
        let ext = extract_tex(&id, &payload).expect("extract");
        assert_eq!(
            ext.main_file.as_deref(),
            Some("main.tex"),
            "main.tex bonus must override smaller-but-also-documentclass other.tex"
        );
    }

    #[test]
    fn tar_falls_back_to_largest_file_when_no_documentclass() {
        let id = make_id("2401.12345");
        let short = b"\\section{Short}".as_slice();
        let mut long_content = b"\\section{Long} ".to_vec();
        long_content.extend(vec![b'x'; 500]);
        let payload = tar_gzip(&[("short.tex", short), ("long.tex", &long_content)]);
        let ext = extract_tex(&id, &payload).expect("extract");
        assert_eq!(ext.main_file.as_deref(), Some("long.tex"));
    }

    #[test]
    fn tar_with_no_tex_files_is_text_unavailable() {
        let id = make_id("2401.12345");
        let payload = tar_gzip(&[("README.md", b"# Paper"), ("figure.eps", b"%!PS")]);
        let err = extract_tex(&id, &payload).expect_err("should fail");
        assert!(matches!(err, FetchError::TextUnavailable { .. }));
    }

    // ── cache ────────────────────────────────────────────────────────────────

    #[test]
    fn resolve_base_defaults_to_production() {
        if std::env::var("DOIGET_ARXIV_SRC_BASE").is_err() {
            let u = resolve_arxiv_src_base().expect("resolve");
            assert_eq!(u.as_str(), "https://export.arxiv.org/");
        }
    }

    #[test]
    fn cache_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root =
            camino::Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("utf8");
        let id = make_id("2401.12345");
        let src = make_src(&id);
        assert!(cache_write(&root, &id, &src));
        let read = cache_read(&root, &id).expect("cache hit");
        assert_eq!(read, src);
    }

    #[test]
    fn cache_expired_returns_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root =
            camino::Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("utf8");
        let id = make_id("2401.12345");
        let src = PaperTexSource {
            arxiv_id: id.as_str().to_string(),
            main_file: None,
            tex_source: "test".into(),
            char_count: 4,
            truncated: false,
            retrieved_from: "https://export.arxiv.org/src/2401.12345".into(),
        };
        let past = Utc::now() - Duration::days(TEX_SRC_CACHE_TTL_DAYS + 1);
        assert!(cache_write_at(&root, &id, &src, past));
        assert!(cache_read_at(&root, &id, Utc::now()).is_none());
    }

    #[test]
    fn cache_schema_version_mismatch_returns_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root =
            camino::Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("utf8");
        let id = make_id("2401.12345");
        let src = make_src(&id);
        // Write a stale-schema entry manually.
        let bad = serde_json::json!({
            "schema_version": "0.9",
            "fetched_at": Utc::now().to_rfc3339(),
            "ttl_seconds": 86_400 * 7i64,
            "inner": src,
        });
        let path = cache_file(&root, &id);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, serde_json::to_vec(&bad).expect("json")).expect("write");
        assert!(
            cache_read_at(&root, &id, Utc::now()).is_none(),
            "stale schema version must be rejected"
        );
    }
}
