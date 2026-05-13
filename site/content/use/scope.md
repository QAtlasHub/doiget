+++
title = "Scope (what doiget does and does not do)"
description = "A single-binary CLI plus stdio MCP server that:"
weight = 130
+++

# Scope and Permanent Non-Goals

> **Status: NORMATIVE.** This document defines binding contracts. Implementations MUST
> conform. Items listed under "Permanent non-goals" cannot be reversed by a PR; see
> [`../CONTRIBUTING.md`](../CONTRIBUTING.md) §"Scope-reopening meta-rule".

## What doiget is

A single-binary CLI plus stdio MCP server that:

1. Resolves DOI / arXiv id input to authoritative metadata via Crossref / Unpaywall / arXiv
   (Phase 1).
2. Fetches PDFs through Open Access sources by default; opens additional metadata sources
   in Phase 4 and gated TDM sources in Phase 5 only when the user has explicitly opted in
   at build time and runtime.
3. Stores fetched papers in a `~/papers/` layout that is bit-compatible with BiblioFetch.jl
   (see [`STORE.md`](STORE.md)).
4. Exposes a stdio MCP server with a fixed set of structured tools to agent hosts (see
   [`MCP_TOOLS.md`](MCP_TOOLS.md)).

That is the totality of doiget's intended scope. All other functionality is either
explicitly out of scope below or requires a new ADR.

## Two classes of non-goal

doiget's non-goals split into two classes:

- **Permanent non-goals.** Reversing one would weaken the
  [`LEGAL.md`](LEGAL.md) posture, the [`SECURITY.md`](SECURITY.md) threat
  model, or the contract-party structure that makes doiget a user-driven
  local tool. They can be re-evaluated **only** by opening a GitHub
  Discussion titled `[scope-reopening] <topic>` and obtaining explicit
  maintainer approval before any code is written. PRs that effectively
  reverse one will be closed.
- **Current design choices.** Operational / UX preferences that are
  *currently* out of scope, but whose reversal does not threaten the posture
  or threat model. They can be changed by a regular ADR (no
  scope-reopening Discussion required).

The first class is the disciplined heart of doiget; the second class is here
so contributors know the maintainer's preferences without needing to ask.

A future ADR may freely move an item from "Current design choices" to
"Permanent non-goals" — that direction tightens scope and never requires a
scope-reopening Discussion. Moving the other way (Permanent → Current →
Removed) requires the full meta-rule.

## Permanent non-goals (14)

### Content / processing

1. **PDF content processing.** doiget does not extract text, perform OCR,
   summarize, parse citations from PDF text, extract annotations, or read
   bibliographic data from PDF metadata streams. PDFs are treated as opaque
   blobs. (ADR-0003; see also [`MCP_TOOLS.md`](MCP_TOOLS.md): `paper_pdf_path`
   returns only a path.) Bibliographic indexing from publisher API responses
   (title / authors / venue / abstract for `search_local`) is in scope and
   distinct from PDF content interpretation; see [`LEGAL.md`](LEGAL.md) §3.

### Distribution / hosting

2. **No SaaS / hosted service.** doiget does not operate `doiget.example`, a
   hosted MCP endpoint, a public proxy, or any maintainer-controlled service
   that fetches on behalf of users.
3. **No paper hosting, redistribution, or "share-vault".** doiget does not
   redistribute fetched PDFs and does not provide any mechanism for one user
   to share their `~/papers/` store with another. The `Store` is local-only.

### Network / transport

4. **No MCP HTTP / SSE / WebSocket transport.** doiget supports MCP via stdio
   only. This is intentional, not a TODO. (ADR-0001.) A multi-tenant
   network-exposed doiget would shift the user's role from contract party to
   service consumer, which conflicts with the [`LEGAL.md`](LEGAL.md) posture.
5. **No `doiget_fetch_url(url: ...)` MCP tool.** Tools accept DOI / arXiv id
   input only, never arbitrary URLs (SSRF surface; see
   [`SECURITY.md`](SECURITY.md) §1.4).

### Credential / safety

6. **No bundled API keys.** No publisher API key is shipped in any doiget
   binary.
7. **No credential sharing feature.** doiget does not provide a mechanism for
   sharing API keys, sessions, or institutional access between users.
8. **No `doiget_set_credentials(...)` MCP tool.** Credentials are read from
   env or `credentials.toml` only; the MCP surface does not accept credential
   writes.

### Operational

9.  **No bulk download mode.** Rate limiting
    (`MAX_CONCURRENT_FETCHES = 5`, `MAX_FETCHES_PER_SECOND = 5.0`) is hard-coded
    as library constants. There is no flag or config to raise these.
10. **No telemetry / phone-home / crash reporting / version check.** doiget
    makes no network connection that is not the result of a user-initiated
    fetch. (ADR-0015.)
11. **No self-update / `doiget upgrade`.** doiget does not modify its own
    binary. (ADR-0015.)

### Build / distribution

12. **No `tdm-all` umbrella feature flag.** Each TDM source must be opted in
    individually. (ADR-0002.) Removing the per-publisher friction is the same
    kind of LEGAL-posture weakening as bundling keys.
13. **No public binary release that includes any TDM source code.** TDM
    features are available only by user-driven `cargo install` / `cargo build`
    with the appropriate `--features tdm-<publisher>` flag.

### Posture interpretation

14. **No re-classification of bibliographic indexing as "PDF content
    processing".** Implementing `search_local` over title/authors/venue is
    explicitly in scope and is **not** governed by item #1 above. This is the
    same boundary [`LEGAL.md`](LEGAL.md) §3 documents.

## Current design choices (5)

These are out of scope **today** because the maintainer judges them more
trouble than they're worth at the current Phase, but they do not threaten the
LEGAL posture or threat model. Reversing one is a regular ADR, not a
scope-reopening Discussion.

15. **No `doiget_delete_paper(...)` MCP tool.** Destructive store operations
    are CLI-only. *Why current-only:* the operational caution (agents
    shouldn't delete user data accidentally) is real, but a future agent
    workflow with explicit user confirmation could revisit this.
16. **No generic shell / exec MCP tool.** doiget never exposes a tool that
    lets an agent run arbitrary commands. *Why current-only:* the security
    boundary is correct today, but composability with sandboxed shells could
    be revisited in Phase 5+.
17. **No bidirectional Obsidian sync.** Obsidian export, when available
    (Phase 7), writes only one direction: store → vault. *Why current-only:*
    conflict resolution complexity is the only reason; if a clean
    last-writer-wins or three-way-merge design surfaces, this can change.
18. **No Obsidian vault auto-discovery.** The vault path is always passed
    explicitly by the user. *Why current-only:* false-positive scanning is
    the only concern; a workspace-style `.doiget/vault.toml` could
    legitimately revisit this.
19. **No `INTEGRATION/` host snippets at Phase 0.** Phase 3 ships them
    alongside `doiget serve`. *Why current-only:* sequencing, not principle.

## Boundaries with adjacent tools

doiget composes with content-processing tools rather than incorporating them:

- For PDF text extraction / OCR / summarization: pair doiget with
  [paper-qa](https://github.com/whitead/paper-qa),
  [marker](https://github.com/VikParuchuri/marker), or other dedicated tools. See
  `INTEGRATION/chain-with-paperqa.md` (Phase 3+).
- For Julia REPL workflows: use BiblioFetch.jl directly; doiget and BiblioFetch.jl share
  the on-disk store format ([`STORE.md`](STORE.md)).

## Why these are non-goals

The non-goal list is the most direct mechanism for keeping doiget's
[`LEGAL.md`](LEGAL.md) posture, [`SECURITY.md`](SECURITY.md) threat model, and operational
simplicity intact. Each non-goal corresponds to a specific risk:

| Non-goal | Primary risk if added |
|---|---|
| PDF content processing | Derivative-work copyright posture; tool-neutrality framing weakens. |
| MCP HTTP transport | Multi-tenant operational status; user is no longer the contract party. |
| Bundled API keys | Direct ToS violation; doiget becomes the contracting party. |
| `fetch_url(...)` tool | Generic SSRF surface; bypasses source-list discipline. |
| Bulk download mode | Bulk-scraper signature pattern; publisher-side flag-and-block. |
| Telemetry / self-update | Phone-home surface; supply-chain risk multiplier. |
| `tdm-all` umbrella flag | Removes the "agree per publisher" friction that grounds opt-in. |
| Bidirectional Obsidian sync | Conflict resolution complexity; user file overwrite incidents. |

If a future situation appears to motivate reversing one of these, the right path is a new
Discussion, not an in-line PR.
