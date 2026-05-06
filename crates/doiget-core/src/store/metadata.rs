//! Metadata struct matching `docs/STORE.md` §2 / `docs/PUBLIC_API.md` §3.
//!
//! The on-disk wire format is TOML, with the reserved top-level fields named
//! by the spec and any tool-specific table (`[doiget]`, `[bibliofetch]`, ...)
//! beneath. Per `docs/STORE.md` §8, both implementations MUST tolerate
//! unknown top-level fields and unknown tables; this module captures unknown
//! entries through the `other` field via `#[serde(flatten)]` so they
//! survive a read/modify/write round-trip.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Metadata for a single stored entry.
///
/// Reserved top-level fields per `docs/STORE.md` §2. `schema_version` is a
/// string of the form `<MAJOR>.<MINOR>`; the current version this build
/// writes is [`crate::SCHEMA_VERSION`].
///
/// Unknown top-level fields and unknown tables are preserved verbatim
/// through the `other` field, so reading-and-rewriting an entry produced
/// by a future minor revision (or by BiblioFetch.jl) does not silently
/// drop data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metadata {
    /// Schema version of the form `<MAJOR>.<MINOR>`. See `docs/STORE.md` §3.
    pub schema_version: String,
    /// Paper title.
    pub title: String,
    /// List of authors (preserve original ordering).
    pub authors: Vec<String>,
    /// Publication year, if known.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub year: Option<i32>,
    /// DOI, if any.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub doi: Option<crate::Doi>,
    /// arXiv id, if any.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub arxiv_id: Option<crate::ArxivId>,
    /// Abstract; serialized as the bare `abstract` key (Rust keyword).
    #[serde(rename = "abstract", skip_serializing_if = "Option::is_none", default)]
    pub abstract_: Option<String>,
    /// Venue (e.g. journal or conference).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub venue: Option<String>,
    /// Publisher.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub publisher: Option<String>,
    /// ISSN (for journals).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub issn: Option<String>,
    /// ISBN (for books).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub isbn: Option<String>,
    /// Crossref-taxonomy type. Serialized as the bare `type` key.
    #[serde(rename = "type", skip_serializing_if = "Option::is_none", default)]
    pub type_: Option<String>,
    /// Free-form keywords.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub keywords: Vec<String>,
    /// Canonical URL for the entry, if any.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub url: Option<String>,
    /// Path to the stored PDF, relative to the store root.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub pdf_path: Option<String>,
    /// doiget-specific extension table. BiblioFetch.jl ignores it.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub doiget: Option<DoigetExtension>,
    /// All other top-level keys and tables (e.g. `[bibliofetch]`).
    ///
    /// Per `docs/STORE.md` §8 we MUST tolerate unknown top-level fields and
    /// unknown tables. Unknown entries are captured here so a read /
    /// modify / write cycle does not silently drop them. Keys are stored in
    /// a `BTreeMap` so re-serialization is alphabetically ordered, matching
    /// the normalization rule in `docs/STORE.md` §7.
    #[serde(flatten)]
    pub other: std::collections::BTreeMap<String, toml::Value>,
}

/// doiget-specific extension table (`[doiget]`).
///
/// Per `docs/STORE.md` §6, doiget owns this table outright and may
/// overwrite its contents on a re-fetch. BiblioFetch.jl ignores it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoigetExtension {
    /// RFC3339 UTC timestamp of the fetch that produced this entry.
    pub fetched_at: DateTime<Utc>,
    /// Which `Source` produced this entry (e.g. `unpaywall`).
    pub source: String,
    /// OA license string, or the literal `"unknown"`.
    pub license: String,
    /// Size of the stored PDF in bytes.
    pub size_bytes: u64,
    /// ULID of the originating MCP call, if the fetch came in via MCP.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub mcp_call_id: Option<String>,
}
