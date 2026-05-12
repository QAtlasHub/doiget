# 0023 - Structured `denial_context` on error envelopes

- **Date:** 2026-05-12
- **Status:** Proposed
- **Supersedes:** -
- **Source:** Discussion #12 (musaabhasan, 2026-05-09)

## Context

The external review on Discussion #12 (musaabhasan, 2026-05-09, comment 2)
asks doiget to

> make denial reasons explicit so LLM workflows can recover safely instead of
> retrying blindly.

Today the public error envelope (NORMATIVE in [`ERRORS.md`](../ERRORS.md) §3
and [`MCP_TOOLS.md`](../MCP_TOOLS.md) §5) is

```jsonc
{ "ok": false, "error": { "code": "NETWORK_ERROR", "message": "..." } }
```

The `code` is one of the closed [`ErrorCode`](../ERRORS.md) variants, and the
`message` is a free-form human-readable string. ADR-0012 fixes the closed
code set and ADR-0014 explains why the docs are NORMATIVE.

The closed `ErrorCode` enum is correctly coarse-grained — it is sized for
*reaction* by humans and agents, not for *diagnosis*. But several of the most
operationally important denial cases (a redirect blocked by the allowlist, a
body that exceeded the size cap, a capability that was not granted) are
*concrete* events whose parameters an LLM agent could use to plan a recovery:
"the redirect to `evil.example.com` was denied because it is not on the
crossref allowlist (which contains `api.crossref.org`, `*.crossref.org`) →
the right next step is to ask the user whether to add a new ADR adding that
host, NOT to retry blindly."

Today an agent has to text-mine `error.message` to recover that information,
which is fragile (string formats can change without a major version bump) and
incomplete (some context is not in the message at all).

## Decision

### 1. Add a structured `denial_context` field to the error envelope

The error envelope becomes

```jsonc
{
  "ok": false,
  "error": {
    "code":    "NETWORK_ERROR",
    "message": "redirect target evil.example.com not in allowlist for source crossref",
    "denial_context": {
      "reason":    "redirect_not_in_allowlist",
      "source":    "crossref",
      "attempted": "evil.example.com",
      "expected":  ["api.crossref.org", "*.crossref.org"],
      "hop_index": 1
    }
  }
}
```

The `denial_context` field is **optional**: every previously-shipped
`{code, message}` envelope remains valid, and the agent / CLI consumer is free
to ignore the new field. When present, it is structured and machine-parseable.

### 2. `DenialReason` is a closed enum

Per the user's 2026-05-12 directive (Discussion #12 incorporation review),
`reason` is a **closed** enum. The Phase-1 set is:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DenialReason {
    /// Redirect target host did not match the source's allowlist.
    RedirectNotInAllowlist,
    /// Redirect target had a non-HTTPS scheme.
    InsecureScheme,
    /// Source produced a URL whose host is on a blocklist (future use).
    HostInBlockList,
    /// Body exceeded `PDF_MAX_BYTES`.
    SizeCapExceeded,
    /// Store entry's `schema_version` is ahead of this binary.
    SchemaDrift,
    /// Source not in `CapabilityProfile`.
    CapabilityNotGranted,
    /// Rate limiter rejected the call inside the current window.
    RateLimitWindow,
    /// SSRF guard rejected a private / link-local / cloud-metadata address.
    SsrfPrivateAddress,
    /// Response Content-Type / magic-byte mismatch.
    ContentTypeMismatch,
}
```

Adding a new variant is a minor semver bump (additive); renaming or
repurposing one is a breaking change, mirroring the `ErrorCode` rule from
ADR-0012.

### 3. `DenialContext` field shape

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DenialContext {
    pub reason:    DenialReason,
    pub source:    Option<String>,   // resolver source key (e.g. "crossref")
    pub attempted: Option<String>,   // host, path, or other concrete value
    pub expected:  Vec<String>,      // allowlist entries / acceptable values
    pub hop_index: Option<u8>,       // redirect chain position, 0-indexed
    pub cap:       Option<u64>,      // size or rate cap value
    pub actual:    Option<u64>,      // observed value (e.g. response bytes)
}
```

Field-shape rules (NORMATIVE):

- All fields except `reason` are optional. Producers populate the fields
  relevant to the reason and leave the rest at default (`None` / empty
  `Vec`). Consumers MUST tolerate any subset of fields being present.
- `expected` is `Vec<String>` even when only one value is meaningful, to
  avoid format ambiguity for cases where multiple values are acceptable
  (allowlist with several patterns).
- `hop_index` is `u8` because the redirect chain is hard-capped at 10 in
  [`http.rs`](../../crates/doiget-core/src/http.rs) (`MAX_REDIRECTS`) and
  any larger value indicates a bug.

### 4. Mapping table (NORMATIVE)

The producer-side mapping from internal error variants to `DenialContext` is:

| Internal source                                  | `reason`                  | Populated fields                                    |
|--------------------------------------------------|---------------------------|-----------------------------------------------------|
| `HttpError::RedirectDenied { source_key, host }` | `redirect_not_in_allowlist` | `source=source_key`, `attempted=host`, `expected=allowlist hosts`, `hop_index` if known |
| `HttpError::OversizedBody { actual, cap }`       | `size_cap_exceeded`       | `cap`, `actual`                                     |
| `HttpError::NotAPdf { got }`                     | `content_type_mismatch`   | `attempted=hex(got)`, `expected=["%PDF-"]`          |
| `HttpError::InsecureRedirect { scheme }`         | `insecure_scheme`         | `attempted=scheme:...`, `expected=["https"]`        |
| `FetchError::NotEligible { source_key }`         | `capability_not_granted`  | `source=source_key`                                 |
| `RateLimiter` denial (future)                    | `rate_limit_window`       | `source`, `cap=per-source rate`                     |
| Store schema rejection                           | `schema_drift`            | `actual=row schema_version`, `cap=binary version`   |
| SSRF guard (future)                              | `ssrf_private_address`    | `attempted=ip:port`                                 |

The mapping is implemented as a `From<HttpError> for Option<DenialContext>`
plus `From<FetchError> for Option<DenialContext>`, sitting next to the
existing `From<FetchError> for ErrorCode` collapse in
[`source.rs`](../../crates/doiget-core/src/source.rs).

### 5. `error.message` is still required

`message` MUST continue to embed the concrete parameters in human-readable
form (per the existing [`ERRORS.md`](../ERRORS.md) §6 "no silent failures"
posture). `denial_context` is a *parallel* machine-parseable channel for the
same facts. CLI text consumers and existing log scrapers continue to work
unchanged; new agent-side consumers can prefer `denial_context` when present.

## Consequences

**Positive.**
- Agents recover from denials with structured data instead of regex over
  free-form messages.
- The closed `DenialReason` enum is review-able: a reviewer can assert "every
  call site that returns `RedirectDenied` populates `attempted` and
  `expected`" via a single grep.
- Backwards compatible: the field is optional and additive.

**Negative.**
- One more field to keep populated at every error producer site. Mitigated by
  the `From` impls in §4 — most call sites become one-liner conversions.
- The closed enum requires an ADR + minor version bump to add a new reason.
  Acceptable: the same constraint applies to `ErrorCode` (ADR-0012) and we
  consider it a feature, not a bug.

**Out of scope.**
- Open-ended `denial_context` (rejected per the user's "closed" directive).
- A `denial_context` envelope on success results (rejected — this is purely a
  failure-mode aid).

To revise this decision, write a new ADR with Status: Accepted and Supersedes:
0023, and update this file's Status to Superseded by NNNN per CONTRIBUTING.md.
