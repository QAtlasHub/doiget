# 0032 - Structured full-text (HTML/XML) extraction is in scope; PDF-blob content processing remains out of scope

- **Date:** 2026-06-06
- **Status:** Accepted (design; implementation slice tracked separately —
  PR4 ships the ar5iv leg, PMC JATS is a planned follow-up).
- **Amends:** ADR-0003 (PDF content processing out of scope) — narrows it
  to **PDF-blob** processing, see D1; and `docs/SCOPE.md` permanent
  non-goal #1, which gains the explicit carve sentence in D1.
- **Supersedes:** -
- **Source:** #281 item 3 (`paper_text` — "Read. No text extraction today
  … → need `paper_text`"); maintainer scope decision 2026-06-06
  ("HTML/XML 全文は可能です / pdf の処理はしないという意味です").

## Context

`doiget` covers `search → triage → expand → fetch` but not **read**: an
agent that has identified a relevant paper still has to shell out to an
external pdf-to-text tool to get its content. #281 item 3 asks for
`paper_text` so the loop can self-complete.

The wrinkle is ADR-0003 / `docs/SCOPE.md` permanent non-goal #1, **"PDF
content processing"**:

> doiget does not extract text, perform OCR, summarize, parse citations
> from PDF text, extract annotations, or read bibliographic data from PDF
> metadata streams. PDFs are treated as opaque blobs.

Read literally, "extract text" could be taken to forbid `paper_text`
entirely. But the non-goal is, by its title and every clause, about the
**PDF blob** — and it *already* draws the relevant line itself:

> Bibliographic indexing from publisher API responses … is **in scope and
> distinct from PDF content interpretation**.

i.e. doiget already treats "structured content from a source that
publishes it as structured data" as a different category from "interpret
the bytes inside a PDF". An abstract from OpenAlex is in scope; OCR of a
scanned PDF is not. `paper_text` sits on the in-scope side **iff** it
sources text from an already-structured artifact and never touches the
PDF blob.

The maintainer confirmed this reading on 2026-06-06: HTML/XML full text
is acceptable; the non-goal's intent is specifically "do not process the
PDF". This ADR records that boundary so it cannot drift.

## Decision

### D1 — Narrow non-goal #1 to *PDF-blob* processing; structured full-text is in scope

`doiget` may extract full text **only** from sources that publish it as
already-structured HTML/XML (e.g. ar5iv's LaTeXML XHTML for arXiv, PMC /
Europe PMC JATS XML). The following stay **permanently out of scope**,
exactly as ADR-0003 / non-goal #1 states:

- extracting text from the **PDF blob** (parsing PDF content streams),
- OCR of any kind,
- summarization,
- parsing citations from PDF text,
- annotation extraction,
- reading bibliographic data from PDF metadata streams.

PDFs remain **opaque blobs** (`paper_pdf_path` still returns only a
path). The PDF is fetched, hashed, and stored byte-for-byte; doiget never
looks inside it. `paper_text` is a *separate* fetch of a *separate*
artifact (the publisher's HTML/XML rendering), not a reinterpretation of
the stored PDF.

`docs/SCOPE.md` non-goal #1 gains a carve sentence pointing here, in the
same spirit as the existing "bibliographic indexing … is in scope and
distinct from PDF content interpretation" clause. This is a
**clarification of a boundary, not a reversal** of the permanent
non-goal: the load-bearing prohibition (no PDF-blob processing, no OCR)
is unchanged and, if anything, stated more precisely.

### D2 — `paper_text` is a Tier-1, always-on capability

Full-text HTML/XML extraction is classified **Tier 1 OA metadata**, same
posture class as discovery search (ADR-0031): read-only, open-access,
never paywalled, never reinterprets a PDF. It ships in the default
`oa-only` binary with **no env gate and no Cargo feature gate**. The
sources it reads (ar5iv, PMC) are open full-text renderings of OA papers;
gated/paywalled full text is never bypassed (a paper with no OA full-text
source yields a structured "no full-text source" outcome, never a
paywall workaround).

### D3 — ar5iv is the PR4 source; the parser is best-effort over LaTeXML XHTML

PR4 implements a single full-text source, **ar5iv**
(`ar5iv.labs.arxiv.org/html/<arxiv-id>`), which renders arXiv papers as
LaTeXML XHTML. A new always-compiled `http::fulltext_allowlist()`
registers `ar5iv.labs.arxiv.org` under a dedicated `"ar5iv"` source key —
distinct from the `"arxiv"` PDF/Atom key so the provenance trail records
that the text came from the ar5iv HTML renderer, not the arxiv PDF API.
The host is a `*.arxiv.org` subdomain, so it is within the existing arXiv
network surface (ADR-0027 / `REDIRECT_ALLOWLIST.md`); the dedicated key
keeps the audit label precise.

Extraction is **best-effort**: a `quick-xml` walk (the same parser the
arXiv Atom path uses) splits the document into `{ heading, text }`
sections on `h1`–`h6`, skips `script` / `style` / `math` subtrees
(capturing each `<math>`'s `alttext`, the LaTeX source, as inline text so
formulae read as `\(…\)` rather than MathML noise), and normalizes
whitespace. The result is cached full, then truncated to the caller's
`max_chars` on return (truncation is flagged, never silent).

### D4 — Cache in the doiget-private cache root, not the shared `~/papers/` store

Extracted text is cached at `<cache_root>/text/<safekey>.json`
(`docs/CACHE.md`), the doiget-private latency cache — **not** the
`~/papers/` store (`docs/STORE.md`). STORE.md is a NORMATIVE spec shared
with BiblioFetch.jl; adding a `.text/` artifact there would require
cross-tool coordination. The cache is best-effort (a miss / parse error /
write failure degrades to a re-fetch, never an error), mirroring the
resolver cache. The cache holds the **full** extracted text; `max_chars`
truncation is a view applied on return, so the same cached entry serves
any `max_chars`.

### D5 — DOI input and MCP exposure are separate slices

`paper_text`'s core takes an **arXiv id**. The CLI `doiget text <ref>`
accepts a `Ref`: an arXiv id reads via ar5iv; a bare **DOI** yields a
structured `NO_OA_AVAILABLE` ("no full-text source for this id — pass the
arXiv id if a preprint exists"). DOI→arXiv resolution is #281 item 5
(arXiv↔DOI dedup), a separate PR. The MCP `doiget_paper_text` tool is the
next slice (#281 item 2), mirroring how `doiget_paper_search` followed
`doiget search`. This ADR governs the core capability + CLI surface only.

## Consequences

### Positive

1. The shipped `oa-only` binary gains the **read** step of the #281 loop:
   `doiget text arxiv:2401.12345` returns sectioned full text, no env
   var, no external pdf-to-text tool.
2. The permanent non-goal is now stated **precisely** (PDF-blob, OCR) and
   is easier to defend, not weaker: a future "extract text from the PDF"
   or "OCR" PR is still closed on sight.
3. No new env var / Cargo feature for the common case; one clearly-scoped
   always-on Tier-1 capability, consistent with ADR-0031.

### Negative

1. **A permanent non-goal was touched.** Even as a clarification, #1 now
   reads "PDF-blob processing" rather than a blanket "text extraction".
   Mitigated by D1's explicit, narrow enumeration of what stays out and
   by the maintainer scope decision recorded above.
2. **Best-effort extraction can be imperfect.** LaTeXML XHTML varies;
   some papers extract cleanly, some lose structure, some aren't on ar5iv
   at all (→ a structured `NOT_FOUND`). doiget supplies the text it can
   and flags truncation; it does not promise faithful reconstruction.
3. **A second arXiv-facing host** (`ar5iv.labs.arxiv.org`) joins the
   network surface. It is a `*.arxiv.org` subdomain already covered by
   the arXiv allowlist policy; the distinct source key is the guardrail.

### Migration

- None for existing users: `paper_text` / `doiget text` is purely
  additive. No env-var or store-schema change (the text cache lives in
  the doiget-private cache root).

## References

- #281 item 3 (`paper_text` — the read step)
- ADR-0003 (PDF content processing out of scope — narrowed by D1)
- ADR-0031 (discovery search Tier-1 always-on — the posture template D2 follows)
- ADR-0027 / `docs/REDIRECT_ALLOWLIST.md` (the allowlist `fulltext_allowlist()` plugs into)
- `docs/SCOPE.md` non-goal #1 (gains the D1 carve sentence)
- `docs/CACHE.md` (the cache root D4 reuses)
- `docs/STORE.md` (the shared store D4 deliberately does NOT touch)
