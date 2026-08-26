+++
title = "Capability profile"
description = "```rust"
weight = 110
+++

# CapabilityProfile

> **Status: NORMATIVE.** Defines the runtime capability gate that authorizes which
> sources may be invoked. Every `Source::fetch` implementation MUST require a
> `&CapabilityProfile` argument; sources whose capability is not granted at startup
> cannot be invoked at the type level.

## 1. Type definition

```rust
// These types are defined in the doiget-core crate.

use secrecy::SecretString; // = SecretBox<str>; the `secrecy` 0.10 owned-string secret
use chrono::{DateTime, Utc};

// All structs below are #[non_exhaustive] in the Rust source. External crates
// cannot construct them via struct-literal syntax — go through
// `CapabilityProfile::from_env()` (see §2). `TdmGrant::api_key` exists only
// when at least one `tdm-*` Cargo feature is compiled in (the `secrecy` dep is
// `optional = true` and gated on those features per ADR-0002). The field is
// additive under `#[non_exhaustive]`; default release binaries — which contain
// no TDM code at all — do not carry it.

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CapabilityProfile {
    pub oa: AlwaysOn,
    pub metadata: MetadataAccess,
    pub tdm_elsevier: Option<TdmGrant>,
    pub tdm_aps: Option<TdmGrant>,
    pub tdm_springer: Option<TdmGrant>,
    pub tdm_ieee: Option<TdmGrant>,
    pub rate_limits: RateLimits,
}

#[derive(Debug, Clone, Copy)]
pub struct AlwaysOn;   // unit struct — Tier 1 OA is always permitted

#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct MetadataAccess {
    pub openalex: bool,
    pub semantic_scholar: bool,
    pub doaj: bool,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TdmGrant {
    // Present only under a `tdm-*` feature (see the note above). `secrecy`
    // 0.10 replaced `Secret<String>` with `SecretString` (= `SecretBox<str>`).
    #[cfg(any(feature = "tdm-elsevier", feature = "tdm-aps",
              feature = "tdm-springer", feature = "tdm-ieee"))]
    pub api_key:       SecretString,
    pub agreed_at:     DateTime<Utc>,
    pub agree_env_var: String,            // e.g. "DOIGET_AGREE_TDM_ELSEVIER"
}

#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct RateLimits {
    pub(crate) max_concurrent_fetches: u32, // hard-coded 5 (LEGAL §6 safeguard 8)
    pub(crate) max_fetches_per_second: f32, // hard-coded 5.0
    pub(crate) per_source_backoff_ms:  u64, // hard-coded 200
}

impl RateLimits {
    /// Sole public constructor. There is no other way to obtain a
    /// `RateLimits` outside of `doiget-core`: fields are `pub(crate)`, the
    /// struct is `#[non_exhaustive]`, and no public `new`-style function
    /// exists. This closes the legal-safeguard loophole that bare `pub`
    /// fields would create (cf. `docs/LEGAL.md` §6 safeguard 8).
    pub const HARD_CODED: Self = Self {
        max_concurrent_fetches: 5,
        max_fetches_per_second: 5.0,
        per_source_backoff_ms:  200,
    };

    pub const fn max_concurrent_fetches(&self) -> u32 { self.max_concurrent_fetches }
    pub const fn max_fetches_per_second(&self) -> f32 { self.max_fetches_per_second }
    pub const fn per_source_backoff_ms(&self)  -> u64 { self.per_source_backoff_ms }
}
```

### External construction

External crates always go through:

```rust
let profile = CapabilityProfile::from_env()?;
```

Struct-literal construction (`CapabilityProfile { oa: ..., ... }`) is blocked
outside `doiget-core` by `#[non_exhaustive]`. Tests inside `doiget-core` may
still construct profiles directly for fixture purposes.

`api_key` is wrapped in `secrecy::SecretString` (the `secrecy` 0.10 replacement
for the 0.9 `Secret<String>`) so that `Debug` prints a redaction placeholder
rather than the key. Logs additionally use a redactor for known sensitive field
names, and any URL that carries a key as a query parameter (Springer Nature —
see §3) is passed through `redact_api_key_in_url` before it is logged or
recorded in provenance.

## 2. Resolution algorithm

```rust
impl CapabilityProfile {
    pub fn from_env() -> Result<Self, CapabilityError> {
        Ok(Self {
            oa: AlwaysOn,
            metadata: MetadataAccess {
                openalex:        env::var("DOIGET_ENABLE_OPENALEX").is_ok(),
                semantic_scholar: env::var("DOIGET_ENABLE_S2").is_ok(),
                doaj:            env::var("DOIGET_ENABLE_DOAJ").is_ok(),
                datacite:        env::var("DOIGET_ENABLE_DATACITE").is_ok(),
                hal:             env::var("DOIGET_ENABLE_HAL").is_ok(),
                openaire:        env::var("DOIGET_ENABLE_OPENAIRE").is_ok(),
                core:            env::var("DOIGET_ENABLE_CORE").is_ok(),
                europe_pmc:      env::var("DOIGET_ENABLE_EUROPE_PMC").is_ok(),
            },
            tdm_elsevier: read_tdm_grant("DOIGET_AGREE_TDM_ELSEVIER", "DOIGET_KEY_ELSEVIER")?,
            tdm_aps:      read_tdm_grant("DOIGET_AGREE_TDM_APS",      "DOIGET_KEY_APS")?,
            tdm_springer: read_tdm_grant("DOIGET_AGREE_TDM_SPRINGER", "DOIGET_KEY_SPRINGER")?,
            tdm_ieee:     read_tdm_grant("DOIGET_AGREE_TDM_IEEE",     "DOIGET_KEY_IEEE")?,
            rate_limits:  RateLimits::HARD_CODED,
        })
    }
}

// `file_key` is `[tdm.<publisher>] api_key` from credentials.toml, one rung
// BELOW the env var (#509, ADR-0050). The agreement has no file rung.
fn read_tdm_grant(agree_var: &str, key_var: &str, file_key: Option<&str>)
    -> Result<Option<TdmGrant>, CapabilityError>
{
    let agreed = matches!(env::var(agree_var).as_deref(), Ok("1"));
    let key    = env::var(key_var).ok().or_else(|| file_key.map(str::to_string));
    match (agreed, key) {
        (true, Some(k)) => Ok(Some(TdmGrant {
            // `secrecy` 0.10: `SecretString::from(String)` replaces the
            // 0.9 `Secret::new(k)`. Field present only under a `tdm-*`
            // feature; the actual source splits this into a small
            // `build_tdm_grant` helper so the cfg lives in one place.
            api_key:       SecretString::from(k),
            agreed_at:     Utc::now(),
            agree_env_var: agree_var.to_string(),
        })),
        (true, None)    => Err(CapabilityError::AgreedButNoKey {
            agree_var: agree_var.into(), key_var: key_var.into(),
        }),
        (false, Some(_)) => Err(CapabilityError::KeyButNotAgreed {
            agree_var: agree_var.into(),
        }),
        (false, None)   => Ok(None),
    }
}
```

### Three resolution rules

The **key** may come from `DOIGET_KEY_<PUBLISHER>` or from
`[tdm.<publisher>] api_key` in `credentials.toml`, env first
([`CONFIG.md`](CONFIG.md) §6). The **agreement** has no file rung: it is
`DOIGET_AGREE_TDM_<PUBLISHER>=1` in the environment or it does not exist, so
that it is an act taken in the session that runs the fetch rather than a
boolean written once and forgotten ([`LEGAL.md`](LEGAL.md) §6a.2, ADR-0050).
The three rules below are unchanged by that — a key from the file still needs
the agreement, so rule 3 now also fires for a file-supplied key.

1. **`agree=1` + key present** → `Some(TdmGrant)`. The source is enabled this session.
2. **`agree=1` but key missing** → `Err(AgreedButNoKey)`. Startup fails; user has agreed
   but provided no credential. Silent skip would mask a misconfiguration.
3. **`agree` unset but key present** → `Err(KeyButNotAgreed)`. Startup fails; we require
   the explicit agreement env var even when the key is set. Otherwise a leaked
   `DOIGET_KEY_ELSEVIER` from a parent shell environment could enable a source the user
   did not intend.

## 3. Environment variable reference

| Variable | Type | Effect |
|---|---|---|
> **The `metadata` Cargo feature vs. these variables (ADR-0040, NORMATIVE).** The
> feature decides what is *compiled*; the variables below decide what is *reachable*.
> As of 0.8.8 `metadata` gates the whole optional non-Tier-1 source surface —
> enrichment, resolution and retrieval — and release binaries compile it in. A source
> is inert until its own `DOIGET_ENABLE_<NAME>` is set, so with all of them unset the
> binary behaves exactly as a Tier-1-only build.

| `DOIGET_ENABLE_OPENALEX` | presence | Enables OpenAlex (metadata only). |
| `DOIGET_ENABLE_S2` | presence | Enables Semantic Scholar. |
| `DOIGET_ENABLE_DOAJ` | presence | Enables DOAJ. |
| `DOIGET_ENABLE_DATACITE` | presence | Enables DataCite DOI **resolution** (Zenodo / figshare / Dryad / OSF). Unlike its siblings this is not enrichment: without it those DOIs report `NOT_FOUND` even when the record is live and open (#414). |
| `DOIGET_ENABLE_HAL` | presence | Enables HAL, the French national OA repository. OA deposits only: a record whose `openAccess_bool` is not `true` is rejected rather than returned (#418). |
| `DOIGET_ENABLE_OPENAIRE` | presence | Enables OpenAIRE (European repository aggregation, Graph API v1). Mixed access rights: only a COAR `c_abf2` (OPEN) `bestAccessRight` is accepted; EMBARGO / RESTRICTED / CLOSED / absent are refused (#416). |
| `DOIGET_ENABLE_CORE` | presence | Enables CORE, the broadest cross-repository OA index and therefore the last fallback in the chain (#417). |
| `DOIGET_CORE_API_KEY` | value | **Optional.** Your own free CORE key, raising the rate limit. Absent (or blank) degrades to the key-less limit rather than failing. Never bundled; sent as a bearer header and never logged. A 401/403 with a key set is reported as a transport error, distinct from "not found", so a bad key does not look like a missing paper. |
| `DOIGET_ENABLE_EUROPE_PMC` | presence | Enables Europe PMC (biomedical OA full text Unpaywall does not index). Gated on the record advertising a retrievable PDF — a `fullTextUrlList` entry with `documentStyle = pdf` and `availabilityCode` `F` or `OA` (#503) — **not** on `inEPMC` (presence is not openness, #415) and **not** on `isOpenAccess` (bulk-subset membership, which is reported rather than a veto). |
| `DOIGET_AGREE_TDM_ELSEVIER` | `=1` | Acknowledges Elsevier TDM ToS. Pairs with key. |
| `DOIGET_KEY_ELSEVIER` | secret string | Elsevier API key. Read into `Secret<String>`. |
| `DOIGET_AGREE_TDM_APS` | `=1` | Acknowledges APS Harvest TDM ToS. |
| `DOIGET_KEY_APS` | secret string | APS API key. |
| `DOIGET_AGREE_TDM_SPRINGER` | `=1` | Acknowledges Springer Nature OA ToS. |
| `DOIGET_KEY_SPRINGER` | secret string | Springer API key. |
| `DOIGET_AGREE_TDM_IEEE` | `=1` | Acknowledges IEEE TDM ToS. |
| `DOIGET_KEY_IEEE` | secret string | IEEE Xplore API key. |

> **Note (ADR-0031):** `DOIGET_ENABLE_OPENALEX` gates the OpenAlex
> *enrichment* / citation-graph source only (`/works/doi:…`,
> `referenced_works[]`). The `doiget search` **discovery** path
> (`/works?search=`, `doiget_core::discovery`) is classified as Tier-1
> OA metadata, **always-on** — it needs no env var and ships in the
> default `oa-only` binary.

> **Note (ADR-0032):** full-text extraction (`doiget text`,
> `doiget_core::paper_text`) over **ar5iv** is likewise classified Tier-1
> OA metadata, **always-on** — no env var, ships in the `oa-only` binary.
> It reads the ar5iv HTML rendering, never the PDF blob (permanent
> non-goal #1 / ADR-0003 stays intact, narrowed by ADR-0032 to PDF-blob
> processing).

Setting `DOIGET_AGREE_TDM_*` only makes the relevant source eligible. The corresponding
TDM-specific Cargo feature must also have been compiled in (`cargo build --features
tdm-elsevier` etc.). Default release binaries do not contain TDM source code at all.

## 4. Source trait integration

```rust
pub trait Source: Send + Sync {
    fn name(&self) -> &str;
    fn can_serve(&self, profile: &CapabilityProfile, ref_: &Ref) -> bool;
    async fn fetch(&self, ref_: &Ref, profile: &CapabilityProfile, ctx: &FetchContext)
        -> Result<FetchResult, FetchError>;
}

// Example: Elsevier TDM source (only compiled when feature = "tdm-elsevier")
#[cfg(feature = "tdm-elsevier")]
impl Source for ElsevierTdm {
    fn can_serve(&self, p: &CapabilityProfile, _: &Ref) -> bool {
        p.tdm_elsevier.is_some()
    }
    async fn fetch(&self, ref_: &Ref, profile: &CapabilityProfile, ctx: &FetchContext)
        -> Result<FetchResult, FetchError>
    {
        let grant = profile.tdm_elsevier.as_ref()
            .ok_or(FetchError::CapabilityDenied)?;
        // grant.api_key.expose_secret() — use the API key
        // ...
    }
}
```

## 5. Startup banner (auditability)

On startup, `doiget` (CLI or MCP server) writes a single line to **stderr** describing
the resolved profile. Example:

```
[doiget] capability: oa=on metadata=[openalex] tdm=[elsevier(agreed=2026-05-05T08:00:00Z)]
```

The banner is on stderr in all modes, including MCP mode (where stdout is reserved for
JSON-RPC). The banner does not include any portion of any API key.

## 6. Reload semantics

`CapabilityProfile` is **immutable for the lifetime of the process**. A change to
`DOIGET_AGREE_TDM_*` or a key environment variable while the process is running has no
effect; the user must restart. This avoids partial-state security weakening.

## 7. MCP tool exposure

The MCP tool `doiget_capability_profile` (see [`MCP_TOOLS.md`](MCP_TOOLS.md)) reports the
current profile to agents in a redacted form (no API keys, just booleans / source names
and `agreed_at` timestamps). Agents can use this to decide whether a `fetch_paper(...)`
call against a TDM source will succeed before issuing it.
