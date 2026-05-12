# 0022 - Dry-run mode for fetch operations

- **Date:** 2026-05-12
- **Status:** Proposed
- **Supersedes:** -
- **Source:** Discussion #12 (musaabhasan, 2026-05-08)

## Context

The external review on Discussion #12 (musaabhasan, 2026-05-08, comment 1)
asks doiget to provide

> a dry-run mode that reports the planned resolver/host/path before touching
> network or filesystem.

Phase 1 today exposes `doiget fetch <ref>` (CLI) and `doiget_fetch_paper`
(MCP). Both run the full orchestrator: pick the metadata sources that
`can_serve` the ref under the current `CapabilityProfile`, hit the network,
write the PDF and metadata TOML, and append to the provenance log. There is no
way to ask "what *would* you do?" without paying for the network round-trip
and the on-disk side-effects.

The asymmetry matters for three audiences:

- **Agents.** An LLM building a multi-step plan benefits from being able to
  preview the resolver chain ("I will fetch via Unpaywall, falling back to
  arXiv if the OA URL host is denied") before committing the plan to action.
- **Users debugging configuration drift.** "Why did this DOI go through arXiv
  yesterday and Unpaywall today?" — a `--dry-run` answer is faster than
  reading the provenance log.
- **Security-minded operators.** The review's framing is explicitly about
  separating *intent* from *side effect*: a dry-run produces an audit-quality
  preview of where bytes would have flowed, with no bytes flowing.

## Decision

### 1. CLI flag

`doiget fetch <ref> --dry-run` and `doiget batch <path> --dry-run` produce a
structured preview and exit `0` with no network call, no file write, and no
provenance row appended.

The preview shape (NORMATIVE) is

```jsonc
{
  "ok": true,
  "dry_run": true,
  "ref": { "doi": "10.1234/foo" } | { "arxiv": "2401.12345" },
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

The preview is emitted on `stdout` when `--json` is also passed (or when the
output mode resolves to `json` per ADR-0017); otherwise a human-friendly form
is emitted on `stderr` and `stdout` carries no bytes. The
`would_append_provenance` field is always `true` in Phase 1+ (every successful
fetch appends a row), but it is named explicitly so future fetch modes can
declare "this fetch would NOT append" without having to invert the flag's
meaning.

The `candidate_hosts_are_upper_bound` field is always `true` in Phase 1 and
machine-encodes the §4 ("Honesty about candidate uncertainty") disclaimer
directly into the wire envelope: `pdf_sources[].candidate_hosts` is the
static resolver allowlist, NOT a prediction of the single host the real
fetch would touch. The field exists so an agent can detect the upper-bound
semantics without consulting the spec.

### 2. MCP parameter

`doiget_fetch_paper` and `doiget_metadata_only` (ADR-0023's companion tool)
accept an optional `dry_run: boolean` input field, defaulting to `false`. When
`true`, the tool returns a result with the same `dry_run: true` /
`plan: {...}` shape as the CLI, never touches the network, and never writes
to the store.

The `dry_run` flag is rejected as `INVALID_REF`-class (i.e. surfaces as
`{ok:false, error:{code:"INVALID_REF", ...}}`) on tools where it does not
apply (e.g. `doiget_info` or `doiget_search_local`), to keep the MCP surface
homogeneous and to prevent silent acceptance of irrelevant flags.

### 3. No partial dry-run

Dry-run is all-or-nothing per call: either no network and no writes happen, or
the call is not a dry-run and the full orchestrator runs. There is no
"dry-run the network leg but write the metadata TOML" mode and there is no
"dry-run only the PDF leg" mode. This keeps the auditing story simple — every
provenance row corresponds to a real fetch, and every dry-run preview
corresponds to zero on-disk side-effects.

### 4. Honesty about candidate uncertainty

The `pdf_sources[].candidate_hosts` list is the static allowlist for the
named resolver, not the host the actual fetch would have hit. doiget cannot
know the post-Unpaywall OA URL host without making the Unpaywall network
call, and `--dry-run` MUST NOT make it. The preview is therefore an
*upper-bound* on the hosts a real fetch could touch, not a prediction of the
single host it would touch. This is documented in
[`MCP_TOOLS.md`](../MCP_TOOLS.md) §"Dry-run preview semantics" and in the CLI
help text for `--dry-run`.

### 5. Posture lint

The `posture-lint.yml` workflow gains a check that `--dry-run`-tagged code
paths in `doiget-cli::commands::fetch` and `doiget-mcp` never reach
`HttpClient::fetch_bytes` / `fetch_pdf` or `FsStore::write_*` /
`ProvenanceLog::append`. The check is a `grep`-style scan against a
hand-maintained allowlist of branch points, mirroring the existing
no-stdout-in-MCP check from ADR-0001.

## Consequences

**Positive.**
- Agents can preview a fetch plan as a normal MCP call — no out-of-band
  configuration introspection required.
- Operators can validate `CapabilityProfile`, `redirect_allowlist`, and store
  paths without committing to a fetch — useful when a TDM grant is being
  configured and the operator wants to confirm that "yes, this DOI will go
  through `tdm-springer` once `DOIGET_AGREE_TDM_SPRINGER=1` is set."
- The preview shape is a stable contract; integration tests can pin it.

**Negative.**
- One more codepath to maintain in the orchestrator — the "build a plan
  without executing it" path. Mitigated by the posture-lint check (§5) that
  asserts the dry-run path never reaches the side-effecting modules.
- Honest preview semantics (§4) require explaining to agents that
  `candidate_hosts` is an upper bound, not a prediction. Documented in
  `MCP_TOOLS.md` and the CLI help.

**Out of scope.**
- A "live preview" mode that hits the metadata sources but skips the PDF leg
  — that is the role of the `doiget_metadata_only` tool (ADR-0023's companion
  tool), not of `--dry-run`.
- Persisting dry-run previews to the provenance log. Dry-runs are
  zero-side-effect by definition; if an operator wants a record of what they
  tried, `doiget fetch ... --dry-run --json | tee preview.json` is the
  workflow.

To revise this decision, write a new ADR with Status: Accepted and Supersedes:
0022, and update this file's Status to Superseded by NNNN per CONTRIBUTING.md.
