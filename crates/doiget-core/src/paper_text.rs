//! Full-text **extraction** of an arXiv paper from ar5iv's LaTeXML XHTML
//! (the #281 "read" step; ADR-0032).
//!
//! This is the step that lets an agent actually *read* a paper without an
//! external pdf-to-text tool. It is deliberately **distinct from PDF
//! content processing** (permanent non-goal #1 / ADR-0003): the PDF blob
//! is never opened. Instead doiget fetches a *separate, already-structured
//! artifact* — the publisher-rendered HTML — and extracts text from it.
//! ADR-0032 D1 records the boundary: PDF-blob parsing and OCR stay
//! permanently out of scope; structured HTML/XML full text is in scope.
//!
//! ## Source (ADR-0032 D3)
//!
//! PR4 ships one source, **ar5iv** (`ar5iv.labs.arxiv.org/html/<id>`),
//! which renders arXiv papers as LaTeXML XHTML. The fetch goes through the
//! dedicated `"ar5iv"` source key (see [`crate::http::fulltext_allowlist`])
//! so the provenance trail distinguishes ar5iv full text from the arXiv
//! PDF/Atom API. PMC / Europe PMC JATS is a planned follow-up source.
//!
//! ## Capability tier (ADR-0032 D2)
//!
//! Tier 1 OA metadata, **always-on**: no env gate, no Cargo feature gate.
//! Read-only, open-access, never a PDF reinterpretation — same posture
//! class as discovery search (ADR-0031). Ships in the default `oa-only`
//! binary.
//!
//! ## Caching (ADR-0032 D4)
//!
//! Extracted text is cached at `<cache_root>/text/<safekey>.json` (the
//! doiget-private cache root, `docs/CACHE.md`) — **not** the shared
//! `~/papers/` store (`docs/STORE.md`), so no cross-tool coordination is
//! needed. The cache holds the **full** text; `max_chars` truncation is a
//! view applied on return, so one cached entry serves any `max_chars`.
//! Best-effort: a miss / parse error / write failure degrades to a
//! re-fetch, never an error (mirrors [`crate::resolver_cache`]).
//!
//! ## Extraction
//!
//! A `quick-xml` walk (the same parser the arXiv Atom path uses) splits
//! the document into `{ heading, text }` sections on `h1`–`h6`, skips
//! `script` / `style` / `math` subtrees — capturing each `<math>`'s
//! `alttext` (the LaTeX source) as inline `\(…\)` text so formulae read
//! cleanly rather than as MathML noise — and normalizes whitespace.
//! Extraction is best-effort: it supplies the text it can and flags
//! truncation; it does not promise faithful reconstruction.

use camino::{Utf8Path, Utf8PathBuf};
use chrono::{DateTime, Duration, Utc};
use quick_xml::events::Event;
use quick_xml::Reader;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::provenance::{Capability, LogEvent, LogResult, RowInput};
use crate::source::{FetchContext, FetchError};
use crate::{ArxivId, Ref};

/// Source key for the per-source HTTP client + redirect allowlist. Kept
/// distinct from `"arxiv"` (PDF/Atom) so the provenance trail records that
/// extracted text came from the ar5iv renderer (ADR-0032 D3).
const SOURCE_KEY: &str = "ar5iv";

/// Production ar5iv base. Overridable via `DOIGET_AR5IV_BASE` (test
/// wiremock origin), mirroring the `DOIGET_ARXIV_BASE` override.
pub const AR5IV_DEFAULT_BASE: &str = "https://ar5iv.labs.arxiv.org";

/// Cache entry TTL. ar5iv output for a given id is effectively static, so
/// a long TTL is safe; 30 days bounds staleness while keeping repeat reads
/// offline.
const TEXT_CACHE_TTL_DAYS: i64 = 30;

/// On-disk text-cache schema version (`docs/CACHE.md`).
const TEXT_CACHE_SCHEMA_VERSION: &str = "1.0";

/// Which structured full-text source produced a [`PaperText`].
///
/// `#[non_exhaustive]` reserves room for future Tier-1 full-text sources
/// (PMC / Europe PMC JATS) without a breaking change; the wire form is the
/// lowercase variant name.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum TextSource {
    /// ar5iv LaTeXML XHTML (`ar5iv.labs.arxiv.org`). Serializes `"ar5iv"`.
    Ar5iv,
}

/// One `{ heading, text }` section of an extracted paper.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TextSection {
    /// Section heading (`h1`–`h6` text), or `None` for the lead matter
    /// before the first heading.
    pub heading: Option<String>,
    /// Plain section text: tags stripped, whitespace normalized, inline
    /// math rendered as the LaTeX `\(…\)` from the source's `alttext`.
    pub text: String,
}

/// Extracted full text of an arXiv paper.
///
/// The value returned by [`paper_text`] reflects the caller's `max_chars`
/// (see [`PaperText::truncated`]); the cached form is always the full
/// text.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PaperText {
    /// The arXiv id this text belongs to.
    pub arxiv_id: String,
    /// Which source produced the text (PR4: always [`TextSource::Ar5iv`]).
    pub source: TextSource,
    /// Document title, if one was extracted (the `<title>` element).
    pub title: Option<String>,
    /// Ordered sections.
    pub sections: Vec<TextSection>,
    /// Total `char`s across `sections[].text` (after any truncation).
    pub char_count: usize,
    /// `true` when the returned text was truncated to honor `max_chars`.
    pub truncated: bool,
    /// Final URL the text was retrieved from (after redirects), for
    /// provenance. Carried through the cache.
    pub retrieved_from: String,
}

/// Fetch and extract the full text of an arXiv paper from ar5iv.
///
/// `base` is the ar5iv base URL (production [`AR5IV_DEFAULT_BASE`]; tests
/// inject a wiremock origin via `DOIGET_AR5IV_BASE`). `max_chars` caps the
/// returned **section body text** (`char_count`; the `title` and section
/// `heading`s are not counted against it) (`None` = no cap); truncation is
/// flagged on
/// [`PaperText::truncated`], never silent. When `ctx.cache_root` is `Some`,
/// a fresh cache entry is served from disk; otherwise the text is fetched,
/// parsed, cached (best-effort), and one `Fetch` provenance row is emitted.
///
/// Never opens a PDF — this is a separate fetch of the ar5iv HTML artifact
/// (ADR-0032 D1).
///
/// # Errors
///
/// - [`FetchError::Http`] for transport / status failures (a 404 / 410
///   collapses to `NOT_FOUND` at the boundary — the id is genuinely absent
///   from ar5iv).
/// - [`FetchError::TextUnavailable`] when ar5iv returns a **200** with no
///   extractable prose (`char_count == 0`: the paper was never converted to
///   HTML). Distinct from `NotFound`: the id is valid and the PDF may be
///   fetchable, so an agent should fetch rather than treat the ref as wrong
///   (issue #302).
/// - [`FetchError::SourceSchema`] if the ar5iv URL cannot be constructed
///   from `base` + the id. (HTML parsing itself is best-effort and
///   infallible on content — see the `parse_ar5iv` helper.)
/// - [`FetchError::Log`] if the provenance write fails (fail-closed).
pub async fn paper_text(
    base: &Url,
    id: &ArxivId,
    max_chars: Option<usize>,
    ctx: &FetchContext,
) -> Result<PaperText, FetchError> {
    // Cache read (best-effort). A hit serves the full text from disk; the
    // `max_chars` view is applied below, so the same entry serves any cap.
    if let Some(root) = &ctx.cache_root {
        if let Some(full) = cache_read(root, id) {
            return Ok(apply_max_chars(full, max_chars));
        }
    }

    let full = fetch_and_parse(base, id, ctx).await?;

    if let Some(root) = &ctx.cache_root {
        cache_write(root, id, &full);
    }

    Ok(apply_max_chars(full, max_chars))
}

/// Network fetch + parse + provenance for one ar5iv document. Always
/// returns the **full** (untruncated) text.
async fn fetch_and_parse(
    base: &Url,
    id: &ArxivId,
    ctx: &FetchContext,
) -> Result<PaperText, FetchError> {
    // Politeness gate — same channel every source uses.
    let _permit = ctx.rate_limiter.acquire(SOURCE_KEY).await;

    let url = ar5iv_url(base, id)?;
    // `fetch_bytes` (not `fetch_pdf`): the body is HTML, and the PDF
    // magic-byte check must NOT apply here.
    let (body, final_url) = ctx.http.fetch_bytes(SOURCE_KEY, url).await?;

    let (title, sections) = parse_ar5iv(&body)?;

    // A 200 with no readable prose (ar5iv's "not converted" placeholder, or
    // a stub that parses to section shells with empty bodies) is an
    // authoritative "nothing to read". Gate on the total extractable char
    // count — NOT just `sections.is_empty()` — so the empty-bodied-section
    // case (issue #302's repro: a non-empty `sections` whose text is all
    // blank) is caught too and never escapes as an empty `Ok` (a silent
    // exit-0 the caller misreads as a bad identifier).
    //
    // Surfaced as `TextUnavailable`, NOT `NotFound`: the arXiv id was
    // already validated and resolved, so the honest signal is "this
    // representation is missing — fetch the PDF", not "the id does not
    // exist" (which would send an agent down a wrong debugging path).
    let char_count: usize = sections.iter().map(|s| s.text.chars().count()).sum();
    if char_count == 0 {
        return Err(FetchError::TextUnavailable {
            arxiv_id: id.clone(),
        });
    }

    // ADR-0021 §1 canonical digest under the "ar5iv" resolver profile.
    let canonical = Ref::Arxiv(id.clone())
        .promote(SOURCE_KEY, None)
        .digest_hex();
    ctx.log.append(RowInput {
        event: LogEvent::Fetch,
        result: LogResult::Ok,
        // OA full-text content; same capability class the arXiv PDF leg
        // uses. Never a PDF reinterpretation (ADR-0032 D1).
        capability: Capability::Oa,
        ref_: Some(id.as_str()),
        source: Some(SOURCE_KEY),
        error_code: None,
        size_bytes: Some(body.len() as u64),
        license: Some("arxiv-default"),
        store_path: None,
        canonical_digest: Some(&canonical),
    })?;

    Ok(PaperText {
        arxiv_id: id.as_str().to_string(),
        source: TextSource::Ar5iv,
        title,
        sections,
        // Computed above (the non-empty gate); reused here so the count is
        // derived exactly once.
        char_count,
        truncated: false,
        retrieved_from: final_url.to_string(),
    })
}

/// Build the ar5iv URL for an id: `<base>/html/<id>`.
///
/// Old-style ids (`cond-mat/9501001`) contain a `/`; the resulting path
/// `/html/cond-mat/9501001` is the form ar5iv expects. `Url::join` of the
/// absolute reference `/html/<id>` resolves correctly for both id shapes
/// (the base has no path beyond `/`), mirroring the arXiv PDF URL builder.
fn ar5iv_url(base: &Url, id: &ArxivId) -> Result<Url, FetchError> {
    base.join(&format!("/html/{}", id.as_str()))
        .map_err(|e| FetchError::SourceSchema {
            hint: format!("ar5iv URL construction failed: {e}"),
        })
}

/// Apply a `max_chars` cap to a full [`PaperText`], producing the returned
/// view. Truncation is by `char` (boundary-safe) and is flagged; sections
/// past the budget are dropped and the straddling section is cut.
fn apply_max_chars(full: PaperText, max_chars: Option<usize>) -> PaperText {
    let Some(max) = max_chars else {
        return full;
    };

    let mut out: Vec<TextSection> = Vec::new();
    let mut used = 0usize;
    let mut truncated = false;
    for sec in full.sections {
        if used >= max {
            truncated = true;
            break;
        }
        let remaining = max - used;
        let len = sec.text.chars().count();
        if len <= remaining {
            used += len;
            out.push(sec);
        } else {
            let cut: String = sec.text.chars().take(remaining).collect();
            used += remaining;
            out.push(TextSection {
                heading: sec.heading,
                text: cut,
            });
            truncated = true;
            break;
        }
    }

    PaperText {
        arxiv_id: full.arxiv_id,
        source: full.source,
        title: full.title,
        sections: out,
        char_count: used,
        truncated,
        retrieved_from: full.retrieved_from,
    }
}

// ---------------------------------------------------------------------------
// ar5iv XHTML parser
// ---------------------------------------------------------------------------

/// Local element names whose entire subtree is skipped (text discarded).
/// `math` is in this set, but its `alttext` is captured before the subtree
/// is skipped (see [`extract_alttext`]).
fn is_skip_element(local: &str) -> bool {
    matches!(local, "script" | "style" | "math")
}

/// Heading level (1..=6) for an `h1`–`h6` local name, else `None`.
fn heading_level(local: &str) -> Option<u8> {
    match local {
        "h1" => Some(1),
        "h2" => Some(2),
        "h3" => Some(3),
        "h4" => Some(4),
        "h5" => Some(5),
        "h6" => Some(6),
        _ => None,
    }
}

/// Extract a `<math alttext="...">` LaTeX source, if present.
fn extract_alttext(e: &quick_xml::events::BytesStart<'_>) -> Option<String> {
    for attr in e.attributes().flatten() {
        if attr.key.as_ref() == "alttext" {
            if let Ok(v) = attr.normalized_value(quick_xml::XmlVersion::Explicit1_0) {
                let s = v.into_owned();
                if !s.trim().is_empty() {
                    return Some(s);
                }
            }
        }
    }
    None
}

/// Normalize collected text: collapse all whitespace runs to single spaces
/// and trim. Keeps inline LaTeX (`\(…\)`) intact bar internal whitespace
/// collapse.
fn normalize(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Parse an ar5iv LaTeXML-XHTML body into `(title, sections)`.
///
/// Best-effort: see the module docs. Splits on `h1`–`h6`; skips
/// `script` / `style` / `math` subtrees (capturing `math` `alttext` as
/// inline `\(…\)`); normalizes whitespace. Sections with empty text but a
/// heading are kept (a heading with no body still maps the document).
///
/// Best-effort and **infallible** on content: a malformed/non-well-formed
/// document is recovered (the reader runs with `check_end_names = false`,
/// and a hard syntax error stops the walk while keeping the partial
/// result rather than discarding it — ADR-0032 D3). An empty result is
/// handled by the caller (→ `TextUnavailable`). Returns `Result` only to keep the
/// call-site uniform; it does not currently produce an `Err`.
fn parse_ar5iv(html: &[u8]) -> Result<(Option<String>, Vec<TextSection>), FetchError> {
    let mut reader = Reader::from_reader(html);
    let config = reader.config_mut();
    config.trim_text(true);
    // Best-effort (ADR-0032 D3): ar5iv LaTeXML output is normally
    // well-formed XHTML, but real renderings can carry mismatched/void
    // tags. Don't reject the whole document over an end-name mismatch —
    // recover and keep extracting.
    config.check_end_names = false;

    let mut sections: Vec<TextSection> = Vec::new();
    let mut cur_heading: Option<String> = None;
    let mut cur_text = String::new();

    let mut title: Option<String> = None;
    let mut title_buf = String::new();
    let mut in_title = false;

    // Depth-counted skip: >0 means we are inside a script/style/math
    // subtree and must discard text. Increment on the skip element's
    // Start, decrement on its End. (`math` captures alttext first.)
    let mut skip: u32 = 0;
    // >0 means we are inside an h1..h6 element; text routes to heading_buf.
    let mut in_heading: u8 = 0;
    let mut heading_buf = String::new();

    let mut buf: Vec<u8> = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = e.name();
                let local = local_name(name.as_ref());
                if is_skip_element(local) {
                    if skip == 0 && local == "math" {
                        if let Some(alt) = extract_alttext(&e) {
                            let frag = format!("\\({alt}\\) ");
                            push_target(
                                in_title,
                                in_heading,
                                &mut title_buf,
                                &mut heading_buf,
                                &mut cur_text,
                                &frag,
                            );
                        }
                    }
                    skip += 1;
                } else if let Some(level) = heading_level(local) {
                    // A new heading closes the current section.
                    flush_section(&mut sections, &mut cur_heading, &mut cur_text);
                    in_heading = level;
                    heading_buf.clear();
                } else if local == "title" && title.is_none() {
                    in_title = true;
                    title_buf.clear();
                }
                buf.clear();
            }
            Ok(Event::Empty(e)) => {
                let name = e.name();
                let local = local_name(name.as_ref());
                // A self-closing `<math .../>` contributes its alttext but
                // has no subtree to skip.
                if skip == 0 && local == "math" {
                    if let Some(alt) = extract_alttext(&e) {
                        let frag = format!("\\({alt}\\) ");
                        push_target(
                            in_title,
                            in_heading,
                            &mut title_buf,
                            &mut heading_buf,
                            &mut cur_text,
                            &frag,
                        );
                    }
                }
                buf.clear();
            }
            Ok(Event::Text(t)) => {
                match quick_xml::escape::unescape(&t).ok().map(|c| c.into_owned()) {
                    Some(s) => {
                        if !s.is_empty() && skip == 0 {
                            let mut frag = s;
                            frag.push(' ');
                            push_target(
                                in_title,
                                in_heading,
                                &mut title_buf,
                                &mut heading_buf,
                                &mut cur_text,
                                &frag,
                            );
                        }
                    }
                    // A text fragment that fails to decode/unescape is
                    // dropped (best-effort), but log it: a *systematic*
                    // decode failure would otherwise vanish silently while
                    // the extraction still "succeeds" (review #285).
                    None => {
                        tracing::debug!(
                            "ar5iv: skipped a text fragment that failed to decode/unescape"
                        )
                    }
                }
                buf.clear();
            }
            Ok(Event::End(e)) => {
                let name = e.name();
                let local = local_name(name.as_ref());
                if is_skip_element(local) {
                    skip = skip.saturating_sub(1);
                } else if heading_level(local).is_some() && in_heading > 0 {
                    cur_heading = {
                        let h = normalize(&heading_buf);
                        if h.is_empty() {
                            None
                        } else {
                            Some(h)
                        }
                    };
                    in_heading = 0;
                    // The body of the new section starts fresh.
                    cur_text.clear();
                } else if local == "title" && in_title {
                    in_title = false;
                    let t = normalize(&title_buf);
                    if !t.is_empty() {
                        title = Some(t);
                    }
                }
                buf.clear();
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                // Best-effort (ADR-0032 D3): a syntax error deep in the
                // document must not discard everything already collected.
                // Stop here and return the partial result — an empty result
                // still maps to TextUnavailable upstream. The error is observable
                // on stderr, never on the stdout JSON-RPC channel.
                tracing::debug!(error = %e, "ar5iv HTML parse error; returning best-effort partial text");
                break;
            }
            _ => {
                buf.clear();
            }
        }
    }

    // Final section (text after the last heading, or the lead matter when
    // the document had no headings at all).
    flush_section(&mut sections, &mut cur_heading, &mut cur_text);

    Ok((title, sections))
}

/// Append `frag` to whichever buffer is currently active: the `<title>`
/// buffer, the heading buffer, or the section-body buffer.
fn push_target(
    in_title: bool,
    in_heading: u8,
    title_buf: &mut String,
    heading_buf: &mut String,
    cur_text: &mut String,
    frag: &str,
) {
    if in_title {
        title_buf.push_str(frag);
    } else if in_heading > 0 {
        heading_buf.push_str(frag);
    } else {
        cur_text.push_str(frag);
    }
}

/// Push the current `(heading, text)` as a section if it carries anything
/// (non-empty body OR a heading), then reset the body buffer. The heading
/// is left in place — it is replaced when the next heading is parsed.
fn flush_section(
    sections: &mut Vec<TextSection>,
    cur_heading: &mut Option<String>,
    cur_text: &mut String,
) {
    let text = normalize(cur_text);
    if !text.is_empty() || cur_heading.is_some() {
        sections.push(TextSection {
            heading: cur_heading.clone(),
            text,
        });
    }
    cur_text.clear();
}

/// Strip an XML namespace prefix, returning the local part
/// (`"xhtml:p"` -> `"p"`). Mirrors the arXiv Atom parser's helper.
fn local_name(qname: &str) -> &str {
    match qname.rfind(':') {
        Some(idx) => &qname[idx + 1..],
        None => qname,
    }
}

// ---------------------------------------------------------------------------
// Text cache (docs/CACHE.md; ADR-0032 D4)
// ---------------------------------------------------------------------------

/// On-disk cache entry. `paper_text` is stored as nested JSON (not a
/// string) since the whole entry is JSON.
#[derive(Debug, Serialize, Deserialize)]
struct TextCacheEntry {
    schema_version: String,
    /// RFC 3339 UTC timestamp of the fetch that produced this entry.
    fetched_at: String,
    ttl_seconds: i64,
    paper_text: PaperText,
}

/// The on-disk path for an id's text cache entry:
/// `<cache_root>/text/<safekey>.json`.
fn cache_file(cache_root: &Utf8Path, id: &ArxivId) -> Utf8PathBuf {
    let safekey = Ref::Arxiv(id.clone()).safekey();
    cache_root
        .join("text")
        .join(format!("{}.json", safekey.as_str()))
}

/// Read the cached full text for `id` if present and within its TTL. Any
/// miss condition (absent / unparsable / expired) returns `None`.
fn cache_read(cache_root: &Utf8Path, id: &ArxivId) -> Option<PaperText> {
    cache_read_at(cache_root, id, Utc::now())
}

/// [`cache_read`] with an injected clock for tests.
fn cache_read_at(cache_root: &Utf8Path, id: &ArxivId, now: DateTime<Utc>) -> Option<PaperText> {
    let path = cache_file(cache_root, id);
    let text = std::fs::read_to_string(&path).ok()?;
    let entry: TextCacheEntry = serde_json::from_str(&text).ok()?;
    let fetched: DateTime<Utc> = DateTime::parse_from_rfc3339(&entry.fetched_at)
        .ok()?
        .with_timezone(&Utc);
    if now > fetched + Duration::seconds(entry.ttl_seconds) {
        return None;
    }
    Some(entry.paper_text)
}

/// Write the full text for `id` to the cache. Best-effort: returns `false`
/// (after a `tracing::debug!`) on any failure rather than propagating — a
/// cache write must never fail an extraction.
fn cache_write(cache_root: &Utf8Path, id: &ArxivId, full: &PaperText) -> bool {
    cache_write_at(cache_root, id, full, Utc::now())
}

/// [`cache_write`] with an injected clock for tests.
fn cache_write_at(
    cache_root: &Utf8Path,
    id: &ArxivId,
    full: &PaperText,
    now: DateTime<Utc>,
) -> bool {
    let entry = TextCacheEntry {
        schema_version: TEXT_CACHE_SCHEMA_VERSION.to_string(),
        fetched_at: now.to_rfc3339(),
        ttl_seconds: TEXT_CACHE_TTL_DAYS * 86_400,
        paper_text: full.clone(),
    };
    let json = match serde_json::to_string(&entry) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(error = %e, "text cache: serialize failed; skipping write");
            return false;
        }
    };
    let path = cache_file(cache_root, id);
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::debug!(error = %e, dir = %parent, "text cache: mkdir failed; skipping write");
            return false;
        }
    }
    if let Err(e) = std::fs::write(&path, json) {
        tracing::debug!(error = %e, path = %path, "text cache: write failed");
        return false;
    }
    true
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    use std::sync::Arc;

    use camino::Utf8PathBuf;
    use tempfile::TempDir;
    use wiremock::matchers::{method, path as path_matcher};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::http::HttpClient;
    use crate::provenance::{LogRow, ProvenanceLog};
    use crate::rate_limiter::RateLimiter;
    use crate::RateLimits;

    /// Synthetic ar5iv-shaped XHTML. Hand-crafted (not a snapshot) to avoid
    /// third-party redistribution; exercises title, lead matter, two
    /// headed sections, inline math `alttext`, and skipped script/style.
    const SAMPLE_AR5IV: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE html>
<html xmlns="http://www.w3.org/1999/xhtml">
<head>
  <title>Tropical Tensor Networks</title>
  <style>.ltx_page { color: black; }</style>
</head>
<body>
  <div class="ltx_page_content">
    <p>We study tropical tensor networks for spin glasses.</p>
    <section class="ltx_section">
      <h2 class="ltx_title">1 Introduction</h2>
      <p>The free energy is <math alttext="F = -kT \log Z"><mrow><mi>F</mi></mrow></math> in the limit.</p>
      <script>trackingPixel();</script>
    </section>
    <section class="ltx_section">
      <h2 class="ltx_title">2 Methods</h2>
      <p>We use a contraction scheme.</p>
    </section>
  </div>
</body>
</html>"#;

    fn build_test_context(
        wiremock_host: &str,
        cache: Option<Utf8PathBuf>,
    ) -> (TempDir, FetchContext) {
        let td = TempDir::new().expect("tempdir");
        let log_dir =
            Utf8PathBuf::try_from(td.path().to_path_buf()).expect("temp dir path must be UTF-8");
        let log_path = log_dir.join("test.jsonl");

        let http = Arc::new(HttpClient::new_for_tests_allow_http("ar5iv", wiremock_host));
        let rate_limiter = Arc::new(RateLimiter::new(RateLimits::HARD_CODED));
        let session_id = "01J0000000000000000000TEST".to_string();
        let log = Arc::new(
            ProvenanceLog::open(log_path, session_id.clone()).expect("provenance log opens"),
        );
        let ctx = FetchContext {
            http,
            rate_limiter,
            log,
            session_id,
            cache_root: cache,
        };
        (td, ctx)
    }

    fn read_rows(path: &camino::Utf8Path) -> Vec<LogRow> {
        let raw = std::fs::read_to_string(path).expect("read log");
        raw.lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str::<LogRow>(l).expect("valid LogRow"))
            .collect()
    }

    // ---- parse_ar5iv -----------------------------------------------------

    #[test]
    fn parse_extracts_title_sections_and_inline_math() {
        let (title, sections) = parse_ar5iv(SAMPLE_AR5IV.as_bytes()).expect("parses");
        assert_eq!(title.as_deref(), Some("Tropical Tensor Networks"));
        assert_eq!(
            sections.len(),
            3,
            "lead + two headed sections: {sections:?}"
        );

        // Lead matter (before the first heading) has no heading.
        assert_eq!(sections[0].heading, None);
        assert_eq!(
            sections[0].text,
            "We study tropical tensor networks for spin glasses."
        );

        assert_eq!(sections[1].heading.as_deref(), Some("1 Introduction"));
        // Inline math becomes the LaTeX `alttext` as `\(…\)`; the MathML
        // (`<mi>F</mi>`) subtree is skipped.
        assert_eq!(
            sections[1].text,
            "The free energy is \\(F = -kT \\log Z\\) in the limit."
        );
        assert!(
            !sections[1].text.contains("trackingPixel"),
            "script content must be skipped: {}",
            sections[1].text
        );

        assert_eq!(sections[2].heading.as_deref(), Some("2 Methods"));
        assert_eq!(sections[2].text, "We use a contraction scheme.");
    }

    #[test]
    fn parse_no_headings_yields_single_lead_section() {
        let xml = r#"<html><body><p>One paragraph only.</p><p>Second one.</p></body></html>"#;
        let (title, sections) = parse_ar5iv(xml.as_bytes()).expect("parses");
        assert!(title.is_none());
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].heading, None);
        assert_eq!(sections[0].text, "One paragraph only. Second one.");
    }

    #[test]
    fn parse_empty_body_yields_nothing() {
        let xml = r#"<html><head></head><body></body></html>"#;
        let (title, sections) = parse_ar5iv(xml.as_bytes()).expect("parses");
        assert!(title.is_none());
        assert!(sections.is_empty());
    }

    #[test]
    fn parse_mismatched_tags_recovers_full_document() {
        // Mismatched/unclosed tags (a `<b>` closed by `</p>`, an unclosed
        // `<body>`) must NOT discard the document: `check_end_names = false`
        // recovers and keeps extracting past them (ADR-0032 D3).
        let xml = r#"<html><body><p>Alpha beta <b>bold</p><h2>Sec</h2><p>Body text</body>"#;
        let res = parse_ar5iv(xml.as_bytes());
        assert!(res.is_ok(), "best-effort parse must not error: {res:?}");
        let (_title, sections) = res.expect("ok");
        let joined: String = sections
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            joined.contains("Body text") && joined.contains("bold"),
            "recovered text past the mismatched tags: {joined:?}"
        );
    }

    #[test]
    fn parse_hard_syntax_error_degrades_to_partial_not_error() {
        // A bare `&` (invalid entity) is a hard reader error; best-effort
        // (ADR-0032 D3) returns the partial text collected before it rather
        // than erroring — a single defect never collapses the whole call to
        // INTERNAL_ERROR.
        let xml = r#"<html><body><p>Prefix kept here</p><p>Bad & entity halts</body>"#;
        let res = parse_ar5iv(xml.as_bytes());
        assert!(res.is_ok(), "hard syntax error must NOT error: {res:?}");
        let (_title, sections) = res.expect("ok");
        let joined: String = sections
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            joined.contains("Prefix kept here"),
            "the prefix before the hard error is retained: {joined:?}"
        );
    }

    // ---- apply_max_chars -------------------------------------------------

    fn full_fixture() -> PaperText {
        PaperText {
            arxiv_id: "2401.12345".into(),
            source: TextSource::Ar5iv,
            title: Some("T".into()),
            sections: vec![
                TextSection {
                    heading: None,
                    text: "abcde".into(),
                }, // 5
                TextSection {
                    heading: Some("H".into()),
                    text: "fghij".into(),
                }, // 5
            ],
            char_count: 10,
            truncated: false,
            retrieved_from: "https://ar5iv.labs.arxiv.org/html/2401.12345".into(),
        }
    }

    #[test]
    fn max_chars_none_returns_full() {
        let out = apply_max_chars(full_fixture(), None);
        assert!(!out.truncated);
        assert_eq!(out.char_count, 10);
        assert_eq!(out.sections.len(), 2);
    }

    #[test]
    fn max_chars_above_total_is_untruncated() {
        let out = apply_max_chars(full_fixture(), Some(100));
        assert!(!out.truncated);
        assert_eq!(out.char_count, 10);
    }

    #[test]
    fn max_chars_cuts_within_a_section() {
        // 7 chars: first section (5) whole, second cut to 2 ("fg").
        let out = apply_max_chars(full_fixture(), Some(7));
        assert!(out.truncated);
        assert_eq!(out.char_count, 7);
        assert_eq!(out.sections.len(), 2);
        assert_eq!(out.sections[1].text, "fg");
        assert_eq!(out.sections[1].heading.as_deref(), Some("H"));
    }

    #[test]
    fn max_chars_drops_trailing_sections_on_exact_boundary() {
        // 5 chars: first section exactly fits; the second is dropped.
        let out = apply_max_chars(full_fixture(), Some(5));
        assert!(out.truncated);
        assert_eq!(out.char_count, 5);
        assert_eq!(out.sections.len(), 1);
    }

    #[test]
    fn max_chars_zero_yields_no_text_but_flags_truncated() {
        let out = apply_max_chars(full_fixture(), Some(0));
        assert!(out.truncated);
        assert_eq!(out.char_count, 0);
        assert!(out.sections.is_empty());
    }

    #[test]
    fn max_chars_truncation_is_char_boundary_safe_for_multibyte() {
        // Truncation must cut on `char` boundaries — a naive byte slice of a
        // multibyte string (CJK / emoji) would panic. 7 chars, multi-byte.
        let full = PaperText {
            arxiv_id: "2401.12345".into(),
            source: TextSource::Ar5iv,
            title: None,
            sections: vec![TextSection {
                heading: None,
                text: "あいうえお漢字".into(),
            }],
            char_count: 7,
            truncated: false,
            retrieved_from: "u".into(),
        };
        let out = apply_max_chars(full, Some(3));
        assert!(out.truncated);
        assert_eq!(out.char_count, 3);
        assert_eq!(out.sections[0].text, "あいう");
    }

    // ---- url builder -----------------------------------------------------

    #[test]
    fn ar5iv_url_new_and_old_style() {
        let base = Url::parse(AR5IV_DEFAULT_BASE).expect("base");
        let new = ar5iv_url(&base, &ArxivId::parse("2401.12345").unwrap()).expect("url");
        assert_eq!(new.path(), "/html/2401.12345");
        assert_eq!(new.host_str(), Some("ar5iv.labs.arxiv.org"));
        let old = ar5iv_url(&base, &ArxivId::parse("cond-mat/9501001").unwrap()).expect("url");
        assert_eq!(old.path(), "/html/cond-mat/9501001");
    }

    // ---- cache round-trip ------------------------------------------------

    #[test]
    fn cache_write_then_read_round_trips() {
        let dir = TempDir::new().unwrap();
        let root = Utf8Path::from_path(dir.path()).unwrap();
        let id = ArxivId::parse("2401.12345").unwrap();
        let now = Utc::now();
        assert!(cache_write_at(root, &id, &full_fixture(), now));
        let got = cache_read_at(root, &id, now).expect("cache hit");
        assert_eq!(got.arxiv_id, "2401.12345");
        assert_eq!(got.sections.len(), 2);
        assert!(!got.truncated, "cache stores the full, untruncated text");
    }

    #[test]
    fn cache_miss_when_expired() {
        let dir = TempDir::new().unwrap();
        let root = Utf8Path::from_path(dir.path()).unwrap();
        let id = ArxivId::parse("2401.12345").unwrap();
        let written = Utc::now();
        assert!(cache_write_at(root, &id, &full_fixture(), written));
        let later = written + Duration::days(TEXT_CACHE_TTL_DAYS + 1);
        assert!(cache_read_at(root, &id, later).is_none());
    }

    #[test]
    fn cache_file_path_uses_text_dir_and_safekey() {
        let root = Utf8Path::new("/tmp/cache");
        let id = ArxivId::parse("2401.12345").unwrap();
        let p = cache_file(root, &id);
        assert!(p.components().any(|c| c.as_str() == "text"));
        assert!(p.as_str().ends_with(".json"));
    }

    // ---- paper_text end-to-end (wiremock) --------------------------------

    #[tokio::test]
    async fn paper_text_fetches_parses_and_logs() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/html/2401.12345"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_AR5IV))
            .mount(&server)
            .await;

        let host = server
            .uri()
            .parse::<Url>()
            .unwrap()
            .host_str()
            .unwrap()
            .to_string();
        let (_td, ctx) = build_test_context(&host, None);
        let log_path = ctx.log.path().to_path_buf();
        let base = Url::parse(&server.uri()).expect("wiremock URI parses");
        let id = ArxivId::parse("2401.12345").unwrap();

        let out = paper_text(&base, &id, None, &ctx).await.expect("ok");
        assert_eq!(out.arxiv_id, "2401.12345");
        assert_eq!(out.source, TextSource::Ar5iv);
        assert_eq!(out.title.as_deref(), Some("Tropical Tensor Networks"));
        assert_eq!(out.sections.len(), 3);
        assert!(!out.truncated);

        // Exactly one Fetch provenance row, attributed to the ar5iv source.
        let rows = read_rows(&log_path);
        assert_eq!(rows.len(), 1, "one fetch row expected");
        assert_eq!(rows[0].source.as_deref(), Some("ar5iv"));
        assert_eq!(rows[0].ref_.as_deref(), Some("2401.12345"));
        assert!(rows[0].error_code.is_none());
    }

    #[tokio::test]
    async fn paper_text_truncates_when_max_chars_set() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/html/2401.12345"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_AR5IV))
            .mount(&server)
            .await;
        let host = server
            .uri()
            .parse::<Url>()
            .unwrap()
            .host_str()
            .unwrap()
            .to_string();
        let (_td, ctx) = build_test_context(&host, None);
        let base = Url::parse(&server.uri()).expect("uri");
        let id = ArxivId::parse("2401.12345").unwrap();

        let out = paper_text(&base, &id, Some(10), &ctx).await.expect("ok");
        assert!(out.truncated);
        assert_eq!(out.char_count, 10);
    }

    #[tokio::test]
    async fn paper_text_second_call_is_served_from_cache() {
        // First call hits the (single-response) mock and populates the
        // cache; the mock is mounted `up_to_n_times(1)`, so a second
        // network call would fail — proving the second call is cached.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/html/2401.12345"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_AR5IV))
            .up_to_n_times(1)
            .mount(&server)
            .await;

        let host = server
            .uri()
            .parse::<Url>()
            .unwrap()
            .host_str()
            .unwrap()
            .to_string();
        let cache_dir = TempDir::new().unwrap();
        let cache_root =
            Utf8PathBuf::try_from(cache_dir.path().to_path_buf()).expect("utf8 cache root");
        let (_td, ctx) = build_test_context(&host, Some(cache_root));
        let base = Url::parse(&server.uri()).expect("uri");
        let id = ArxivId::parse("2401.12345").unwrap();

        let first = paper_text(&base, &id, None, &ctx).await.expect("first ok");
        assert_eq!(first.sections.len(), 3);
        // Second call: no network response available; must be a cache hit.
        let second = paper_text(&base, &id, None, &ctx)
            .await
            .expect("second call served from cache");
        assert_eq!(second.sections.len(), 3);
        assert_eq!(second.title, first.title);
    }

    #[tokio::test]
    async fn paper_text_empty_body_is_text_unavailable() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/html/2401.99999"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("<html><head></head><body></body></html>"),
            )
            .mount(&server)
            .await;
        let host = server
            .uri()
            .parse::<Url>()
            .unwrap()
            .host_str()
            .unwrap()
            .to_string();
        let (_td, ctx) = build_test_context(&host, None);
        let base = Url::parse(&server.uri()).expect("uri");
        let id = ArxivId::parse("2401.99999").unwrap();

        // A 200 that parses to nothing is "this representation is missing",
        // NOT "the id does not exist" (issue #302): the arXiv id was already
        // validated, so it must surface as TextUnavailable / TEXT_UNAVAILABLE
        // so an agent fetches the PDF rather than treating the ref as wrong.
        let err = paper_text(&base, &id, None, &ctx)
            .await
            .expect_err("empty body must be TextUnavailable");
        assert!(
            matches!(err, FetchError::TextUnavailable { .. }),
            "got {err:?}"
        );
        assert_eq!(
            crate::ErrorCode::from(&err),
            crate::ErrorCode::TextUnavailable
        );
    }

    #[tokio::test]
    async fn paper_text_prose_free_body_is_text_unavailable() {
        // Issue #302's actual repro: a 200 whose body parses to a NON-empty
        // `sections` vec (a heading shell) but zero extractable prose
        // (`char_count == 0`). The old gate keyed on `sections.is_empty()`,
        // so this slipped through as an empty `Ok` → a silent exit-0 the
        // caller misread as a bad identifier. The `char_count == 0` gate
        // catches it.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/html/2012.03644"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(
                    "<html><head></head><body><h2>1 Introduction</h2></body></html>",
                ),
            )
            .mount(&server)
            .await;
        let host = server
            .uri()
            .parse::<Url>()
            .unwrap()
            .host_str()
            .unwrap()
            .to_string();
        let (_td, ctx) = build_test_context(&host, None);
        let base = Url::parse(&server.uri()).expect("uri");
        let id = ArxivId::parse("2012.03644").unwrap();

        let err = paper_text(&base, &id, None, &ctx)
            .await
            .expect_err("a body with headings but no prose must be TextUnavailable");
        assert!(
            matches!(err, FetchError::TextUnavailable { .. }),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn paper_text_404_surfaces_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/html/2401.00000"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let host = server
            .uri()
            .parse::<Url>()
            .unwrap()
            .host_str()
            .unwrap()
            .to_string();
        let (_td, ctx) = build_test_context(&host, None);
        let base = Url::parse(&server.uri()).expect("uri");
        let id = ArxivId::parse("2401.00000").unwrap();

        let err = paper_text(&base, &id, None, &ctx)
            .await
            .expect_err("404 must surface");
        // 404 collapses to NOT_FOUND at the boundary.
        assert_eq!(crate::ErrorCode::from(&err), crate::ErrorCode::NotFound);
    }

    #[test]
    fn text_source_serializes_lowercase() {
        let s = serde_json::to_string(&TextSource::Ar5iv).expect("serialize");
        assert_eq!(s, "\"ar5iv\"");
    }
}
