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

/// Provenance audit label for source-bundle / figure fetches (ADR-0034 I4),
/// distinct from [`PROV_SOURCE_LABEL`] so the audit log tells a `tex-source`
/// text fetch apart from a `source` bundle/figures fetch.
const PROV_SOURCE_BUNDLE_LABEL: &str = "arxiv-src-bundle";

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
pub(crate) fn extract_tex(id: &ArxivId, bytes: &[u8]) -> Result<ExtractedTex, FetchError> {
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
fn extract_from_tar(id: &ArxivId, bytes: &[u8]) -> Result<ExtractedTex, FetchError> {
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
                hint: format!("tar contained {tex_attempted} .tex entries but all failed to read"),
            }
        } else {
            FetchError::TextUnavailable {
                arxiv_id: id.clone(),
            }
        });
    }

    let best = tex_files.into_iter().max_by_key(|(name, content)| {
        let docclass = i64::from(content.contains(r"\documentclass")) * 1_000_000;
        let is_main = i64::from(name.ends_with("main.tex") || name == "main.tex") * 100_000;
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

// ─────────────────────────────────────────────────────────────────────────────
// Source bundle / figures (ADR-0034). The arXiv `/src/<id>` tarball already
// downloaded for the text path carries EVERY submission file; this section
// surfaces the full bundle (or figures only) instead of discarding them. Every
// returned path is sanitised (relative, no `..`, no anchor) so a caller can
// join it under any output directory without escaping it (zip-slip, ADR-0034 D3).
// ─────────────────────────────────────────────────────────────────────────────

/// Image/figure file extensions (lowercase, no dot). Saved opaque — never
/// interpreted (ADR-0034 D2). Vector `.pdf` figures are included.
const FIGURE_EXTS: &[&str] = &["pdf", "eps", "ps", "png", "jpg", "jpeg", "gif", "svg"];

/// Cap on the DECOMPRESSED size of an arXiv `/src/` tarball (ADR-0034 I5).
/// The HTTP client already caps the *compressed* download at `PDF_MAX_BYTES`
/// (100 MB), but a small gzip can expand to many GB; this bounds the
/// decompressed bytes held in memory to refuse a gzip bomb. Generous vs real
/// arXiv sources (rarely > 100 MB decompressed), strict vs a multi-GB bomb.
const SRC_MAX_DECOMPRESSED_BYTES: u64 = 500_000_000;

/// Which subset of the source tarball to materialise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BundleFilter {
    /// Every regular file in the tarball.
    All,
    /// Only image/figure files (by the [`FIGURE_EXTS`] allowlist).
    FiguresOnly,
}

/// One file extracted from an arXiv source tarball.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    /// Sanitised **relative** path (never absolute, never contains `..`),
    /// safe to join under any output root (ADR-0034 D3). The field is
    /// `pub(crate)` so it can only be set by [`extract_bundle`] — which runs
    /// every path through [`sanitize_entry_path`] — and an external caller
    /// cannot forge a `SourceFile` carrying an unsafe path. This mirrors the
    /// checked-construction pattern of `Doi` / `ArxivId`. Read it via
    /// [`SourceFile::path`].
    pub(crate) path: Utf8PathBuf,
    /// Raw file bytes, opaque (never interpreted; ADR-0034 D2).
    pub bytes: Vec<u8>,
}

impl SourceFile {
    /// The sanitised relative path of this file (never absolute, no `..`).
    #[must_use]
    pub fn path(&self) -> &Utf8Path {
        &self.path
    }
}

/// Sanitise a raw tar entry path into a safe **relative** path, or `None` to
/// reject it (zip-slip / path-traversal guard, ADR-0034 D3).
///
/// Rejects: absolute / root-anchored paths (leading `/` or `\`, or a Windows
/// drive prefix like `C:`); any `..` component; any component containing `:`
/// or a NUL byte; and paths with no normal component. Splits on BOTH `/` and
/// `\` so a Windows-style traversal in a Unix-produced tar is caught
/// regardless of the extracting platform. The result is always relative with
/// no `..`, so `root.join(result)` cannot escape `root`.
fn sanitize_entry_path(raw: &str) -> Option<Utf8PathBuf> {
    if raw.is_empty() || raw.contains('\0') {
        return None;
    }
    // Absolute / root-anchored — anomalous in an arXiv source tarball.
    if raw.starts_with('/') || raw.starts_with('\\') {
        return None;
    }
    let b = raw.as_bytes();
    // Windows drive prefix `X:` / `X:\`.
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        return None;
    }
    let mut out = Utf8PathBuf::new();
    let mut any = false;
    for seg in raw.split(|c: char| c == '/' || c == '\\') {
        match seg {
            "" | "." => continue, // collapse `//`, drop `.`
            ".." => return None,  // traversal — reject the whole path
            s => {
                if s.contains(':') || s.contains('\0') {
                    return None;
                }
                out.push(s);
                any = true;
            }
        }
    }
    if any {
        Some(out)
    } else {
        None
    }
}

/// True when `path`'s extension is in the figure allowlist (case-insensitive).
fn is_figure(path: &Utf8Path) -> bool {
    match path.extension() {
        Some(ext) => FIGURE_EXTS.contains(&ext.to_ascii_lowercase().as_str()),
        None => false,
    }
}

/// Decompress + untar an arXiv `/src/` body and collect the selected files.
///
/// Applies the same PDF / gzip / ustar magic-byte checks as [`extract_tex`],
/// but a PDF-only response, a bare uncompressed file, or a single gzip'd file
/// yields [`FetchError::SourceUnavailable`] — there is no multi-file bundle in
/// a single-file response (unlike the text path, which passes a bare `.tex`
/// through; the existing `extract_tex` text path is left byte-identical,
/// ADR-0034 D6). Decompression is size-capped against a gzip bomb (ADR-0034
/// I5). Only **regular** tar entries are considered — symlinks / hardlinks /
/// devices are skipped (a symlink is itself a traversal vector, ADR-0034 D3).
/// Every path is run through [`sanitize_entry_path`]; a path-rejected entry is
/// skipped with a `tracing::warn!`. Entries that fail to read (malformed
/// header, non-decodable path, or `read_to_end` error) are skipped, logged,
/// and counted, so an empty result distinguishes a corrupt archive
/// ([`FetchError::SourceSchema`]) from genuinely no matching files
/// ([`FetchError::SourceUnavailable`]) (ADR-0034 C1).
pub(crate) fn extract_bundle(
    id: &ArxivId,
    bytes: &[u8],
    filter: BundleFilter,
) -> Result<Vec<SourceFile>, FetchError> {
    // PDF-only / bare single file: no multi-file bundle.
    if bytes.starts_with(b"%PDF-") || bytes.len() < 2 || bytes[0..2] != [0x1f, 0x8b] {
        return Err(no_files(id, filter));
    }
    // Decompress with a hard cap on the OUTPUT size (ADR-0034 I5): the HTTP
    // client already bounds the COMPRESSED download (PDF_MAX_BYTES); `take`
    // bounds the decompressed bytes so a gzip bomb cannot exhaust memory.
    let mut gz =
        GzDecoder::new(std::io::Cursor::new(bytes)).take(SRC_MAX_DECOMPRESSED_BYTES + 1);
    let mut decompressed = Vec::new();
    gz.read_to_end(&mut decompressed)
        .map_err(|e| FetchError::SourceSchema {
            hint: format!("gzip decompress of arXiv src failed: {e}"),
        })?;
    if decompressed.len() as u64 > SRC_MAX_DECOMPRESSED_BYTES {
        return Err(FetchError::SourceSchema {
            hint: format!(
                "arXiv src decompressed size exceeds {SRC_MAX_DECOMPRESSED_BYTES} bytes \
                 (possible gzip bomb); refusing"
            ),
        });
    }
    let is_tar = decompressed.len() > 262 && &decompressed[257..262] == b"ustar";
    if !is_tar {
        // Single gzip'd file (e.g. one .tex) — no bundle / figures.
        return Err(no_files(id, filter));
    }

    let mut archive = Archive::new(std::io::Cursor::new(decompressed));
    let entries = archive.entries().map_err(|e| FetchError::SourceSchema {
        hint: format!("tar read failed: {e}"),
    })?;

    let mut files: Vec<SourceFile> = Vec::new();
    // Count entries we matched but could not materialise, so an empty result
    // distinguishes a corrupt/unreadable archive (SourceSchema) from a
    // genuinely absent bundle (SourceUnavailable) — mirrors
    // extract_from_tar's tex_attempted (ADR-0034 C1). Path-rejected
    // (zip-slip) and filtered-out entries are deliberate skips, NOT counted.
    let mut unreadable: usize = 0;
    for entry in entries {
        let mut entry = match entry {
            Ok(e) => e,
            Err(e) => {
                unreadable += 1;
                tracing::warn!(arxiv_id = %id.as_str(), error = %e, "arXiv src: skipping malformed tar entry");
                continue;
            }
        };
        // Regular files only: a symlink/hardlink entry is a traversal vector
        // and is never needed for source/figures (ADR-0034 D3).
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let raw_path = match entry.path() {
            Ok(p) => p.to_string_lossy().into_owned(),
            Err(e) => {
                unreadable += 1;
                tracing::warn!(arxiv_id = %id.as_str(), error = %e, "arXiv src: tar entry has a non-decodable path; skipping");
                continue;
            }
        };
        let Some(safe) = sanitize_entry_path(&raw_path) else {
            tracing::warn!(
                entry = %raw_path,
                "arXiv src: rejected unsafe tar entry path (zip-slip guard)"
            );
            continue;
        };
        if filter == BundleFilter::FiguresOnly && !is_figure(&safe) {
            continue;
        }
        let mut buf = Vec::new();
        match entry.read_to_end(&mut buf) {
            Ok(_) => files.push(SourceFile {
                path: safe,
                bytes: buf,
            }),
            Err(e) => {
                unreadable += 1;
                tracing::warn!(arxiv_id = %id.as_str(), entry = %safe, error = %e, "arXiv src: failed to read tar entry; skipping");
            }
        }
    }

    if files.is_empty() {
        // Corrupt/unreadable archive vs genuinely no matching files (ADR-0034 C1).
        return Err(if unreadable > 0 {
            FetchError::SourceSchema {
                hint: format!(
                    "arXiv src tar had {unreadable} unreadable entr(y/ies) and no usable files"
                ),
            }
        } else {
            no_files(id, filter)
        });
    }
    if unreadable > 0 {
        tracing::warn!(
            arxiv_id = %id.as_str(),
            unreadable,
            extracted = files.len(),
            "arXiv src: bundle is partial — some entries were unreadable and skipped"
        );
    }
    Ok(files)
}

/// The "no usable files" error for a `source` fetch, labelled with the
/// requested representation so the message is accurate (not ar5iv-specific;
/// ADR-0034 I2).
fn no_files(id: &ArxivId, filter: BundleFilter) -> FetchError {
    FetchError::SourceUnavailable {
        arxiv_id: id.clone(),
        kind: match filter {
            BundleFilter::All => "source bundle",
            BundleFilter::FiguresOnly => "figures",
        },
    }
}

/// Fetch the arXiv source bundle (or figures only) for `id`.
///
/// Tier-1 OA, always-on (ADR-0034 D1). Performs the SAME single `/src/<id>`
/// request as [`paper_tex_source`] and returns the selected files **in
/// memory**; the caller writes them to disk. Every returned path is sanitised
/// (ADR-0034 D3). Not cached (ADR-0034 D5).
///
/// # Errors
///
/// - [`FetchError::Http`] — transport / status failure.
/// - [`FetchError::TextUnavailable`] — PDF-only / single-file submission, or no
///   matching files (e.g. `--figures-only` on a figure-less submission).
/// - [`FetchError::SourceSchema`] — URL construction or gzip/tar parse error.
/// - [`FetchError::Log`] — provenance append failed (fail-closed).
pub async fn paper_source_bundle(
    base: &Url,
    id: &ArxivId,
    filter: BundleFilter,
    ctx: &FetchContext,
) -> Result<Vec<SourceFile>, FetchError> {
    let _permit = ctx.rate_limiter.acquire(HTTP_SOURCE_KEY).await;

    let url = src_url(base, id)?;
    let (body, _final_url) = ctx.http.fetch_bytes(HTTP_SOURCE_KEY, url).await?;

    let files = extract_bundle(id, &body, filter)?;

    let canonical = Ref::Arxiv(id.clone())
        .promote(PROV_SOURCE_BUNDLE_LABEL, None)
        .digest_hex();
    ctx.log.append(RowInput {
        event: LogEvent::Fetch,
        result: LogResult::Ok,
        capability: Capability::Oa,
        ref_: Some(id.as_str()),
        source: Some(PROV_SOURCE_BUNDLE_LABEL),
        error_code: None,
        size_bytes: Some(body.len() as u64),
        license: Some("arxiv-default"),
        store_path: None,
        canonical_digest: Some(&canonical),
    })?;

    Ok(files)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, missing_docs)]
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
        let root = camino::Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("utf8");
        let id = make_id("2401.12345");
        let src = make_src(&id);
        assert!(cache_write(&root, &id, &src));
        let read = cache_read(&root, &id).expect("cache hit");
        assert_eq!(read, src);
    }

    #[test]
    fn cache_expired_returns_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = camino::Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("utf8");
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
        let root = camino::Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("utf8");
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

    // ── sanitize_entry_path: zip-slip / traversal guard (ADR-0034 D3) ─────────

    #[test]
    fn sanitize_accepts_normal_relative_paths() {
        assert_eq!(
            sanitize_entry_path("main.tex").map(|p| p.as_str().replace('\\', "/")),
            Some("main.tex".to_string())
        );
        assert_eq!(
            sanitize_entry_path("figs/diagram.png").map(|p| p.as_str().replace('\\', "/")),
            Some("figs/diagram.png".to_string())
        );
        // `.` segments dropped, `//` collapsed.
        assert_eq!(
            sanitize_entry_path("./a//b.tex").map(|p| p.as_str().replace('\\', "/")),
            Some("a/b.tex".to_string())
        );
    }

    #[test]
    fn sanitize_rejects_parent_traversal() {
        assert_eq!(sanitize_entry_path("../evil.tex"), None);
        assert_eq!(sanitize_entry_path("a/../../etc/passwd"), None);
        assert_eq!(sanitize_entry_path("sub/../x"), None);
    }

    #[test]
    fn sanitize_rejects_absolute_and_anchored() {
        assert_eq!(sanitize_entry_path("/etc/passwd"), None);
        assert_eq!(sanitize_entry_path("\\windows\\system32"), None);
        assert_eq!(sanitize_entry_path("C:\\Windows\\evil"), None);
        assert_eq!(sanitize_entry_path("C:/Windows/evil"), None);
    }

    #[test]
    fn sanitize_rejects_backslash_traversal_cross_platform() {
        // A Windows-style traversal in a Unix-produced tar must be caught
        // regardless of the extracting platform.
        assert_eq!(sanitize_entry_path("..\\..\\evil"), None);
        assert_eq!(sanitize_entry_path("a\\..\\..\\b"), None);
    }

    #[test]
    fn sanitize_rejects_empty_nul_dot_and_colon() {
        assert_eq!(sanitize_entry_path(""), None);
        assert_eq!(sanitize_entry_path("a/\0/b"), None);
        assert_eq!(sanitize_entry_path("."), None); // no normal component
        assert_eq!(sanitize_entry_path("a:b/c"), None); // colon in a segment
        // ADR-0034 A2 — additional real-world vectors.
        assert_eq!(sanitize_entry_path("foo/../../bar"), None); // mid-path escape
        assert_eq!(sanitize_entry_path("./.."), None); // leading dot then traversal
        assert_eq!(sanitize_entry_path("///"), None); // only separators
        assert_eq!(sanitize_entry_path("\\\\"), None); // only backslashes
        assert_eq!(sanitize_entry_path("C:evil"), None); // bare drive prefix
    }

    // ── is_figure ─────────────────────────────────────────────────────────────

    #[test]
    fn is_figure_matches_allowlist_case_insensitively() {
        for f in ["fig.png", "a/b.EPS", "plot.Pdf", "x.svg", "y.JPEG"] {
            assert!(is_figure(Utf8Path::new(f)), "{f} should be a figure");
        }
        for nf in ["main.tex", "refs.bib", "macros.sty", "README"] {
            assert!(!is_figure(Utf8Path::new(nf)), "{nf} should NOT be a figure");
        }
    }

    // ── extract_bundle ─────────────────────────────────────────────────────────

    #[test]
    fn extract_bundle_all_returns_every_regular_file() {
        let id = make_id("2401.12345");
        let payload = tar_gzip(&[
            ("paper.tex", b"\\documentclass{article}"),
            ("refs.bib", b"@article{x,title={t}}"),
            ("figs/plot.png", b"\x89PNG\r\n"),
        ]);
        let files = extract_bundle(&id, &payload, BundleFilter::All).expect("bundle");
        let mut names: Vec<String> = files.iter().map(|f| f.path.as_str().replace('\\', "/")).collect();
        names.sort();
        assert_eq!(names, vec!["figs/plot.png", "paper.tex", "refs.bib"]);
        // Postcondition: every returned path is relative with no traversal.
        assert!(files
            .iter()
            .all(|f| !f.path.as_str().starts_with('/') && !f.path.as_str().contains("..")));
    }

    #[test]
    fn extract_bundle_figures_only_keeps_images() {
        let id = make_id("2401.12345");
        let payload = tar_gzip(&[
            ("paper.tex", b"\\documentclass{article}"),
            ("refs.bib", b"@article{x}"),
            ("figs/plot.png", b"\x89PNG"),
            ("diagram.eps", b"%!PS"),
        ]);
        let files = extract_bundle(&id, &payload, BundleFilter::FiguresOnly).expect("figs");
        let mut names: Vec<String> = files.iter().map(|f| f.path.as_str().replace('\\', "/")).collect();
        names.sort();
        assert_eq!(names, vec!["diagram.eps", "figs/plot.png"]);
    }

    #[test]
    fn extract_bundle_pdf_only_is_source_unavailable() {
        let id = make_id("2401.12345");
        let err = extract_bundle(&id, b"%PDF-1.5 x", BundleFilter::All).expect_err("pdf-only");
        assert!(matches!(err, FetchError::SourceUnavailable { .. }));
    }

    #[test]
    fn extract_bundle_bare_file_is_source_unavailable() {
        // A bare (non-gzip, non-PDF) single file is not a bundle (ADR-0034 I6):
        // unlike extract_tex (which passes a bare .tex through), the bundle
        // path returns SourceUnavailable.
        let id = make_id("2401.12345");
        let err = extract_bundle(&id, b"\\documentclass{article}\nhi", BundleFilter::All)
            .expect_err("bare file is not a bundle");
        assert!(matches!(err, FetchError::SourceUnavailable { .. }));
    }

    #[test]
    fn extract_bundle_figures_only_none_present_is_source_unavailable() {
        let id = make_id("2401.12345");
        let payload = tar_gzip(&[("paper.tex", b"\\documentclass{article}")]);
        let err =
            extract_bundle(&id, &payload, BundleFilter::FiguresOnly).expect_err("no figures");
        assert!(matches!(err, FetchError::SourceUnavailable { .. }));
    }

    #[test]
    fn extract_bundle_rejects_traversal_entry_but_keeps_safe() {
        // ADR-0034 I1: prove sanitize_entry_path is actually WIRED INTO
        // extract_bundle — a malicious `../evil.tex` entry must be absent from
        // the result while a benign sibling survives. If a refactor dropped
        // the sanitize call, this fails.
        let id = make_id("2401.12345");
        let payload = tar_gzip(&[
            ("../evil.tex", b"evil"),
            ("safe.tex", b"\\documentclass{article}"),
        ]);
        let files = extract_bundle(&id, &payload, BundleFilter::All).expect("bundle");
        let names: Vec<String> = files
            .iter()
            .map(|f| f.path.as_str().replace('\\', "/"))
            .collect();
        assert!(
            names.iter().all(|n| !n.contains("..")),
            "traversal entry must be rejected; got {names:?}"
        );
        assert!(
            names.iter().any(|n| n == "safe.tex"),
            "benign sibling must survive; got {names:?}"
        );
    }
}
