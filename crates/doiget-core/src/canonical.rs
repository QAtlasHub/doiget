//! Canonical-tuple audit identity for fetched papers (ADR-0021 §1, ADR-0024).
//!
//! Binding spec: [`docs/DECISIONS/0021-canonical-tuple-identity.md`](../../../docs/DECISIONS/0021-canonical-tuple-identity.md)
//! §1 (NORMATIVE shape + digest algorithm) and
//! [`docs/DECISIONS/0024-canonical-ref-impl.md`](../../../docs/DECISIONS/0024-canonical-ref-impl.md)
//! (implementation supersession).
//!
//! # Algorithm (NORMATIVE)
//!
//! ```text
//! canonical_digest := SHA256( source_type | 0x00 | source_id | 0x00
//!                           | resolver_profile | 0x00 | version_or_empty )
//! ```
//!
//! where `|` is byte concatenation, `0x00` is the single-byte field
//! separator, and `version_or_empty` is the literal empty byte sequence
//! when `version` is `None` (ADR-0021 §1 Slice 2 clarification — NO
//! sentinel like `"null"` / `"none"` / `"-"`).
//!
//! The four `source_type` wire tokens fed into the digest match the
//! lowercase variant names used by [`Ref`]'s tagged serde encoding:
//! `"doi"`, `"arxiv"`. Future variants (Pmid, Handle, ...) MUST use
//! their lowercase variant name as the wire token.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::Ref;

/// Identifier-class tag for a [`CanonicalRef`].
///
/// Marked `#[non_exhaustive]` so adding new classes (Pmid, Handle, etc.)
/// in a future minor bump is non-breaking. The lowercase variant name is
/// the byte string fed into the canonical-digest hash input
/// (ADR-0021 §1) — renaming a variant is a breaking change to the
/// digest contract.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum SourceType {
    /// DOI source class.
    Doi,
    /// arXiv source class.
    Arxiv,
}

impl SourceType {
    /// The lowercase wire token used in the canonical-digest hash input
    /// (ADR-0021 §1) and in the JSON wire form.
    pub fn as_wire_str(&self) -> &'static str {
        match self {
            SourceType::Doi => "doi",
            SourceType::Arxiv => "arxiv",
        }
    }
}

/// Four-tuple audit identity of a fetched paper (ADR-0021 §1).
///
/// Two fetches of the same DOI through different resolvers (e.g.
/// Crossref vs. Unpaywall) produce two distinct `CanonicalRef` values
/// and therefore two distinct [`CanonicalRef::digest`] outputs. This is
/// the resolver-distinction the audit log uses to separate "fetched via
/// Crossref" from "fetched via Unpaywall" without changing the on-disk
/// filename derivation (which remains keyed on [`Ref`] via
/// [`crate::Safekey`]).
///
/// Marked `#[non_exhaustive]` per `docs/PUBLIC_API.md` §6 — adding new
/// fields in a future minor bump is non-breaking. External callers
/// construct values via [`Ref::promote`] or [`CanonicalRef::new`], not
/// by struct-literal.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CanonicalRef {
    /// Identifier class (DOI / arXiv / future).
    pub source_type: SourceType,
    /// Validated identifier string (`"10.1234/foo"`, `"2401.12345"`).
    pub source_id: String,
    /// Resolver key that produced this audit identity. One of the
    /// existing resolver keys (`"crossref"`, `"unpaywall"`, `"arxiv"`,
    /// `"oa-publisher"`) plus future ones (`"openalex"`, `"s2"`, ...).
    pub resolver_profile: String,
    /// Optional version (arXiv `"v2"`, Crossref-snapshot date). When
    /// `None`, the digest treats this field as the empty byte sequence
    /// (ADR-0021 §1 Slice 2 clarification — no sentinel).
    pub version: Option<String>,
}

impl CanonicalRef {
    /// Construct a new `CanonicalRef` from its four fields.
    ///
    /// The struct is `#[non_exhaustive]`; external code MUST use this
    /// constructor (or [`Ref::promote`]) rather than a struct literal.
    pub fn new(
        source_type: SourceType,
        source_id: impl Into<String>,
        resolver_profile: impl Into<String>,
        version: Option<String>,
    ) -> Self {
        Self {
            source_type,
            source_id: source_id.into(),
            resolver_profile: resolver_profile.into(),
            version,
        }
    }

    /// Compute the canonical-digest per ADR-0021 §1.
    ///
    /// ```text
    /// SHA256( source_type | 0x00 | source_id | 0x00
    ///       | resolver_profile | 0x00 | version_or_empty )
    /// ```
    ///
    /// `version_or_empty` is the empty byte sequence when
    /// [`Self::version`] is `None` (ADR-0021 §1 NORMATIVE clarification
    /// — no `"null"` / `"none"` / `"-"` sentinel).
    pub fn digest(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(self.source_type.as_wire_str().as_bytes());
        hasher.update([0x00]);
        hasher.update(self.source_id.as_bytes());
        hasher.update([0x00]);
        hasher.update(self.resolver_profile.as_bytes());
        hasher.update([0x00]);
        if let Some(v) = &self.version {
            hasher.update(v.as_bytes());
        }
        // When `version` is None, the trailing input is the empty byte
        // sequence — the SHA-256 state is finalized with no further
        // updates. This matches ADR-0021 §1 Slice 2: no sentinel.
        hasher.finalize().into()
    }

    /// Hex-encoded [`Self::digest`] — 64 lowercase ASCII hex chars.
    pub fn digest_hex(&self) -> String {
        hex::encode(self.digest())
    }
}

impl Ref {
    /// Promote a [`Ref`] to a [`CanonicalRef`] under the given resolver
    /// profile and optional version (ADR-0021 §1).
    ///
    /// `resolver_profile` MUST be the resolver key that produced (or is
    /// about to produce) the audit identity — e.g. `"crossref"` for
    /// Crossref metadata, `"oa-publisher"` for the publisher-side OA
    /// PDF leg, `"arxiv"` for arXiv. The string is byte-for-byte fed
    /// into the SHA-256 input.
    ///
    /// `version` is the optional source-specific version token (arXiv
    /// `"v2"`, Crossref snapshot date). `None` selects the empty-bytes
    /// branch of the digest algorithm.
    pub fn promote(&self, resolver_profile: &str, version: Option<&str>) -> CanonicalRef {
        let (source_type, source_id) = match self {
            Ref::Doi(d) => (SourceType::Doi, d.as_str().to_string()),
            Ref::Arxiv(a) => (SourceType::Arxiv, a.as_str().to_string()),
        };
        CanonicalRef {
            source_type,
            source_id,
            resolver_profile: resolver_profile.to_string(),
            version: version.map(str::to_string),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests — canonical-digest golden vectors (ADR-0021 §1, ADR-0024).
//
// Each vector pins ONE concrete (source_type, source_id, resolver_profile,
// version) tuple to its expected SHA-256 hex digest, cross-checked by an
// in-test reference implementation that re-concatenates the bytes and
// runs one SHA-256 pass. The streaming digest and the reference digest
// must agree byte-for-byte; a divergence is a NORMATIVE-contract break.
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::{ArxivId, Doi};

    /// Reference implementation: concatenate the four parts with single
    /// zero-byte separators and run one SHA-256 pass. Used to
    /// double-check the streaming impl in [`CanonicalRef::digest`].
    fn reference_digest_hex(
        source_type: &str,
        source_id: &str,
        resolver_profile: &str,
        version: Option<&str>,
    ) -> String {
        let mut buf: Vec<u8> = Vec::new();
        buf.extend_from_slice(source_type.as_bytes());
        buf.push(0x00);
        buf.extend_from_slice(source_id.as_bytes());
        buf.push(0x00);
        buf.extend_from_slice(resolver_profile.as_bytes());
        buf.push(0x00);
        if let Some(v) = version {
            buf.extend_from_slice(v.as_bytes());
        }
        let d = Sha256::digest(&buf);
        hex::encode(d)
    }

    #[test]
    fn digest_matches_reference_doi_crossref_no_version() {
        // ADR-0021 §1 — version=None MUST be the empty byte sequence,
        // not a sentinel. The reference impl and the streaming impl
        // must agree.
        let c = CanonicalRef::new(SourceType::Doi, "10.1234/foo", "crossref", None);
        let expected = reference_digest_hex("doi", "10.1234/foo", "crossref", None);
        assert_eq!(c.digest_hex(), expected);
    }

    #[test]
    fn digest_matches_reference_doi_unpaywall_no_version() {
        let c = CanonicalRef::new(SourceType::Doi, "10.1234/foo", "unpaywall", None);
        let expected = reference_digest_hex("doi", "10.1234/foo", "unpaywall", None);
        assert_eq!(c.digest_hex(), expected);
        // Crossref vs. Unpaywall MUST differ — that's the whole point
        // of the audit identity (ADR-0021 Context).
        let c_cross = CanonicalRef::new(SourceType::Doi, "10.1234/foo", "crossref", None);
        assert_ne!(c.digest_hex(), c_cross.digest_hex());
    }

    #[test]
    fn digest_matches_reference_doi_oa_publisher_no_version() {
        let c = CanonicalRef::new(SourceType::Doi, "10.1234/foo", "oa-publisher", None);
        let expected = reference_digest_hex("doi", "10.1234/foo", "oa-publisher", None);
        assert_eq!(c.digest_hex(), expected);
    }

    #[test]
    fn digest_matches_reference_arxiv_no_version() {
        let c = CanonicalRef::new(SourceType::Arxiv, "2401.12345", "arxiv", None);
        let expected = reference_digest_hex("arxiv", "2401.12345", "arxiv", None);
        assert_eq!(c.digest_hex(), expected);
    }

    #[test]
    fn digest_matches_reference_arxiv_with_version_v2() {
        let c = CanonicalRef::new(SourceType::Arxiv, "2401.12345", "arxiv", Some("v2".into()));
        let expected = reference_digest_hex("arxiv", "2401.12345", "arxiv", Some("v2"));
        assert_eq!(c.digest_hex(), expected);
        // v2 MUST differ from version=None.
        let c_none = CanonicalRef::new(SourceType::Arxiv, "2401.12345", "arxiv", None);
        assert_ne!(c.digest_hex(), c_none.digest_hex());
    }

    #[test]
    fn digest_matches_reference_arxiv_with_version_v10() {
        let c = CanonicalRef::new(SourceType::Arxiv, "2401.12345", "arxiv", Some("v10".into()));
        let expected = reference_digest_hex("arxiv", "2401.12345", "arxiv", Some("v10"));
        assert_eq!(c.digest_hex(), expected);
    }

    #[test]
    fn digest_matches_reference_doi_crossref_with_snapshot_date() {
        let c = CanonicalRef::new(
            SourceType::Doi,
            "10.1234/foo",
            "crossref",
            Some("2026-05-12".into()),
        );
        let expected = reference_digest_hex("doi", "10.1234/foo", "crossref", Some("2026-05-12"));
        assert_eq!(c.digest_hex(), expected);
    }

    #[test]
    fn digest_matches_reference_real_publisher_doi() {
        let c = CanonicalRef::new(
            SourceType::Doi,
            "10.1103/PhysRevLett.130.200601",
            "oa-publisher",
            None,
        );
        let expected = reference_digest_hex(
            "doi",
            "10.1103/PhysRevLett.130.200601",
            "oa-publisher",
            None,
        );
        assert_eq!(c.digest_hex(), expected);
    }

    #[test]
    fn digest_some_empty_string_version_equals_none_version() {
        // ADR-0021 §1: the empty-bytes trailing input is selected by
        // BOTH `None` AND `Some("")` — neither appends any bytes after
        // the third 0x00 separator. Pin the equality on the wire.
        let c_some_empty = CanonicalRef::new(
            SourceType::Doi,
            "10.1234/foo",
            "crossref",
            Some(String::new()),
        );
        let c_none = CanonicalRef::new(SourceType::Doi, "10.1234/foo", "crossref", None);
        assert_eq!(c_some_empty.digest_hex(), c_none.digest_hex());
    }

    #[test]
    fn digest_matches_reference_old_style_arxiv() {
        let c = CanonicalRef::new(SourceType::Arxiv, "cond-mat/9501001", "arxiv", None);
        let expected = reference_digest_hex("arxiv", "cond-mat/9501001", "arxiv", None);
        assert_eq!(c.digest_hex(), expected);
    }

    #[test]
    fn digest_hex_is_64_lowercase_hex_chars() {
        // The wire form is 64 lowercase hex chars (SHA-256 = 32 bytes).
        let c = CanonicalRef::new(SourceType::Doi, "10.1234/foo", "crossref", None);
        let s = c.digest_hex();
        assert_eq!(s.len(), 64);
        assert!(
            s.chars()
                .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase()),
            "digest_hex must be lowercase ASCII hex, got {s}"
        );
    }

    #[test]
    fn ref_promote_doi_round_trip() {
        // Spec: Ref::promote builds a CanonicalRef with the given
        // resolver / version and copies the inner id through verbatim.
        let r = Ref::Doi(Doi("10.1234/foo".into()));
        let c = r.promote("crossref", None);
        assert!(matches!(c.source_type, SourceType::Doi));
        assert_eq!(c.source_id, "10.1234/foo");
        assert_eq!(c.resolver_profile, "crossref");
        assert!(c.version.is_none());
    }

    #[test]
    fn ref_promote_arxiv_with_version_round_trip() {
        let r = Ref::Arxiv(ArxivId("2401.12345".into()));
        let c = r.promote("arxiv", Some("v2"));
        assert!(matches!(c.source_type, SourceType::Arxiv));
        assert_eq!(c.source_id, "2401.12345");
        assert_eq!(c.resolver_profile, "arxiv");
        assert_eq!(c.version.as_deref(), Some("v2"));
    }

    #[test]
    fn ref_promote_then_digest_matches_direct_construction() {
        let r = Ref::Doi(Doi("10.1234/foo".into()));
        let c_promoted = r.promote("crossref", None);
        let c_direct = CanonicalRef::new(SourceType::Doi, "10.1234/foo", "crossref", None);
        assert_eq!(c_promoted.digest_hex(), c_direct.digest_hex());
    }

    #[test]
    fn source_type_serializes_lowercase() {
        // The lowercase variant name is the wire token AND the digest
        // input. Both must agree.
        let s = serde_json::to_string(&SourceType::Doi).expect("serialize");
        assert_eq!(s, "\"doi\"");
        let a = serde_json::to_string(&SourceType::Arxiv).expect("serialize");
        assert_eq!(a, "\"arxiv\"");
    }
}
