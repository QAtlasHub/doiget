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

## Permanent non-goals

These items are **permanent non-goals**. Their inclusion in doiget will be refused.

A non-goal can be re-evaluated only by opening a new GitHub Discussion titled
`[scope-reopening] <topic>` and obtaining explicit maintainer approval before any code
is written. PRs that effectively reverse a non-goal without this process will be closed.

### Content / processing non-goals

1. **PDF content processing.** doiget does not extract text, perform OCR, summarize,
   parse citations from PDF text, or extract annotations. PDFs are treated as opaque
   blobs. (ADR-0003; see also [`MCP_TOOLS.md`](MCP_TOOLS.md): `paper_pdf_path` returns
   only a path.)
2. **Bibliographic enrichment from PDF.** doiget does not read bibliographic information
   from PDF metadata streams. Metadata comes only from publisher APIs.

### Distribution / hosting non-goals

3. **No SaaS / hosted service.** doiget does not operate `doiget.example`, a hosted MCP
   endpoint, a public proxy, or any maintainer-controlled service that fetches on behalf
   of users.
4. **No paper hosting or redistribution.** doiget does not redistribute fetched PDFs.
   The `Store` is local-only.
5. **No `share-vault` feature.** doiget does not provide a mechanism for one user to
   share their `~/papers/` store with another user as a doiget-supported feature.

### Network / transport non-goals

6. **No MCP HTTP / SSE / WebSocket transport.** doiget supports MCP via stdio only.
   This is intentional, not a TODO. (ADR-0001) A multi-tenant network-exposed doiget
   would shift the user's role from contract party to service consumer, which conflicts
   with the [`LEGAL.md`](LEGAL.md) posture.
7. **No `doiget_fetch_url(url: ...)` MCP tool.** Tools accept DOI / arXiv id input only,
   never arbitrary URLs (SSRF surface; see [`SECURITY.md`](SECURITY.md) §threat 2).

### Credential / safety non-goals

8. **No bundled API keys.** No publisher API key is shipped in any doiget binary.
9. **No credential sharing feature.** doiget does not provide a mechanism for sharing
   API keys, sessions, or institutional access between users.
10. **No `doiget_set_credentials(...)` MCP tool.** Credentials are read from env or
    `credentials.toml` only; the MCP surface does not accept credential writes.
11. **No `doiget_delete_paper(...)` MCP tool.** Destructive store operations are CLI-only,
    never agent-invokable.
12. **No generic shell / exec MCP tool.** doiget never exposes a tool that lets an agent
    run arbitrary commands.

### Operational non-goals

13. **No bulk download mode.** Rate limiting (`MAX_CONCURRENT_FETCHES = 5`,
    `MAX_FETCHES_PER_SECOND = 5.0`) is hard-coded as library constants. There is no flag
    or config to raise these.
14. **No telemetry / phone-home / crash reporting / version check.** doiget makes no
    network connection that is not the result of a user-initiated fetch. (ADR-0015)
15. **No self-update / `doiget upgrade`.** doiget does not modify its own binary. (ADR-0015)

### Build / distribution non-goals

16. **No `tdm-all` umbrella feature flag.** Each TDM source must be opted in
    individually. (ADR-0002)
17. **No public binary release that includes any TDM source code.** TDM features are
    available only by user-driven `cargo install` / `cargo build` with the appropriate
    `--features tdm-<publisher>` flag.

### Integration non-goals (Obsidian)

18. **No bidirectional Obsidian sync.** Obsidian export, when available (Phase 7), writes
    only one direction: store → vault. The vault is not read back as a source of truth.
19. **No Obsidian vault auto-discovery.** The vault path is always passed explicitly by
    the user.

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
