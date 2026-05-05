# MCP tools

> **Status: NORMATIVE.** Defines the tool surface exposed by `doiget serve` over stdio
> JSON-RPC. Renaming or removing a tool is a breaking change.

`doiget` runs as a Model Context Protocol server when invoked as `doiget serve`. It
speaks **stdio only** ([ADR-0001](DECISIONS/), [`SCOPE.md`](SCOPE.md) §non-goal 6).

## 1. Tool list (Phase 3 baseline)

| Tool | Purpose |
|---|---|
| `doiget_resolve_paper` | Resolve DOI / arXiv id to authoritative metadata. |
| `doiget_fetch_paper` | Resolve and download a single PDF to the store. |
| `doiget_batch_fetch` | Up to 100 refs in one call. |
| `doiget_info` | Retrieve a store entry's metadata. |
| `doiget_search_local` | Search store metadata (title / authors / venue). |
| `doiget_list_recent` | Last N fetched entries. |
| `doiget_paper_pdf_path` | Return the local path of a cached PDF. **Does not read content.** |
| `doiget_capability_profile` | Report which sources this instance is allowed to use. |
| `doiget_health` | Operational sanity (store writable, version, schema). |

Phase 4 adds:

| Tool | Purpose |
|---|---|
| `doiget_expand_citation_graph` | BFS expansion of citations. Hard-capped. |
| `doiget_bibtex_export` | BibTeX for one or many entries. |
| `doiget_csl_export` | CSL JSON for one or many entries. |

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
            | "openalex" | "s2" | "doaj"
            | "tdm-elsevier" | "tdm-aps" | "tdm-springer",
      path: string,
      license: string,
      size_bytes: number,
      schema_version: string,
    }
  | { ok: false,
      ref: string,
      error: { code: ErrorCode, message: string }
    };
```

`ErrorCode` is the closed enum in [`ERRORS.md`](ERRORS.md).

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

A CI workflow `mcp-smoke.yml` (Phase 3) spawns the server, sends a minimal sequence
(`initialize` → `tools/list` → `tools/call doiget_health`), asserts the responses, and
asserts that no stray bytes appeared on stdout outside JSON-RPC frames.
