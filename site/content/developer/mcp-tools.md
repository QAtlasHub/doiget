+++
title = "MCP tools"
description = "`doiget` runs as a Model Context Protocol server when invoked as `doiget serve`. It"
weight = 100
+++

# MCP tools

> **Status: NORMATIVE.** Defines the tool surface exposed by `doiget serve` over stdio
> JSON-RPC. Renaming or removing a tool is a breaking change.

`doiget` runs as a Model Context Protocol server when invoked as `doiget serve`. It
speaks **stdio only** ([ADR-0001](DECISIONS/), [`SCOPE.md`](SCOPE.md) §non-goal 6).

## 1. Tool list

| Tool | Purpose |
|---|---|
| `doiget_resolve_paper` | Resolve DOI / arXiv id to authoritative metadata. |
| `doiget_fetch_paper` | Resolve and download a single PDF to the store. Accepts `dry_run`. |
| `doiget_metadata_only` | Resolve to metadata. **Guarantees no PDF / publisher fetch.** Accepts `dry_run`. |
| `doiget_batch_fetch` | Up to 100 refs in one call. Accepts `dry_run`. |
| `doiget_info` | Retrieve a store entry's metadata. |
| `doiget_search_local` | Search store metadata (title / authors / venue). |
| `doiget_paper_search` | External literature discovery over OpenAlex (`/works?search=`); abstract-bearing candidates for triage. Tier-1 OA metadata, always-on; **never fetches a PDF** (ADR-0031). |
| `doiget_paper_text` | Extract an **arXiv** paper's full text from ar5iv as sectioned plain text (`ref`, optional `max_chars`). Tier-1 OA, always-on; **never opens the PDF blob** (ADR-0032). A DOI → `NO_OA_AVAILABLE`. |
| `doiget_link` | Resolve a **DOI** to its arXiv preprint + identity cluster (`{ doi, arxiv, openalex_id, title }`) over OpenAlex, for reading or dedup (#281 item 5). Tier-1 OA, always-on; **never fetches a PDF**. arXiv → DOI is a follow-up; a non-DOI ref → `INVALID_REF`. |
| `doiget_list_recent` | Last N fetched entries. |
| `doiget_paper_pdf_path` | Return the local path of a cached PDF. **Does not read, parse, or transmit content.** |
| `doiget_capability_profile` | Report which sources this instance is allowed to use. |
| `doiget_health` | Operational sanity (store writable, version, schema). `store_writable` is a best-effort probe of the nearest **existing** ancestor of the store root — it creates nothing, so calling this tool never materialises `papers/` (#406). |

Additional tools:

| Tool | Purpose |
|---|---|
| `doiget_expand_citation_graph` | BFS expansion of citations. Hard-capped. |
| `doiget_bibtex_export` | BibTeX for one or many entries. |
| `doiget_csl_export` | CSL JSON for one or many entries. |
| `doiget_resolve_citation` | Resolve a free-form bibliographic citation string to ranked DOI candidates. |
| `doiget_batch_resolve_citations` | Batch resolve bibliographic citation strings to ranked DOI candidates. |

## 2. Naming and convention

- All tools use `snake_case` with the `doiget_` prefix.
- Inputs are validated via JSON Schema declared in the tool's `inputSchema` (per MCP).
- Outputs are structured: `{ ok: true, ... }` or `{ ok: false, error: { code, message } }`.
  Tools never throw across the JSON-RPC boundary.
- Error `code` values are the closed set defined in [`ERRORS.md`](ERRORS.md).

## 3. Tool description format

Each tool's `description` field follows this six-section format so LLM agents can pick
the right tool with minimal mistakes:

```
WHEN TO USE: <one sentence>
INPUTS: <field-by-field>
OUTPUTS: <shape on success>
COSTS: <network / time / quota>
SIDE EFFECTS: <what writes to disk / log / store>
LIMITS: <hard caps>
```

## 4. Example tool spec — `doiget_fetch_paper`

```jsonc
{
  "name": "doiget_fetch_paper",
  "description": "WHEN TO USE: User wants to download a paper PDF given a DOI or arXiv id.\nINPUTS: ref: DOI ('10.1234/abc') or arXiv id ('2401.12345').\nOUTPUTS: { ok: true, ref, source, path, license, size_bytes } or { ok: false, error: { code, message } }.\nCOSTS: 1-3 s network call. May fail if not Open Access.\nSIDE EFFECTS: Writes PDF to the store. Appends a row to the provenance log.\nLIMITS: Max 5 fetches/sec. Use doiget_batch_fetch for >5 refs.",
  "inputSchema": {
    "type": "object",
    "required": ["ref"],
    "properties": {
      "ref": {
        "type": "string",
        "minLength": 7,
        "maxLength": 256,
        "pattern": "^(10\\.\\d{4,9}/[A-Za-z0-9._/()-]+|arXiv:\\d{4}\\.\\d{4,5}|\\d{4}\\.\\d{4,5})$"
      }
    },
    "additionalProperties": false
  }
}
```

## 5. Output shape (NORMATIVE)

```typescript
type FetchResult =
  | { ok: true,
      ref: string,
      source: "crossref" | "unpaywall" | "arxiv"
            | "openalex" | "s2" | "doaj" | "oa-publisher"
            | "tdm-elsevier" | "tdm-aps" | "tdm-springer",
      // ADR-0021 §4 / ADR-0024: the resolver profile under which the
      // canonical-digest for this fetch was minted. Currently equal to
      // `source` verbatim; kept a distinct field so the two can be
      // decoupled if overlapping resolvers are ever added.
      resolver_profile: string,
      path: string,
      license: string,
      // OA transparency (#281 item 4): "gold"|"green"|"hybrid"|"bronze"|
      // "closed" (Unpaywall) or "green" (arXiv); null when not determined.
      // With `pdf.status` an agent tells "paywalled" (closed + no_oa_url)
      // from "couldn't reach it". Also on doiget_metadata_only + batch JSON.
      oa_status: string | null,
      size_bytes: number,
      schema_version: string,
      // Issue #118 / #243: PDF leg status. Always present on ok:true responses.
      pdf: PdfLeg,
    }
  | { ok: true, dry_run: true, ref: RefShape, plan: FetchPlan,
      rate_limit_budget: { global_per_sec: number, per_source_min_gap_ms: number } }
  | { ok: false,
      ref: string,
      error: { code: ErrorCode, message: string, denial_context?: DenialContext }
    };

type PdfLeg =
  | { status: "fetched" }
  | { status: "no_oa_url" }
  | { status: "blocked",
      code: ErrorCode,
      message: string,
      // Present when the failure was an allowlist / scheme denial (ADR-0023).
      denial_context?: DenialContext,
      // Present when Unpaywall metadata includes an arXiv alternative URL
      // for the same paper. The version suffix (e.g. `v2`) is stripped so
      // the ID refers to the latest version. Use as:
      //   doiget fetch arxiv:<suggested_arxiv_id>
      suggested_arxiv_id?: string,
    }
  | { status: "unknown" };

type DenialContext = {
  reason: "redirect_not_in_allowlist" | "insecure_scheme"
        | "host_in_block_list"
        | "size_cap_exceeded" | "schema_drift" | "capability_not_granted"
        | "rate_limit_window" | "ssrf_private_address" | "content_type_mismatch",
  source?: string,
  attempted?: string,
  // `expected?` is absent when the producer did not populate this field for
  // this reason. An empty array (`"expected": []`) is the distinct
  // "explicit empty allowlist" signal — see ADR-0023 §3 for the
  // None / Some(vec![]) disambiguation.
  expected?: string[],
  hop_index?: number,
  cap?: number,
  actual?: number,
};
```

`ErrorCode` is the closed enum in [`ERRORS.md`](ERRORS.md). `DenialContext`
is the optional structured-recovery payload defined in
[ADR-0023](DECISIONS/0023-denial-context-structured.md). `FetchPlan` is the
dry-run preview shape — see §10 below.

### 5.1 `denial_context` presence: single-paper vs batch (NORMATIVE)

There is an **intentional, normative asymmetry** in how the optional
`denial_context` field is represented on an `ok:false` error:

- **Single-paper tools** (`doiget_fetch_paper`, `doiget_metadata_only`,
  `doiget_resolve_paper`): the `denial_context` key is **omit-when-None**.
  When the error *does* carry a structured recovery channel (e.g. a
  `CAPABILITY_DENIED` allowlist/scheme denial), the key **is present**
  with the `DenialContext` payload; when there is no denial channel for
  the error (e.g. a `NETWORK_ERROR`), the key is **omitted** entirely.
  `doiget_resolve_paper` follows this exact same contract as the other
  two single-paper tools — it is *not* a tool that can never carry a
  denial context; the key is simply absent rather than `null` when there
  is nothing to report. Agents MUST treat absence and `null` as
  equivalent ("no structured recovery payload").
- **`doiget_batch_fetch`** per-ref error entries: the `denial_context`
  key is **always present**, set to `null` when there is no denial
  channel. Per-ref rows are uniform table rows in the agent's view, so
  the explicit `null` lets an agent index every row's
  `error.denial_context` without a presence test.

An agent that wants to work across both surfaces should read
`error.denial_context` and treat both *missing* and `null` as "none".
A serialization failure of a non-`null` `DenialContext` (today
unreachable — the type is a typed `Serialize` struct) emits `null` **and
a `tracing::warn!` on stderr** so the swallow is observable; it is never
silent (see #154 / ADR-0023 §4).

## 6. Excluded tools (permanent)

The following are intentionally **not** offered as MCP tools and will not be added.
See [`SCOPE.md`](SCOPE.md) §"Credential / safety non-goals":

- `doiget_delete_paper(...)` — destructive store ops are CLI-only.
- `doiget_set_credentials(...)` — credentials never enter the MCP surface.
- `doiget_run_shell(...)` — no generic command escape.
- `doiget_fetch_url(url: ...)` — SSRF surface; only DOI / arXiv id input.

## 7. Capability awareness

Agents can call `doiget_capability_profile` first to determine which sources the
instance is allowed to use. The output is redacted (no API key contents) and is suitable
for an agent to use in planning whether a TDM-class fetch will succeed.

```typescript
type CapabilityProfileResponse = {
  oa_enabled: true,
  metadata_sources: string[],          // e.g. ["openalex"]
  tdm_enabled: boolean,                // disjunction over individual TDM grants
  tdm_elsevier: boolean,
  tdm_aps: boolean,
  tdm_springer: boolean,
  rate_limit_per_sec: number,          // always 5.0
};
```

## 8. Server lifecycle

- Started by an MCP host as `doiget serve`.
- stdin EOF triggers a 5-second graceful shutdown that completes ongoing fetches and
  releases store locks.
- stdout carries only JSON-RPC frames (banner, log, progress all forbidden, see
  [`SECURITY.md`](SECURITY.md) §3).
- stderr carries `tracing-subscriber` output (`RUST_LOG` controlled).

## 9. Smoke test

A CI workflow `mcp-smoke.yml` spawns the server, sends a minimal sequence
(`initialize` → `tools/list` → `tools/call doiget_health`), asserts the responses, and
asserts that no stray bytes appeared on stdout outside JSON-RPC frames.

## 10. Dry-run preview (NORMATIVE; ADR-0022)

`doiget_fetch_paper`, `doiget_metadata_only`, and `doiget_batch_fetch` accept
an optional `dry_run: boolean` input field, defaulting to `false`. When
`true`:

- The orchestrator builds a `FetchPlan` and returns it without touching the
  network or the filesystem and without appending a provenance row.
- The result envelope is `{ ok: true, dry_run: true, ref, plan,
  rate_limit_budget }`.
- The `plan.pdf_sources[].candidate_hosts` list is the **static allowlist**
  for the resolver, not a prediction of the single host the real fetch would
  hit. doiget cannot resolve the post-Unpaywall OA URL host without making
  the Unpaywall call, and `dry_run` MUST NOT make it.
- The `plan.candidate_hosts_are_upper_bound` boolean is always `true` and
  machine-encodes the bullet above (ADR-0022 §4) directly into
  the wire envelope, so an agent can detect the upper-bound semantics
  without consulting the spec.

```jsonc
{
  "ok": true,
  "dry_run": true,
  "ref": { "doi": "10.1234/foo" },
  "plan": {
    "metadata_sources": ["crossref", "unpaywall"],
    "pdf_sources":      [{
      "key":             "oa-publisher",
      "candidate_hosts": ["*.springer.com", "*.springeropen.com"]
    }],
    "redirect_allowlists_loaded":      ["crossref", "unpaywall", "arxiv", "oa-publisher"],
    "candidate_hosts_are_upper_bound": true,
    "target_pdf_path":                 "/home/.../store/doi_10.1234_foo.pdf",
    "target_metadata_path":            "/home/.../store/doi_10.1234_foo.toml",
    "would_append_provenance":         true
  },
  "rate_limit_budget": {
    "global_per_sec":        5.0,
    "per_source_min_gap_ms": 200
  }
}
```

Tools where `dry_run` does not apply (`doiget_info`, `doiget_search_local`,
`doiget_paper_search`, `doiget_paper_text`, `doiget_link`, `doiget_list_recent`, `doiget_paper_pdf_path`,
`doiget_capability_profile`, `doiget_health`, `doiget_resolve_paper`) reject the field as
`INVALID_REF`-class — i.e. surface as
`{ok:false, error:{code:"INVALID_REF", ...}}`.

## 11. `doiget_metadata_only` (NORMATIVE)

`doiget_metadata_only` resolves a `ref` through the configured metadata
sources (Crossref + Unpaywall + arXiv-meta) and returns the resulting
metadata. It **MUST NOT** trigger a publisher-side PDF fetch, even when the
metadata source returns an OA URL. The OA URL, when known, is surfaced in the
response as `oa_url` (string) for the caller to act on separately.

```jsonc
{
  "name": "doiget_metadata_only",
  "description": "WHEN TO USE: User wants metadata for a DOI / arXiv id without paying for or being noticed by a PDF download.\nINPUTS: ref (DOI or arXiv id), dry_run (optional bool).\nOUTPUTS: { ok: true, ref, source, license?, oa_url:string|null, metadata } or { ok:false, error }.\nCOSTS: 1-2 s metadata round-trip. No publisher fetch.\nSIDE EFFECTS: Appends a provenance row tagged 'metadata-only' (unless dry_run). Writes the metadata TOML to the store.\nLIMITS: Subject to the same rate cap as fetch_paper (5/sec). The OA URL is reported but never followed.",
  "inputSchema": {
    "type": "object",
    "required": ["ref"],
    "properties": {
      "ref": {
        "type": "string",
        "minLength": 7,
        "maxLength": 256,
        "pattern": "^(10\\.\\d{4,9}/[A-Za-z0-9._/()-]+|arXiv:\\d{4}\\.\\d{4,5}|\\d{4}\\.\\d{4,5})$"
      },
      "dry_run": { "type": "boolean", "default": false }
    },
    "additionalProperties": false
  }
}
```

Output:

```typescript
type MetadataOnlyResult =
  | { ok: true,
      ref: string,
      source: "crossref" | "unpaywall" | "arxiv",
      // ADR-0021 §4 / ADR-0024: the resolver profile under which the
      // canonical-digest for this metadata-only call was minted.
      // Currently equal to `source` verbatim.
      resolver_profile: string,
      license: string,
      oa_url: string | null,
      metadata: object,
      schema_version: string,
    }
  | { ok: true, dry_run: true, ref: RefShape, plan: FetchPlan,
      rate_limit_budget: { global_per_sec: number, per_source_min_gap_ms: number } }
  | { ok: false, ref: string, error: { code: ErrorCode, message: string, denial_context?: DenialContext } };
```

Posture: covered by the same posture-lint check as ADR-0022 §5 — a
`metadata_only` codepath that reaches `HttpClient::fetch_pdf` is a hard
failure.

## 12. `doiget_resolve_citation` and `doiget_batch_resolve_citations` (NORMATIVE)

`doiget_resolve_citation` resolves a free-form bibliographic citation string to ranked DOI candidates via the Crossref works query API.

`doiget_batch_resolve_citations` batch-resolves multiple citation strings (up to 50).

Both tools calculate token-based overlap similarity scores, filter out results with a score < 0.5, and sort candidates by score descending. No local store writes or provenance logs are created.
