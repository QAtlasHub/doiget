# Public API (`doiget-core`)

> **Status: NORMATIVE.** This is the semver-locked Rust API surface of the `doiget-core`
> crate. Breaking changes to any item here require a major version bump and an ADR.
> Adding new items is a minor bump.

`doiget-cli` and `doiget-mcp` are not bound by this guarantee — they are end-user
binaries / servers and may evolve more freely.

## 1. Re-exports (top of `lib.rs`)

> **Phase 0 note:** the module paths shown below (`crate::ref_`,
> `crate::capability`, `crate::source`, `crate::store`, `crate::error`,
> `crate::provenance`) describe the **Phase 1+ target layout**. At Phase 0
> HEAD, `doiget-core` is a single `lib.rs` and the same items are visible
> directly from the crate root. The semver-locked surface is the public
> identifier set, not the submodule layout — Phase 1 may split files
> without a major bump.

```rust
pub use crate::ref_::{Ref, Doi, ArxivId};
pub use crate::safekey::Safekey;
pub use crate::capability::{
    AlwaysOn, CapabilityProfile, MetadataAccess, RateLimits, TdmGrant,
};
pub use crate::source::{Source, FetchContext, FetchResult, FetchError};
pub use crate::store::{Store, Metadata, EntryInfo, StoreError};
pub use crate::error::{ErrorCode, DenialContext, DenialReason};
pub use crate::provenance::{ProvenanceLog, LogEvent, LogError};
// Slice 4 / ADR-0024 — audit-identity surface:
pub use crate::canonical::{CanonicalRef, SourceType};
```

## 2. Trait surface

```rust
pub trait Source: Send + Sync {
    fn name(&self) -> &str;
    fn can_serve(&self, profile: &CapabilityProfile, ref_: &Ref) -> bool;
    async fn fetch(
        &self,
        ref_: &Ref,
        profile: &CapabilityProfile,
        ctx: &FetchContext,
    ) -> Result<FetchResult, FetchError>;
}

pub trait Store: Send + Sync {
    fn read(&self, key: &Safekey) -> Result<Option<Metadata>, StoreError>;
    fn write(
        &self,
        key: &Safekey,
        m: &Metadata,
        pdf: Option<&Path>,
    ) -> Result<(), StoreError>;
    fn list_recent(&self, limit: usize) -> Result<Vec<EntryInfo>, StoreError>;
    fn search(&self, query: &str, limit: usize) -> Result<Vec<EntryInfo>, StoreError>;
}
```

## 3. Core types

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Ref {
    Doi(Doi),
    Arxiv(ArxivId),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct Doi(pub(crate) String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct ArxivId(pub(crate) String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct Safekey(pub(crate) String);

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Metadata {
    pub schema_version: String,
    pub title:    String,
    pub authors:  Vec<String>,
    pub year:     Option<i32>,
    pub doi:      Option<Doi>,
    pub arxiv_id: Option<ArxivId>,
    pub abstract_: Option<String>,
    pub venue:    Option<String>,
    pub publisher: Option<String>,
    pub issn:     Option<String>,
    pub isbn:     Option<String>,
    pub type_:    Option<String>,
    pub keywords: Vec<String>,
    pub doiget:   Option<DoigetExtension>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DoigetExtension {
    pub fetched_at: chrono::DateTime<chrono::Utc>,
    pub source:     String,
    pub license:    String,
    pub size_bytes: u64,
    pub mcp_call_id: Option<String>,
}
```

## 4. Constructors and validation

```rust
impl Doi {
    pub fn parse(s: &str) -> Result<Self, RefParseError>;
    pub fn as_str(&self) -> &str;
}

impl ArxivId {
    pub fn parse(s: &str) -> Result<Self, RefParseError>;
    pub fn as_str(&self) -> &str;
}

impl Ref {
    pub fn parse(s: &str) -> Result<Self, RefParseError>;
    pub fn safekey(&self) -> Safekey;
}
```

`parse` returns a [`RefParseError`] variant naming the specific rejection
category (`Empty`, `MissingDoiPrefix`, `MissingDoiSuffixSeparator`,
`InvalidDoiRegistrant`, `EmptyDoiSuffix`, `DoiSuffixTooLong { len, max }`,
`InvalidDoiSuffixChar { ch }`, `InvalidArxivShape`). The granular shape is
preserved for tests and future log breadcrumbs; at the public MCP / CLI
boundary, all variants funnel to [`ErrorCode::InvalidRef`] via the
`impl From<RefParseError> for ErrorCode` blanket conversion, so `?`
propagation collapses to `INVALID_REF` automatically.

`RefParseError` is `#[non_exhaustive]`; adding new categories is a
non-breaking change. Pattern-match with a wildcard arm.

History: prior revisions of this section documented the signature as
`Result<Self, ErrorCode>` (a Phase 0 placeholder before the parse impls
landed). PR #55 introduced the dedicated `RefParseError` type; see also
the [`Unreleased`] entry in [`CHANGELOG.md`](../CHANGELOG.md).

## 5. CapabilityProfile

```rust
impl CapabilityProfile {
    pub fn from_env() -> Result<Self, CapabilityError>;
}

pub enum CapabilityError {
    AgreedButNoKey { agree_var: String, key_var: String },
    KeyButNotAgreed { agree_var: String },
}
```

See [`CAPABILITY.md`](CAPABILITY.md) for the full type definition and resolution rules.

## 6. Stability guarantees

- A type listed here may gain new fields if they are `#[non_exhaustive]` or behind a
  `Default` value; this is a minor bump.
- Removing or renaming any item listed here is a breaking change requiring a major
  version bump and an ADR.
- Items not listed here (private types, `pub(crate)`, types under `__internal::`) carry
  no stability guarantee.
- During the `0.x` line, breaking changes are allowed at any minor bump but must still
  be documented in `CHANGELOG.md` and an ADR. Phase 6 freezes this surface as `1.0`.

## 7. MSRV

`doiget-core`'s declared MSRV (`Cargo.toml [workspace.package] rust-version`) is
**1.86**. Active development tracks `channel = "stable"` in `rust-toolchain.toml`,
so day-to-day builds use the latest stable toolchain; the CI `msrv` job pins
explicitly to 1.86 to verify the declared floor still holds.

Raising the declared MSRV is a **minor** version bump and requires a CHANGELOG
entry. Lowering it requires an ADR (we do not retroactively re-support older
toolchains without explicit reason). Phase 6 may re-evaluate the policy and
adopt a stable-channel-tracks-current-stable-minus-N rule at that point.

## 8. Structured denial context (NORMATIVE; ADR-0023)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DenialReason {
    RedirectNotInAllowlist,
    InsecureScheme,
    HostInBlockList,
    SizeCapExceeded,
    SchemaDrift,
    CapabilityNotGranted,
    RateLimitWindow,
    SsrfPrivateAddress,
    ContentTypeMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DenialContext {
    pub reason:    DenialReason,
    pub source:    Option<String>,
    pub attempted: Option<String>,
    pub expected:  Option<Vec<String>>,
    pub hop_index: Option<u8>,
    pub cap:       Option<u64>,
    pub actual:    Option<u64>,
}

impl From<&crate::http::HttpError> for Option<DenialContext> { /* … */ }
impl From<&crate::source::FetchError> for Option<DenialContext> { /* … */ }
```

The `DenialReason` enum is **closed**: adding a variant is a minor semver
bump, renaming or repurposing one is breaking. The `DenialContext` struct is
**not** `#[non_exhaustive]` because `deny_unknown_fields` already prevents
forward-compatible field additions on the wire — adding a field is a
breaking change. See [`ERRORS.md`](ERRORS.md) §3.1, §5.1 for the runtime
surface and [`MCP_TOOLS.md`](MCP_TOOLS.md) §5 for the JSON envelope.

## 9. Audit-identity: `CanonicalRef` (NORMATIVE; ADR-0021, ADR-0024)

Slice 4 ships the four-tuple audit identity reserved by
[ADR-0021](DECISIONS/0021-canonical-tuple-identity.md) and implemented per
[ADR-0024](DECISIONS/0024-canonical-ref-impl.md). The re-exports are listed
in §1 (`CanonicalRef`, `SourceType`).

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum SourceType {
    Doi,
    Arxiv,
    // future: Pmid, Handle, ...
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct CanonicalRef {
    pub source_type: SourceType,
    pub source_id: String,
    pub resolver_profile: String, // e.g. "crossref", "unpaywall", "arxiv", "oa-publisher"
    pub version: Option<String>,  // e.g. arXiv "v2"; None encodes the empty trailing input
}

impl CanonicalRef {
    pub fn new(
        source_type: SourceType,
        source_id: impl Into<String>,
        resolver_profile: impl Into<String>,
        version: Option<String>,
    ) -> Self;
    pub fn digest(&self) -> [u8; 32];
    pub fn digest_hex(&self) -> String;
}

impl Ref {
    /// Promote a `Ref` to a `CanonicalRef` with the given resolver
    /// profile and optional version (ADR-0021 §1).
    pub fn promote(&self, resolver_profile: &str, version: Option<&str>) -> CanonicalRef;
}
```

The digest algorithm is the NORMATIVE
`SHA256(source_type | 0x00 | source_id | 0x00 | resolver_profile | 0x00 | version_or_empty)`
shape — `version_or_empty` is the empty byte sequence when `version` is
`None`, NOT a sentinel.

The companion provenance-log row schema bump (v1 → v2) is documented in
[`PROVENANCE_LOG.md`](PROVENANCE_LOG.md) §3 + §3.1. The one-shot
migration ships as `doiget provenance migrate [--dry-run]`.
