//! arXiv source — arXiv id → PDF + Atom-feed metadata.
//!
//! Spec: `docs/SOURCES.md` §4 arXiv. No auth. The API's Terms of Use cap
//! requests at **one every three seconds, single connection** — stricter
//! than the global 5/sec + 200 ms backoff, which this comment previously
//! claimed "comfortably respects" it. It did not: that was 15x the rate and
//! 5x the concurrency (#493, ADR-0045).
//!
//! The limit now comes from [`crate::SOURCE_RATE_OVERRIDES`], and because
//! it caps REQUESTS rather than attempts, the PDF leg below calls
//! [`crate::rate_limiter::RateLimiter::pace`] — one attempt issues two
//! requests, and only the first was paced by the permit.
//!
//! # Fetch flow (full)
//!
//! 1. `can_serve` returns `true` only for `Ref::Arxiv(_)`; `Ref::Doi(_)` is
//!    rejected up front.
//! 2. `fetch` acquires a permit from the shared `RateLimiter`, then
//!    best-effort fetches the Atom feed (`<base>/api/query?id_list=<id>`)
//!    and parses it into a JSON metadata object via the private
//!    `parse_atom_feed` helper. Atom failures degrade gracefully
//!    (`metadata_json = None` + `tracing::warn!`) — the existing 1.0
//!    PDF-leg semantics are preserved.
//! 3. The PDF URL `<base>/pdf/<id>.pdf` is fetched via
//!    [`crate::http::HttpClient::fetch_pdf`] which enforces the magic-byte
//!    (`%PDF-`) check per `docs/SECURITY.md` §1.2.
//! 4. ONE `LogEvent::Fetch` row is appended for the PDF leg. The Atom leg
//!    does NOT emit its own row — the source-level audit unit is
//!    "one fetch attempt = one row" and the Atom call is a supporting
//!    leg of the same attempt.
//!
//! # Metadata-only path
//!
//! [`ArxivSource::fetch_metadata_only`] performs ONLY the Atom feed fetch
//! and is the entry point for the `metadata_only` orchestrator
//! (`crate::orchestrator::metadata_only`). It MUST NOT call
//! [`crate::http::HttpClient::fetch_pdf`] — doing so would violate the
//! `doiget_metadata_only` contract (`docs/MCP_TOOLS.md` §11). It emits
//! one `LogEvent::Fetch` row under `Capability::Metadata` so the audit
//! trail distinguishes metadata-only fetches from full fetches without
//! breaking the schema (the `capability` field is the structured channel
//! for this distinction; spec §3 documents it as one of `oa` / `metadata`
//! / `tdm-*`).

use async_trait::async_trait;
use bytes::Bytes;
use quick_xml::events::Event;
use quick_xml::Reader;
use serde_json::{json, Value};
use url::Url;

use crate::provenance::{Capability, LogEvent, LogResult, RowInput};
use crate::source::{FetchContext, FetchError, FetchResult, Source};
use crate::{ArxivId, CapabilityProfile, Ref};

/// Default base for the PDF endpoint. arXiv serves PDFs at
/// `https://arxiv.org/pdf/<id>` (the trailing `.pdf` is optional but
/// most reliable to include). PDFs may redirect to `cdn.arxiv.org` —
/// the per-source allowlist in `crate::http::tier_1_allowlist()` covers
/// this via the `*.arxiv.org` glob.
const PDF_BASE: &str = "https://arxiv.org";

/// Default base for the Atom metadata endpoint. arXiv serves the API at
/// `https://export.arxiv.org/api/query` — a DIFFERENT host from the PDF
/// endpoint. Hitting `arxiv.org/api/query` instead redirects and fails
/// the metadata leg, so the two endpoints must use separate bases.
/// `export.arxiv.org` is covered by the `*.arxiv.org` allowlist glob.
const META_BASE: &str = "https://export.arxiv.org";

/// arXiv [`Source`] impl. PDFs are served from `arxiv.org`; Atom metadata
/// from `export.arxiv.org` (the `metadata_url` builder).
#[derive(Clone, Debug)]
pub struct ArxivSource {
    /// PDF endpoint base (`arxiv.org` in production).
    base: Url,
    /// Atom metadata endpoint base (`export.arxiv.org` in production).
    meta_base: Url,
}

impl ArxivSource {
    /// Production constructor. PDFs from `arxiv.org`, Atom metadata from
    /// `export.arxiv.org`.
    pub fn new() -> Self {
        // Both hard-coded constants are `'static` string literals known at
        // compile time to be valid absolute URLs; the `expect`s can only
        // fire if a constant regresses, which every `ArxivSource::new()`
        // test exercises.
        #[allow(clippy::expect_used)]
        let base = Url::parse(PDF_BASE).expect("hard-coded PDF base URL is valid");
        #[allow(clippy::expect_used)]
        let meta_base = Url::parse(META_BASE).expect("hard-coded meta base URL is valid");
        Self { base, meta_base }
    }

    /// Construct with an arbitrary base URL.
    ///
    /// The orchestrator (`doiget-cli::commands::fetch`) uses this to honor
    /// the `DOIGET_ARXIV_BASE` env var, which lets integration tests point
    /// the source at a wiremock origin without resorting to compile-time
    /// gates. Both the PDF and metadata legs share the one override base
    /// (a single wiremock origin serves both paths). Production callers
    /// use [`ArxivSource::new`].
    pub fn with_base(base: Url) -> Self {
        Self {
            meta_base: base.clone(),
            base,
        }
    }

    /// Build the PDF URL for a given arXiv id. arXiv accepts both
    /// `/pdf/<id>` and `/pdf/<id>.pdf`; we use the trailing-`.pdf` form to
    /// make the URL self-describing.
    ///
    /// Old-style ids (`cond-mat/9501001`) contain a `/` in the id itself;
    /// the resulting path `/pdf/cond-mat/9501001.pdf` is the form arXiv
    /// expects. Because the base URL has no path beyond `/`, `Url::join`
    /// resolves the absolute reference `/pdf/<id>.pdf` to exactly that
    /// path for both new-style (`2401.12345`) and old-style
    /// (`cond-mat/9501001`) ids. The `arxiv_fetch_old_style_id_*` test
    /// pins this behavior.
    fn pdf_url(&self, id: &ArxivId) -> Result<Url, FetchError> {
        let path = format!("/pdf/{}.pdf", id.as_str());
        self.base.join(&path).map_err(|e| FetchError::SourceSchema {
            hint: format!("arxiv URL construction failed: {e}"),
        })
    }

    /// Build the Atom-feed metadata URL for a given arXiv id.
    ///
    /// Production: `https://export.arxiv.org/api/query?id_list=<id>`. In
    /// tests the base is the wiremock origin; the path is the same
    /// (`/api/query?id_list=<id>`). The `export.arxiv.org` host is on the
    /// `arxiv` redirect allowlist (per
    /// `crate::http::tier_1_allowlist`) so the redirect closure does not
    /// reject this leg.
    ///
    /// Old-style ids (`cond-mat/9501001`) contain a `/` which we
    /// URL-encode via `query_pairs_mut().append_pair` so the wire form is
    /// `id_list=cond-mat%2F9501001`.
    fn metadata_url(&self, id: &ArxivId) -> Result<Url, FetchError> {
        let mut url = self
            .meta_base
            .join("/api/query")
            .map_err(|e| FetchError::SourceSchema {
                hint: format!("arxiv metadata URL construction failed: {e}"),
            })?;
        url.query_pairs_mut().append_pair("id_list", id.as_str());
        Ok(url)
    }

    /// Fetch ONLY the Atom-feed metadata for the given arXiv id. Does NOT
    /// touch the PDF endpoint — this is the entry point for the
    /// `metadata_only` orchestrator (`docs/MCP_TOOLS.md` §11).
    ///
    /// Emits a single `LogEvent::Fetch` row under `Capability::Metadata`
    /// so the audit trail distinguishes metadata-only attempts from full
    /// (PDF) fetches.
    ///
    /// # Errors
    ///
    /// - [`FetchError::Http`] on transport / status / size-cap failures.
    /// - [`FetchError::SourceSchema`] if the response body is not
    ///   well-formed Atom XML.
    /// - [`FetchError::Log`] if the provenance row write fails
    ///   (fail-closed per `docs/PROVENANCE_LOG.md` §5).
    pub async fn fetch_metadata_only(
        &self,
        id: &ArxivId,
        ctx: &FetchContext,
    ) -> Result<Value, FetchError> {
        // Same politeness gate as the full fetch path.
        let _permit = ctx.rate_limiter.acquire(self.name()).await;

        let url = self.metadata_url(id)?;
        let (body, _final_url) = ctx.http.fetch_bytes(self.name(), url).await?;
        let metadata = parse_atom_feed(&body)?;

        // ADR-0021 §1 canonical-digest under the "arxiv" resolver
        // profile. version=None until a follow-up slice threads the
        // Atom-feed-discovered version (`v2`, etc.) into this row.
        let canonical =
            crate::CanonicalRef::new(crate::SourceType::Arxiv, id.as_str(), self.name(), None)
                .digest_hex();
        ctx.log.append(RowInput {
            event: LogEvent::Fetch,
            result: LogResult::Ok,
            // Distinguish metadata-only from full (PDF) fetches via the
            // structured `capability` channel rather than mangling the
            // `source` string — `docs/PROVENANCE_LOG.md` §3 lists
            // `metadata` as a first-class capability value.
            capability: Capability::Metadata,
            ref_: Some(id.as_str()),
            source: Some(self.name()),
            error_code: None,
            size_bytes: Some(body.len() as u64),
            license: Some("arxiv-default"),
            store_path: None,
            canonical_digest: Some(&canonical),
        })?;

        Ok(metadata)
    }
}

impl Default for ArxivSource {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Source for ArxivSource {
    fn name(&self) -> &str {
        "arxiv"
    }

    fn can_serve(&self, _profile: &CapabilityProfile, ref_: &Ref) -> bool {
        matches!(ref_, Ref::Arxiv(_))
    }

    async fn fetch(
        &self,
        ref_: &Ref,
        _profile: &CapabilityProfile,
        ctx: &FetchContext,
    ) -> Result<FetchResult, FetchError> {
        // Eligibility gate. The orchestrator is expected to call
        // `can_serve` first, but a runtime check here gives a clean error
        // path if it does not.
        let id = match ref_ {
            Ref::Arxiv(a) => a,
            Ref::Doi(_) => {
                return Err(FetchError::NotEligible {
                    source_key: "arxiv".into(),
                });
            }
        };

        // Hold the rate-limiter permit for the duration of the HTTP
        // fetch. Drop happens at end of scope after the log append below.
        let _permit = ctx.rate_limiter.acquire(self.name()).await;

        // ----- Atom-feed metadata leg (best-effort) -------------------
        //
        // Fetched BEFORE the PDF so that `FetchResult::metadata_json` is
        // populated for a single-pass fetch (the orchestrator does not
        // need to re-issue a metadata-only call). Failures here degrade
        // gracefully: we set `metadata_json = None`, emit a tracing
        // warning, and proceed with the PDF leg unchanged. NO log row
        // is emitted from this leg — the source-level audit unit is
        // "one fetch attempt = one row" and the row comes from the PDF
        // leg below. This is what preserves the 4-row sequence asserted
        // by `crates/doiget-cli/tests/fetch_arxiv_e2e.rs`.
        let metadata_json = match self.metadata_url(id) {
            Ok(meta_url) => match ctx.http.fetch_bytes(self.name(), meta_url).await {
                Ok((bytes, _final)) => match parse_atom_feed(&bytes) {
                    Ok(v) => Some(v),
                    Err(e) => {
                        tracing::warn!(
                            arxiv_id = %id.as_str(),
                            error = %e,
                            "arxiv Atom feed parse failed; continuing with PDF-only fetch"
                        );
                        None
                    }
                },
                Err(e) => {
                    tracing::warn!(
                        arxiv_id = %id.as_str(),
                        error = %e,
                        "arxiv Atom feed fetch failed; continuing with PDF-only fetch"
                    );
                    None
                }
            },
            Err(e) => {
                tracing::warn!(
                    arxiv_id = %id.as_str(),
                    error = %e,
                    "arxiv metadata URL construction failed; continuing with PDF-only fetch"
                );
                None
            }
        };

        // ----- PDF leg -------------------------------------------------
        //
        // #493: arXiv's terms cap REQUESTS, not attempts, and this is the
        // second request of this attempt -- the Atom leg above was the
        // first. The `acquire` permit paced that one; without this the two
        // went out back to back, so even a perfectly serialised caller
        // broke the published interval.
        ctx.rate_limiter.pace(self.name()).await;

        let url = self.pdf_url(id)?;

        // `fetch_pdf` enforces the magic-byte check (`%PDF-`) per
        // `docs/SECURITY.md` §1.2 — non-PDF response surfaces as
        // `HttpError::NotAPdf`, which `From` converts to `FetchError::Http`.
        let (body, final_url): (Bytes, Url) = ctx.http.fetch_pdf(self.name(), url).await?;

        // One `event=fetch` row per attempt, per `docs/ARCHITECTURE.md` §6
        // and `docs/PROVENANCE_LOG.md` §3. Per `docs/SECURITY.md` §1.8 a
        // log write failure is fail-closed — the `?` aborts the fetch.
        // ADR-0021 §1 canonical-digest: build under the "arxiv" resolver
        // profile. version=None in Slice 4 — a follow-up may surface
        // the `vN` discriminator from the Atom-feed `id` element.
        let canonical = ref_.promote(self.name(), None).digest_hex();
        ctx.log.append(RowInput {
            event: LogEvent::Fetch,
            result: LogResult::Ok,
            capability: Capability::Oa,
            ref_: Some(id.as_str()),
            source: Some(self.name()),
            error_code: None,
            size_bytes: Some(body.len() as u64),
            // arXiv does not expose a per-item license string; the
            // platform-wide license declaration lives at
            // <https://info.arxiv.org/help/license/>. Phase 1 records
            // `"arxiv-default"` so the value is informative without
            // claiming a specific Creative Commons license.
            license: Some("arxiv-default"),
            store_path: None,
            canonical_digest: Some(&canonical),
        })?;

        Ok(FetchResult {
            source: self.name().to_string(),
            license: "arxiv-default".into(),
            pdf_bytes: Some(body),
            final_url: Some(final_url),
            metadata_json,
        })
    }
}

// ---------------------------------------------------------------------------
// Atom-feed parser (B.1)
// ---------------------------------------------------------------------------

/// Parse the arXiv Atom-feed response body into a structured JSON
/// metadata object.
///
/// Endpoint: `https://export.arxiv.org/api/query?id_list=<id>` (see
/// arXiv API user manual §3.1). The response is an `<feed>` document
/// containing one `<entry>` per requested id. We extract the fields
/// listed in `docs/SOURCES.md` §4 arXiv (title, summary/abstract,
/// authors, published, updated, categories) into the synthetic JSON
/// shape:
///
/// ```jsonc
/// {
///   "title": "...",
///   "abstract": "...",
///   "authors": ["Family, Given", ...],
///   "published": "YYYY-MM-DDTHH:MM:SSZ",  // RFC3339 UTC, passed through verbatim
///   "updated":   "YYYY-MM-DDTHH:MM:SSZ",
///   "categories": ["cs.LG", "stat.ML"],
///   "doi": "10.1103/...",          // PUBLISHED (journal) DOI cross-ref, NOT this entry's id; omit-when-absent (#281 item 5)
///   "journal_ref": "Phys. Rev. ..."  // omit-when-absent
/// }
/// ```
///
/// All fields are best-effort: any missing element is omitted from the
/// JSON output (NOT serialized as `null`). The parser is a small
/// `quick-xml` event walker — no DOM allocation. Only the FIRST `<entry>`
/// element is consumed (we always query a single id).
///
/// # Errors
///
/// Returns [`FetchError::SourceSchema`] if the XML is malformed (parser
/// reports a syntax error), or [`FetchError::NotFound`] if no `<entry>`
/// element is present (arXiv returns HTTP 200 with an empty `<feed>` on an
/// unknown id — an authoritative absence, not a parse error).
pub(crate) fn parse_atom_feed(xml: &[u8]) -> Result<Value, FetchError> {
    let mut reader = Reader::from_reader(xml);
    let config = reader.config_mut();
    config.trim_text(true);

    // Top-level state. `in_entry` tracks whether we are inside the first
    // (and only) `<entry>` element; once we exit, we stop collecting.
    let mut in_entry = false;
    let mut saw_entry = false;
    let mut depth = 0_i32; // depth WITHIN the entry; 0 = at <entry> root

    // Accumulators. Per-author state is kept on a stack so a nested
    // `<author><name>...</name></author>` populates the right slot.
    let mut title: Option<String> = None;
    let mut abstract_: Option<String> = None;
    let mut published: Option<String> = None;
    let mut updated: Option<String> = None;
    let mut authors: Vec<String> = Vec::new();
    let mut categories: Vec<String> = Vec::new();
    // arXiv-namespaced elements (`<arxiv:doi>`, `<arxiv:journal_ref>`):
    // present only when the submitter supplied a published DOI / journal
    // reference. They are the canonical arXiv → published-DOI link source
    // (#281 item 5), surfaced here so the metadata path carries them.
    let mut doi: Option<String> = None;
    let mut journal_ref: Option<String> = None;

    // Current text-collection target — None when we are not inside a
    // leaf element whose text we want.
    #[derive(Clone, Copy)]
    enum Target {
        Title,
        Summary,
        Published,
        Updated,
        AuthorName,
        Doi,
        JournalRef,
    }
    let mut target: Option<Target> = None;
    let mut in_author = false;
    let mut buf: Vec<u8> = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name_bytes = e.name();
                let local = local_name(name_bytes.as_ref());
                if !in_entry {
                    if local == b"entry" {
                        in_entry = true;
                        saw_entry = true;
                        depth = 0;
                    }
                    buf.clear();
                    continue;
                }
                depth += 1;
                // Depth==1 means a direct child of `<entry>`.
                if depth == 1 {
                    match local {
                        b"title" => target = Some(Target::Title),
                        b"summary" => target = Some(Target::Summary),
                        b"published" => target = Some(Target::Published),
                        b"updated" => target = Some(Target::Updated),
                        // arXiv namespace; `local_name` strips the `arxiv:`
                        // prefix, so these match `<arxiv:doi>` /
                        // `<arxiv:journal_ref>`.
                        b"doi" => target = Some(Target::Doi),
                        b"journal_ref" => target = Some(Target::JournalRef),
                        b"author" => {
                            in_author = true;
                            authors.push(String::new());
                        }
                        _ => {}
                    }
                } else if depth == 2 && in_author && local == b"name" {
                    target = Some(Target::AuthorName);
                }
                buf.clear();
            }
            Ok(Event::Empty(e)) => {
                let name_bytes = e.name();
                let local = local_name(name_bytes.as_ref());
                if in_entry && depth == 0 && local == b"category" {
                    // <category term="cs.LG" scheme="..."/> — extract `term`.
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"term" {
                            // quick-xml 0.40: `unescape_value()` is
                            // deprecated in favour of `normalized_value()`
                            // (attribute-value normalization resolves the
                            // same character/entity references). arXiv's
                            // Atom feed is XML 1.0.
                            if let Ok(v) = attr.normalized_value(quick_xml::XmlVersion::Explicit1_0)
                            {
                                categories.push(v.into_owned());
                            }
                        }
                    }
                }
                buf.clear();
            }
            Ok(Event::Text(t)) => {
                if let Some(tg) = target {
                    // quick-xml 0.40 removed `BytesText::unescape`.
                    // Reproduce the old behaviour: decode the bytes, then
                    // unescape XML entities via `quick_xml::escape::unescape`.
                    // Best-effort — skip the text on decode/unescape error.
                    if let Some(s) = t.decode().ok().and_then(|raw| {
                        quick_xml::escape::unescape(&raw)
                            .ok()
                            .map(|c| c.into_owned())
                    }) {
                        match tg {
                            Target::Title => title.get_or_insert_with(String::new).push_str(&s),
                            Target::Summary => {
                                abstract_.get_or_insert_with(String::new).push_str(&s)
                            }
                            Target::Published => {
                                published.get_or_insert_with(String::new).push_str(&s)
                            }
                            Target::Updated => updated.get_or_insert_with(String::new).push_str(&s),
                            Target::Doi => doi.get_or_insert_with(String::new).push_str(&s),
                            Target::JournalRef => {
                                journal_ref.get_or_insert_with(String::new).push_str(&s)
                            }
                            Target::AuthorName => {
                                if let Some(last) = authors.last_mut() {
                                    last.push_str(&s);
                                }
                            }
                        }
                    }
                }
                buf.clear();
            }
            Ok(Event::End(e)) => {
                if !in_entry {
                    buf.clear();
                    continue;
                }
                let name_bytes = e.name();
                let local = local_name(name_bytes.as_ref());
                if depth == 0 && local == b"entry" {
                    // Done with the first entry — stop. We deliberately
                    // ignore any subsequent entries since the orchestrator
                    // always queries a single id.
                    break;
                }
                depth -= 1;
                if depth == 0 {
                    if local == b"author" {
                        in_author = false;
                        // Drop empty author names (defensive).
                        if let Some(last) = authors.last() {
                            if last.is_empty() {
                                authors.pop();
                            }
                        }
                    }
                    target = None;
                } else if depth == 1 && in_author && local == b"name" {
                    target = None;
                }
                buf.clear();
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(FetchError::SourceSchema {
                    hint: format!("arxiv Atom XML parse error: {e}"),
                });
            }
            // CDATA / Comment / Decl / PI / DocType — ignored.
            _ => {
                buf.clear();
            }
        }
    }

    if !saw_entry {
        // arXiv signals an unknown id with HTTP 200 + an empty `<feed>`
        // (no `<entry>`), NOT a 404. Surface it as an authoritative
        // absence so `doiget verify` classifies it `absent` (a dead
        // reference) rather than a tolerable transport blip.
        return Err(FetchError::NotFound {
            hint: "arxiv Atom feed had no <entry> element (unknown id?)".into(),
        });
    }

    // Build the JSON object, omitting empty optionals. `serde_json::Map`
    // preserves insertion order so the output is stable.
    let mut obj = serde_json::Map::new();
    if let Some(t) = title {
        let trimmed = t.trim().to_string();
        if !trimmed.is_empty() {
            obj.insert("title".into(), Value::String(trimmed));
        }
    }
    if let Some(a) = abstract_ {
        let trimmed = a.trim().to_string();
        if !trimmed.is_empty() {
            obj.insert("abstract".into(), Value::String(trimmed));
        }
    }
    if !authors.is_empty() {
        obj.insert(
            "authors".into(),
            Value::Array(authors.into_iter().map(Value::String).collect()),
        );
    }
    if let Some(p) = published {
        let trimmed = p.trim().to_string();
        if !trimmed.is_empty() {
            obj.insert("published".into(), Value::String(trimmed));
        }
    }
    if let Some(u) = updated {
        let trimmed = u.trim().to_string();
        if !trimmed.is_empty() {
            obj.insert("updated".into(), Value::String(trimmed));
        }
    }
    // arXiv → published-DOI link (#281 item 5): omitted when the submitter
    // did not supply a DOI / journal reference.
    //
    // HAZARD: this `doi` is the PUBLISHED (journal) DOI, NOT this arXiv
    // record's own identifier. It must NOT be promoted to the reserved
    // top-level `doi` of the store `Metadata` (STORE.md) — that field is the
    // entry's own identity. `orchestrator::build_metadata_only_metadata`
    // correctly forces an arXiv entry's `doi` to `None`; any future consumer
    // mapping `metadata_json["doi"]` into `Metadata.doi` would write the
    // wrong identity. Treat this strictly as a cross-reference.
    if let Some(d) = doi {
        let trimmed = d.trim().to_string();
        if !trimmed.is_empty() {
            obj.insert("doi".into(), Value::String(trimmed));
        }
    }
    if let Some(j) = journal_ref {
        let trimmed = j.trim().to_string();
        if !trimmed.is_empty() {
            obj.insert("journal_ref".into(), Value::String(trimmed));
        }
    }
    if !categories.is_empty() {
        obj.insert(
            "categories".into(),
            Value::Array(categories.into_iter().map(Value::String).collect()),
        );
    }
    Ok(json!(obj))
}

/// Strip an XML namespace prefix from a qualified name, returning the
/// local-part bytes. `b"atom:entry"` -> `b"entry"`. Atom uses the default
/// namespace so most names arrive unprefixed; this helper makes the
/// parser robust to either form without depending on quick-xml's
/// namespace resolver (which would require us to thread a
/// `NsReader` and explicit prefix bindings through every event).
fn local_name(qname: &[u8]) -> &[u8] {
    match qname.iter().rposition(|&b| b == b':') {
        Some(idx) => &qname[idx + 1..],
        None => qname,
    }
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
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::http::{HttpClient, HttpError};
    use crate::provenance::{LogRow, ProvenanceLog};
    use crate::rate_limiter::RateLimiter;
    use crate::source::FetchContext;
    use crate::{ArxivId, CapabilityProfile, Doi, RateLimits, Ref};

    const TEST_SESSION_ID: &str = "01J0000000000000000000TEST";

    /// Build a complete `FetchContext` against a wiremock host for use in
    /// the source-level tests below.
    fn build_test_context(wiremock_host: &str) -> (TempDir, FetchContext) {
        let td = TempDir::new().expect("tempdir");
        let log_dir =
            Utf8PathBuf::try_from(td.path().to_path_buf()).expect("temp dir path must be UTF-8");
        let log_path = log_dir.join("test.jsonl");

        let http = Arc::new(HttpClient::new_for_tests_allow_http("arxiv", wiremock_host));
        let rate_limiter = Arc::new(RateLimiter::new(RateLimits::HARD_CODED));
        let session_id = TEST_SESSION_ID.to_string();
        let log = Arc::new(
            ProvenanceLog::open(log_path, session_id.clone()).expect("provenance log opens"),
        );

        (
            td,
            FetchContext {
                http,
                rate_limiter,
                log,
                session_id,
                cache_root: None,
            },
        )
    }

    fn read_rows(path: &camino::Utf8Path) -> Vec<LogRow> {
        let raw = std::fs::read_to_string(path).expect("read log");
        raw.lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str::<LogRow>(l).expect("valid LogRow"))
            .collect()
    }

    fn profile() -> CapabilityProfile {
        CapabilityProfile::for_tests()
    }

    // -----------------------------------------------------------------
    // can_serve
    // -----------------------------------------------------------------

    #[test]
    fn arxiv_can_serve_returns_true_for_arxiv() {
        let s = ArxivSource::new();
        let id = ArxivId::parse("2401.12345").expect("valid id");
        let r = Ref::Arxiv(id);
        assert!(s.can_serve(&profile(), &r));
    }

    #[test]
    fn production_metadata_url_uses_export_host_pdf_uses_arxiv() {
        // Regression guard: the Atom metadata leg MUST hit
        // export.arxiv.org, while PDFs hit arxiv.org. Sending metadata to
        // arxiv.org/api/query redirects and fails the resolve.
        let s = ArxivSource::new();
        let id = ArxivId::parse("1706.03762").expect("valid id");
        let meta = s.metadata_url(&id).expect("meta url");
        assert_eq!(meta.host_str(), Some("export.arxiv.org"));
        assert_eq!(meta.path(), "/api/query");
        let pdf = s.pdf_url(&id).expect("pdf url");
        assert_eq!(pdf.host_str(), Some("arxiv.org"));
    }

    #[test]
    fn with_base_shares_one_origin_for_both_legs() {
        // The DOIGET_ARXIV_BASE override (wiremock) serves both paths from
        // a single origin, so meta and PDF must resolve to the same host.
        let s = ArxivSource::with_base("http://127.0.0.1:9999".parse().expect("url"));
        let id = ArxivId::parse("2401.12345").expect("valid id");
        assert_eq!(
            s.metadata_url(&id).expect("meta").host_str(),
            s.pdf_url(&id).expect("pdf").host_str()
        );
    }

    #[test]
    fn arxiv_can_serve_returns_false_for_doi() {
        let s = ArxivSource::new();
        let r = Ref::Doi(Doi("10.1234/example".to_string()));
        assert!(!s.can_serve(&profile(), &r));
    }

    // -----------------------------------------------------------------
    // fetch — happy paths
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn arxiv_fetch_new_style_id_returns_pdf_bytes() {
        let server = MockServer::start().await;
        let body = b"%PDF-1.7\n%fixture\n".to_vec();
        Mock::given(method("GET"))
            .and(path("/pdf/2401.12345.pdf"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
            .mount(&server)
            .await;

        let host = server
            .uri()
            .parse::<Url>()
            .unwrap()
            .host_str()
            .unwrap()
            .to_string();
        let (_td, ctx) = build_test_context(&host);
        let s = ArxivSource::with_base(server.uri().parse().unwrap());

        let id = ArxivId::parse("2401.12345").unwrap();
        let r = Ref::Arxiv(id);
        let res = s.fetch(&r, &profile(), &ctx).await.expect("fetch ok");

        assert_eq!(res.source, "arxiv");
        assert_eq!(res.license, "arxiv-default");
        let bytes = res.pdf_bytes.expect("pdf bytes set");
        assert!(
            bytes.starts_with(b"%PDF-"),
            "expected PDF magic prefix, got {:?}",
            &bytes[..bytes.len().min(8)]
        );
        assert_eq!(&bytes[..], &body[..]);
    }

    #[tokio::test]
    async fn arxiv_fetch_old_style_id_returns_pdf_bytes() {
        // Old-style id contains `/` (`cond-mat/9501001`); the URL must
        // become `/pdf/cond-mat/9501001.pdf`. This pins the URL-builder
        // behavior across both id shapes.
        let server = MockServer::start().await;
        let body = b"%PDF-1.4\n%old-style fixture\n".to_vec();
        Mock::given(method("GET"))
            .and(path("/pdf/cond-mat/9501001.pdf"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
            .mount(&server)
            .await;

        let host = server
            .uri()
            .parse::<Url>()
            .unwrap()
            .host_str()
            .unwrap()
            .to_string();
        let (_td, ctx) = build_test_context(&host);
        let s = ArxivSource::with_base(server.uri().parse().unwrap());

        let id = ArxivId::parse("cond-mat/9501001").expect("old-style id");
        let r = Ref::Arxiv(id);
        let res = s.fetch(&r, &profile(), &ctx).await.expect("fetch ok");

        let bytes = res.pdf_bytes.expect("pdf bytes set");
        assert!(bytes.starts_with(b"%PDF-"));
        assert_eq!(&bytes[..], &body[..]);
    }

    // -----------------------------------------------------------------
    // fetch — error paths
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn arxiv_fetch_with_doi_ref_errors_not_eligible() {
        let server = MockServer::start().await;
        let host = server
            .uri()
            .parse::<Url>()
            .unwrap()
            .host_str()
            .unwrap()
            .to_string();
        let (_td, ctx) = build_test_context(&host);
        let s = ArxivSource::with_base(server.uri().parse().unwrap());

        let r = Ref::Doi(Doi("10.1234/example".to_string()));
        let err = s
            .fetch(&r, &profile(), &ctx)
            .await
            .expect_err("doi ref must not be eligible");
        match err {
            FetchError::NotEligible { source_key } => {
                assert_eq!(source_key, "arxiv");
            }
            other => panic!("expected NotEligible, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn arxiv_fetch_writes_log_row_with_arxiv_default_license() {
        let server = MockServer::start().await;
        let body = b"%PDF-1.7\n%log-row fixture\n".to_vec();
        Mock::given(method("GET"))
            .and(path("/pdf/2401.12345.pdf"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
            .mount(&server)
            .await;
        let host = server
            .uri()
            .parse::<Url>()
            .unwrap()
            .host_str()
            .unwrap()
            .to_string();
        let (_td, ctx) = build_test_context(&host);
        // Capture the log path before the fetch call for later read-back.
        let log_path = ctx.log.path().to_path_buf();
        let s = ArxivSource::with_base(server.uri().parse().unwrap());

        let id = ArxivId::parse("2401.12345").unwrap();
        let r = Ref::Arxiv(id);
        let _ = s.fetch(&r, &profile(), &ctx).await.expect("fetch ok");

        let rows = read_rows(&log_path);
        assert_eq!(rows.len(), 1, "exactly one fetch row expected");
        let row = &rows[0];
        assert_eq!(row.source.as_deref(), Some("arxiv"));
        assert_eq!(row.ref_.as_deref(), Some("2401.12345"));
        assert_eq!(row.license.as_deref(), Some("arxiv-default"));
        assert_eq!(row.size_bytes, Some(body.len() as u64));
        assert!(row.error_code.is_none());
    }

    #[tokio::test]
    async fn arxiv_non_pdf_body_rejected() {
        // Wiremock returns 200 with a non-PDF body. The magic-byte check
        // inside `HttpClient::fetch_pdf` rejects it as `HttpError::NotAPdf`,
        // surfacing as `FetchError::Http`.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/pdf/2401.12345.pdf"))
            .respond_with(
                ResponseTemplate::new(200).set_body_bytes(b"<html>not a pdf</html>".to_vec()),
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
        let (_td, ctx) = build_test_context(&host);
        let s = ArxivSource::with_base(server.uri().parse().unwrap());

        let id = ArxivId::parse("2401.12345").unwrap();
        let r = Ref::Arxiv(id);
        let err = s
            .fetch(&r, &profile(), &ctx)
            .await
            .expect_err("non-pdf body must be rejected");
        match err {
            FetchError::Http(HttpError::NotAPdf { got }) => {
                assert_eq!(&got, b"<html");
            }
            other => panic!("expected FetchError::Http(NotAPdf), got {:?}", other),
        }
    }

    #[tokio::test]
    async fn arxiv_404_maps_to_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/pdf/2401.99999.pdf"))
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
        let (_td, ctx) = build_test_context(&host);
        let s = ArxivSource::with_base(server.uri().parse().unwrap());

        let id = ArxivId::parse("2401.99999").unwrap();
        let r = Ref::Arxiv(id);
        let err = s
            .fetch(&r, &profile(), &ctx)
            .await
            .expect_err("404 must surface");
        match err {
            FetchError::Http(HttpError::HttpStatus { status, .. }) => {
                assert_eq!(status, 404);
            }
            other => panic!("expected FetchError::Http(HttpStatus), got {:?}", other),
        }
    }

    // -----------------------------------------------------------------
    // parse_atom_feed (B.1) — unit tests
    // -----------------------------------------------------------------

    /// Synthetic Atom payload from the Slice 1 spec (deliverable B.3). Do
    /// not hit real arXiv from tests.
    const SAMPLE_ATOM_FEED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <entry>
    <id>http://arxiv.org/abs/2401.12345v1</id>
    <updated>2024-02-01T00:00:00Z</updated>
    <published>2024-01-15T00:00:00Z</published>
    <title>Example arXiv Paper Title</title>
    <summary>This is an example abstract.</summary>
    <author>
      <name>Jane Doe</name>
    </author>
    <author>
      <name>John Roe</name>
    </author>
    <category term="cs.LG" scheme="http://arxiv.org/schemas/atom"/>
    <category term="stat.ML" scheme="http://arxiv.org/schemas/atom"/>
  </entry>
</feed>"#;

    #[test]
    fn parse_atom_feed_extracts_all_fields() {
        let v = parse_atom_feed(SAMPLE_ATOM_FEED.as_bytes()).expect("Atom parses");
        assert_eq!(v["title"], serde_json::json!("Example arXiv Paper Title"));
        assert_eq!(
            v["abstract"],
            serde_json::json!("This is an example abstract.")
        );
        assert_eq!(v["authors"], serde_json::json!(["Jane Doe", "John Roe"]));
        assert_eq!(v["published"], serde_json::json!("2024-01-15T00:00:00Z"));
        assert_eq!(v["updated"], serde_json::json!("2024-02-01T00:00:00Z"));
        assert_eq!(v["categories"], serde_json::json!(["cs.LG", "stat.ML"]));
    }

    #[test]
    fn parse_atom_feed_empty_feed_is_not_found() {
        // An unknown arXiv id yields HTTP 200 + an empty `<feed>`. That is
        // an authoritative absence (→ `FetchError::NotFound` →
        // `ErrorCode::NotFound` → verify `absent`), NOT a schema error.
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom"></feed>"#;
        let err = parse_atom_feed(xml.as_bytes()).expect_err("empty feed must error");
        match err {
            FetchError::NotFound { hint } => {
                assert!(
                    hint.contains("entry"),
                    "expected mention of <entry>; got {hint}"
                );
            }
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn parse_atom_feed_captures_published_doi_and_journal_ref() {
        // When the submitter supplied a published DOI / journal reference,
        // arXiv emits `<arxiv:doi>` / `<arxiv:journal_ref>` (the arXiv
        // namespace). They are the arXiv → published-DOI link (#281 item 5)
        // and must surface in the metadata JSON. Absent on most entries.
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom" xmlns:arxiv="http://arxiv.org/schemas/atom">
  <entry>
    <id>http://arxiv.org/abs/2101.54321v2</id>
    <title>Published Later</title>
    <arxiv:doi>10.1103/PhysRevLett.130.200601</arxiv:doi>
    <arxiv:journal_ref>Phys. Rev. Lett. 130, 200601 (2023)</arxiv:journal_ref>
  </entry>
</feed>"#;
        let v = parse_atom_feed(xml.as_bytes()).expect("parses");
        assert_eq!(
            v["doi"],
            serde_json::json!("10.1103/PhysRevLett.130.200601")
        );
        assert_eq!(
            v["journal_ref"],
            serde_json::json!("Phys. Rev. Lett. 130, 200601 (2023)")
        );
    }

    #[test]
    fn parse_atom_feed_omits_doi_when_absent() {
        // The common case: no published DOI yet → no `doi` / `journal_ref`
        // key (omitted, not null).
        let v = parse_atom_feed(SAMPLE_ATOM_FEED.as_bytes()).expect("parses");
        let obj = v.as_object().expect("object");
        assert!(!obj.contains_key("doi"), "doi must be omitted: {obj:?}");
        assert!(
            !obj.contains_key("journal_ref"),
            "journal_ref must be omitted: {obj:?}"
        );
    }

    #[test]
    fn parse_atom_feed_journal_ref_only_without_doi() {
        // A real, common state: a journal_ref but no DOI. The `doi` key must
        // be absent while `journal_ref` is present (independent extraction).
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom" xmlns:arxiv="http://arxiv.org/schemas/atom">
  <entry>
    <id>http://arxiv.org/abs/2101.00001v1</id>
    <title>Journal Ref Only</title>
    <arxiv:journal_ref>J. Stat. Mech. (2021) 013203</arxiv:journal_ref>
  </entry>
</feed>"#;
        let v = parse_atom_feed(xml.as_bytes()).expect("parses");
        let obj = v.as_object().expect("object");
        assert!(!obj.contains_key("doi"), "doi must be omitted: {obj:?}");
        assert_eq!(
            obj.get("journal_ref").and_then(Value::as_str),
            Some("J. Stat. Mech. (2021) 013203")
        );
    }

    #[test]
    fn parse_atom_feed_whitespace_doi_is_omitted() {
        // A whitespace-only `<arxiv:doi>` trims to empty and must be omitted,
        // not emitted as `""` (exercises the trim→empty omit branch).
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom" xmlns:arxiv="http://arxiv.org/schemas/atom">
  <entry>
    <id>http://arxiv.org/abs/2101.00002v1</id>
    <title>Blank DOI</title>
    <arxiv:doi>   </arxiv:doi>
  </entry>
</feed>"#;
        let v = parse_atom_feed(xml.as_bytes()).expect("parses");
        assert!(
            !v.as_object().expect("object").contains_key("doi"),
            "whitespace-only doi must be omitted: {v:?}"
        );
    }

    #[test]
    fn parse_atom_feed_omits_missing_optional_fields() {
        // An entry with only an id and title — abstract/authors/categories
        // absent. The output must omit those keys entirely (not emit
        // `null`).
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <entry>
    <id>http://arxiv.org/abs/2401.00001v1</id>
    <title>Minimal Entry</title>
  </entry>
</feed>"#;
        let v = parse_atom_feed(xml.as_bytes()).expect("parses");
        let obj = v.as_object().expect("object");
        assert_eq!(
            obj.get("title").and_then(Value::as_str),
            Some("Minimal Entry")
        );
        assert!(
            !obj.contains_key("abstract"),
            "abstract should be omitted: {obj:?}"
        );
        assert!(
            !obj.contains_key("authors"),
            "authors should be omitted: {obj:?}"
        );
        assert!(
            !obj.contains_key("categories"),
            "categories should be omitted: {obj:?}"
        );
    }

    // -----------------------------------------------------------------
    // fetch_metadata_only — orchestrator entry point
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn arxiv_fetch_metadata_only_returns_atom_metadata() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/query"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_ATOM_FEED))
            .mount(&server)
            .await;
        let host = server
            .uri()
            .parse::<Url>()
            .unwrap()
            .host_str()
            .unwrap()
            .to_string();
        let (_td, ctx) = build_test_context(&host);
        let s = ArxivSource::with_base(server.uri().parse().unwrap());
        let id = ArxivId::parse("2401.12345").unwrap();

        let meta = s
            .fetch_metadata_only(&id, &ctx)
            .await
            .expect("metadata_only ok");
        assert_eq!(
            meta["title"],
            serde_json::json!("Example arXiv Paper Title")
        );
        assert_eq!(meta["authors"], serde_json::json!(["Jane Doe", "John Roe"]));
    }

    #[tokio::test]
    async fn arxiv_fetch_populates_metadata_json_when_atom_endpoint_mocked() {
        // Full Source::fetch with BOTH Atom and PDF endpoints mocked must
        // populate `metadata_json` from the Atom response.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/query"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE_ATOM_FEED))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/pdf/2401.12345.pdf"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"%PDF-1.7\n%fix\n".to_vec()))
            .mount(&server)
            .await;
        let host = server
            .uri()
            .parse::<Url>()
            .unwrap()
            .host_str()
            .unwrap()
            .to_string();
        let (_td, ctx) = build_test_context(&host);
        let s = ArxivSource::with_base(server.uri().parse().unwrap());
        let id = ArxivId::parse("2401.12345").unwrap();
        let r = Ref::Arxiv(id);

        let res = s.fetch(&r, &profile(), &ctx).await.expect("fetch ok");
        let meta = res.metadata_json.expect("metadata_json populated");
        assert_eq!(
            meta["title"],
            serde_json::json!("Example arXiv Paper Title")
        );
    }

    #[tokio::test]
    async fn arxiv_fetch_atom_failure_falls_back_to_pdf_only() {
        // PDF endpoint mocked; Atom endpoint deliberately unmocked
        // (will 404). The fetch must still succeed with
        // `metadata_json = None` — the best-effort contract.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/pdf/2401.12345.pdf"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"%PDF-1.7\nx".to_vec()))
            .mount(&server)
            .await;
        let host = server
            .uri()
            .parse::<Url>()
            .unwrap()
            .host_str()
            .unwrap()
            .to_string();
        let (_td, ctx) = build_test_context(&host);
        let s = ArxivSource::with_base(server.uri().parse().unwrap());
        let id = ArxivId::parse("2401.12345").unwrap();
        let r = Ref::Arxiv(id);

        let res = s.fetch(&r, &profile(), &ctx).await.expect("fetch ok");
        assert!(res.metadata_json.is_none());
        assert!(res.pdf_bytes.is_some());
    }
}
