# Error taxonomy

> **Status: NORMATIVE.** Defines the closed set of error codes that doiget surfaces and
> how each persona experiences each code. Adding a new error code is a minor semver
> bump; renaming or repurposing one is a breaking change.

## 1. ErrorCode enum

```rust
// Phase 1+ target module path; Phase 0 ships ErrorCode in monolithic lib.rs.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    InvalidRef,
    NoOaAvailable,
    RateLimited,
    NetworkError,
    StoreError,
    LogError,
    CapabilityDenied,
    FetchTimeout,
    SchemaTooNew,
    LockTimeout,
    InternalError,
}
```

Wire form (JSON / MCP): `"INVALID_REF"`, `"NO_OA_AVAILABLE"`, etc.

## 2. Code semantics

| Code | Meaning | Recoverable? |
|---|---|---|
| `INVALID_REF` | DOI / arXiv id failed validation. | No (user must correct input). |
| `NO_OA_AVAILABLE` | Tier 1 sources reported no OA URL. | Try later, or enable opt-in source. |
| `RATE_LIMITED` | Internal rate cap hit, OR 429 from source. | Retry after `Retry-After` (or 1 s). |
| `NETWORK_ERROR` | Transport / DNS / TLS failure. | Retry usually fine. |
| `STORE_ERROR` | Filesystem write failed (disk, permission, etc.). | Depends on cause. |
| `LOG_ERROR` | Provenance log write failed. **Fetch is aborted.** | Free disk / fix perms. |
| `CAPABILITY_DENIED` | Source not in `CapabilityProfile`. | User opts in, or pick different source. |
| `FETCH_TIMEOUT` | Per-request timeout exceeded. | Retry. |
| `SCHEMA_TOO_NEW` | Store entry's `schema_version` is ahead. | Upgrade doiget. |
| `LOCK_TIMEOUT` | Could not acquire `flock` within 5 s. | Retry; another process holds it. |
| `INTERNAL_ERROR` | Bug. | Report at <https://github.com/sotashimozono/doiget/issues>. |

## 3. Persona × error matrix

| Persona | Surface |
|---|---|
| Agent (MCP) | Structured `{ ok: false, error: { code, message, denial_context? } }`. Never throws. |
| Researcher (CLI human) | `cargo`-style stderr: `error[E0007]: rate limited from unpaywall: retry after 1s`. Exit code 1. |
| CI / Batch (CLI `--json`) | JSON Lines record per ref with `{"ok":false, "error":{"code":"...","message":"...","denial_context":{...}?}}`. Exit code = number of failures (capped at 255). |
| Library (Rust) | `Err(FetchError)` (typed via `thiserror`). |

### 3.1 Structured `denial_context` (NORMATIVE; ADR-0023)

The `error` envelope MAY carry an additional structured `denial_context`
field for machine-readable recovery. The field is **optional and additive** —
consumers MUST tolerate both its presence and its absence — and is
populated by the producer on the denial classes named in the §5 mapping
table below.

`denial_context.reason` is a **closed** enum (per ADR-0023):

```jsonc
"denial_context": {
  "reason":    "redirect_not_in_allowlist",   // closed enum, snake_case
  "source":    "crossref",                     // resolver source key, optional
  "attempted": "evil.example.com",             // host/path/value, optional
  "expected":  ["api.crossref.org",
                "*.crossref.org"],             // allowlist entries, [] when N/A
  "hop_index": 1,                              // redirect-chain position, optional
  "cap":       104857600,                      // size/rate cap, optional
  "actual":    209715200                       // observed value, optional
}
```

Closed `reason` set: `redirect_not_in_allowlist`, `insecure_scheme`,
`host_in_block_list`, `size_cap_exceeded`, `schema_drift`,
`capability_not_granted`, `rate_limit_window`, `ssrf_private_address`,
`content_type_mismatch`. Adding a new variant is a minor semver bump;
renaming or repurposing one is a breaking change.

`error.message` MUST continue to embed the same parameters in human-readable
form — `denial_context` is a parallel channel, not a replacement.

## 4. CLI exit codes

| Exit | Meaning |
|---|---|
| `0` | Success (all refs ok). |
| `1` | At least one fetch failed. |
| `2` | Misuse (bad arguments, missing config). |
| `3` | Capability denied (no source could serve). |
| `4` | I/O failure (store / log unwritable). |
| `64..=78` | `sysexits.h` mapping for select cases. |
| `124` | Timeout (matches GNU `timeout`). |
| `255` | Capped failure count for `batch`. |

## 5. Error wrapping

- `doiget-core` exports `FetchError` (typed, `thiserror`). Each variant carries an
  `ErrorCode` and any context data needed by callers.
- `doiget-cli` uses `anyhow::Error` for context, mapping the leaf `FetchError` to a CLI
  presentation per persona.
- `doiget-mcp` translates `FetchError` to the MCP `{ok: false, error}` shape and never
  throws across the JSON-RPC boundary.

### 5.1 `DenialContext` mapping (ADR-0023 §4)

The producer-side mapping from internal error variants to `DenialContext` is
defined in [`DECISIONS/0023-denial-context-structured.md`](DECISIONS/0023-denial-context-structured.md)
§4 (NORMATIVE table). Summary: every `HttpError::RedirectDenied`,
`OversizedBody`, `NotAPdf`, `InsecureRedirect`, and every
`FetchError::NotEligible` produces a populated `Option<DenialContext>` via
`From` impls in `doiget-core`. Other error variants leave `denial_context`
unset.

## 6. No silent failures

doiget MUST NOT return a "success" result with placeholder data when a real fetch
failed. A fetch either succeeds with a real PDF + license + metadata, or returns an
error with one of the codes above.
