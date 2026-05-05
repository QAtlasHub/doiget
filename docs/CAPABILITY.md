# CapabilityProfile

> **Status: NORMATIVE.** Defines the runtime capability gate that authorizes which
> sources may be invoked. Every `Source::fetch` implementation MUST require a
> `&CapabilityProfile` argument; sources whose capability is not granted at startup
> cannot be invoked at the type level.

## 1. Type definition

```rust
// crates/doiget-core/src/capability.rs

use secrecy::Secret;
use chrono::{DateTime, Utc};

#[derive(Debug, Clone)]
pub struct CapabilityProfile {
    pub oa: AlwaysOn,
    pub metadata: MetadataAccess,
    pub tdm_elsevier: Option<TdmGrant>,
    pub tdm_aps: Option<TdmGrant>,
    pub tdm_springer: Option<TdmGrant>,
    pub rate_limits: RateLimits,
}

#[derive(Debug, Clone, Copy)]
pub struct AlwaysOn;   // unit struct — Tier 1 OA is always permitted

#[derive(Debug, Clone)]
pub struct MetadataAccess {
    pub openalex: bool,            // Phase 4+
    pub semantic_scholar: bool,
    pub doaj: bool,
}

#[derive(Debug, Clone)]
pub struct TdmGrant {
    pub api_key:       Secret<String>,
    pub agreed_at:     DateTime<Utc>,
    pub agree_env_var: String,        // e.g. "DOIGET_AGREE_TDM_ELSEVIER"
}

#[derive(Debug, Clone, Copy)]
pub struct RateLimits {
    pub max_concurrent_fetches:  u32, // hard-coded 5 (LEGAL §6 safeguard 8)
    pub max_fetches_per_second:  f32, // hard-coded 5.0
    pub per_source_backoff_ms:   u64, // hard-coded 200
}

impl RateLimits {
    pub const HARD_CODED: Self = Self {
        max_concurrent_fetches: 5,
        max_fetches_per_second: 5.0,
        per_source_backoff_ms:  200,
    };
}
```

`api_key` is wrapped in `secrecy::Secret<String>` so that `Display` and `Debug` print
`****`. Logs use a redactor for known sensitive field names.

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
            },
            tdm_elsevier: read_tdm_grant("DOIGET_AGREE_TDM_ELSEVIER", "DOIGET_KEY_ELSEVIER")?,
            tdm_aps:      read_tdm_grant("DOIGET_AGREE_TDM_APS",      "DOIGET_KEY_APS")?,
            tdm_springer: read_tdm_grant("DOIGET_AGREE_TDM_SPRINGER", "DOIGET_KEY_SPRINGER")?,
            rate_limits:  RateLimits::HARD_CODED,
        })
    }
}

fn read_tdm_grant(agree_var: &str, key_var: &str) -> Result<Option<TdmGrant>, CapabilityError> {
    let agreed = matches!(env::var(agree_var).as_deref(), Ok("1"));
    let key    = env::var(key_var).ok();
    match (agreed, key) {
        (true, Some(k)) => Ok(Some(TdmGrant {
            api_key:       Secret::new(k),
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
| `DOIGET_ENABLE_OPENALEX` | presence | Enables OpenAlex (metadata only). Phase 4+. |
| `DOIGET_ENABLE_S2` | presence | Enables Semantic Scholar. Phase 4+. |
| `DOIGET_ENABLE_DOAJ` | presence | Enables DOAJ. Phase 4+. |
| `DOIGET_AGREE_TDM_ELSEVIER` | `=1` | Acknowledges Elsevier TDM ToS. Pairs with key. |
| `DOIGET_KEY_ELSEVIER` | secret string | Elsevier API key. Read into `Secret<String>`. |
| `DOIGET_AGREE_TDM_APS` | `=1` | Acknowledges APS Harvest TDM ToS. |
| `DOIGET_KEY_APS` | secret string | APS API key. |
| `DOIGET_AGREE_TDM_SPRINGER` | `=1` | Acknowledges Springer Nature OA ToS. |
| `DOIGET_KEY_SPRINGER` | secret string | Springer API key. |

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
