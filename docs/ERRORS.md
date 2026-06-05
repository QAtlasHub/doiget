# Error taxonomy

> **Status: NORMATIVE.** Defines the closed set of error codes that doiget surfaces and
> how each persona experiences each code. Adding a new error code is a minor semver
> bump; renaming or repurposing one is a breaking change.

## 1. ErrorCode enum

```rust
// ErrorCode is defined in the doiget-core crate.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    InvalidRef,
    NoOaAvailable,
    RateLimited,
    NetworkError,
    NotFound,
    Ambiguous,
    StoreError,
    LogError,
    CapabilityDenied,
    FetchTimeout,
    SchemaTooNew,
    LockTimeout,
    InternalError,
    NotImplemented,
}
```

Wire form (JSON / MCP): `"INVALID_REF"`, `"NO_OA_AVAILABLE"`, etc.

## 2. Code semantics

| Code | Meaning | Recoverable? |
|---|---|---|
| `INVALID_REF` | DOI / arXiv id failed validation. | No (user must correct input). |
| `NO_OA_AVAILABLE` | Tier 1 sources reported no OA URL. | Try later, or enable opt-in source. |
| `RATE_LIMITED` | Internal rate cap hit, OR 429 from source. | Retry after `Retry-After` (or 1 s). |
| `NETWORK_ERROR` | Transport / DNS / TLS failure. **Does NOT cover a deliberate supply-chain policy block** — see §6.1: an off-allowlist / redirect-denied / insecure-scheme OA-PDF leg is `CAPABILITY_DENIED`, not `NETWORK_ERROR`. | Retry usually fine. |
| `NOT_FOUND` | Metadata source authoritatively reported the id does not exist: HTTP `404` / `410` / `451`, or a source-specific absence (arXiv returns HTTP 200 with an empty `<feed>` for an unknown id). Network-independent and reproducible — distinct from the transient `NETWORK_ERROR` / `RATE_LIMITED`. `doiget verify` treats it as a definite dead reference (`absent`). For a DOI it is emitted only when all configured sources (Crossref, Unpaywall) fail to resolve it; a DataCite-only DOI may thus be reported `NOT_FOUND`. | No (the id is wrong or retracted). |
| `AMBIGUOUS` | A name filter (`--author` / `--venue` / `--publisher`) matched several entities with no clear winner; the error lists the candidates. Distinct from `NOT_FOUND` ("matched nothing"). CLI exit `2`. | Yes — narrow the name (add a first name / fuller title) or pass an exact id. |
| `STORE_ERROR` | Filesystem write failed (disk, permission, etc.). | Depends on cause. |
| `LOG_ERROR` | Provenance log write failed. **Fetch is aborted.** | Free disk / fix perms. |
| `CAPABILITY_DENIED` | Source not in `CapabilityProfile`. | User opts in, or pick different source. |
| `FETCH_TIMEOUT` | Per-request timeout exceeded. | Retry. |
| `SCHEMA_TOO_NEW` | Store entry's `schema_version` is ahead. | Upgrade doiget. |
| `LOCK_TIMEOUT` | Could not acquire `flock` within 5 s. | Retry; another process holds it. |
| `INTERNAL_ERROR` | Bug. | Report at <https://github.com/sotashimozono/doiget/issues>. |
| `NOT_IMPLEMENTED` | Feature is spec'd but not yet wired in this Phase. | Wait for next minor release; do not retry. |

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
                "*.crossref.org"],             // allowlist entries; the field
                                                //   is ABSENT when the producer
                                                //   did not populate it. An
                                                //   explicit [] means "empty
                                                //   allowlist" (ADR-0023 §3
                                                //   None/Some(vec![])
                                                //   disambiguation).
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

### 6.1 Off-allowlist / redirect-denied OA-PDF leg → `CAPABILITY_DENIED` / exit 3 (NORMATIVE; issue #145)

When a DOI fetch discovers an OA PDF URL but the OA-PDF leg is **blocked
by supply-chain redirect policy** — the host is off the `oa-publisher`
allowlist (`redirect_not_in_allowlist`), a redirect hop is non-HTTPS
(`insecure_scheme`), or the host is on the block list
(`host_in_block_list`) — the metadata is still written, but the leg is a
**deliberate policy denial, not a transport failure**.

Internally `doiget-core` collapses every `FetchError::Http(_)` (including
a redirect denial that `reqwest` re-wraps as `HttpError::Network`) to
`NetworkError`, and the provenance-log row for the failed `oa-publisher`
leg is therefore written with `error_code = NETWORK_ERROR` (the
transport-layer truth — unchanged). However, surfacing this to the user
as `NETWORK_ERROR` would be **wrong**: §2 defines `NETWORK_ERROR` as
"retry usually fine", whereas retrying a policy block never helps.

NORMATIVE rule: the CLI MUST reclassify such a blocked OA-PDF leg using
the preserved `denial_context.reason` (§3.1) and surface it as:

- error code **`CAPABILITY_DENIED`** (rendered `error[CAPABILITY_DENIED]:`,
  with the closed-set `denial_context.reason` named inline so the block is
  unambiguously a policy denial, not a flaky network);
- process **exit code `3`** (§4 "Capability denied"), the same exit code
  `fetch` / `graph` use for every other `ErrorCode::CapabilityDenied`.

The provenance row keeps `error_code = NETWORK_ERROR` (it records the
transport mechanism); the *user-facing* code/exit is `CAPABILITY_DENIED` /
`3`. Non-policy OA-PDF blocks (genuine transport fault with no
`denial_context`, or a non-policy reason such as `size_cap_exceeded` /
`content_type_mismatch`) remain `NETWORK_ERROR` / exit 1.

Covered end-to-end (closed by #163; originally tracked under #145): the
`oa-publisher` host allowlist is no longer enforced *only* inside the
redirect-policy closure. PR #163 added a **pre-fetch host allowlist
check** on the metadata-discovered OA URL in
`doiget_core::orchestrator::try_fetch_oa_pdf`
(`docs/REDIRECT_ALLOWLIST.md` §1 — NORMATIVE), applied **before** the PDF
fetch is issued, not only on redirect hops. An OA URL whose *initial*
host is off-allowlist with **no redirect** is therefore rejected by the
pre-check with the **same** `HttpError::RedirectDenied` value the redirect
closure produces (same `source_key` / lowercased `host` /
`expected_hosts`), so it still carries a policy `denial_context`
(`redirect_not_in_allowlist`). The CLI reclassification rule above then
promotes it to `CAPABILITY_DENIED` / exit 3 — the off-allowlist OA URL is
**not** fetched and never reaches connect. The reclassification rule now
covers every supply-chain policy block uniformly: pre-fetch off-allowlist
OA URLs, real redirect denials, insecure-scheme hops, and host-blocklist
hits. Only a genuine transport fault with no `denial_context` (or a
non-policy reason such as `size_cap_exceeded` / `content_type_mismatch`)
remains `NETWORK_ERROR` / exit 1, consistent with §2.
