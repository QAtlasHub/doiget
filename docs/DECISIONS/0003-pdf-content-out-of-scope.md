# 0003 - PDF content processing is permanently out of scope

- **Date:** 2026-05-05
- **Status:** Accepted — standing policy, enforced; `docs/SCOPE.md` permanent non-goal #1 + posture-lint; no PDF-content code path exists in any shipped slice
- **Supersedes:** -
- **Source:** Discussion #9

## Context

This is a stub ADR. The full Context, Decision, and Consequences sections will be
expanded during Phase 0 from the corresponding GitHub Discussion review threads.
The summary below is sufficient for ADR INDEX.md and for cross-references in the
NORMATIVE specs to resolve.

## Decision

doiget treats PDFs as opaque blobs. Text extraction, OCR, summarization, citation parsing from PDF text, and annotation extraction are all permanent non-goals. Users compose doiget with downstream tools (paper-qa, marker, etc.) rather than expecting doiget to grow content-processing features.

## Consequences

See the source Discussion for the analyzed positives and negatives. The binding
result is captured in the Decision above and is referenced from the relevant
NORMATIVE doc(s) under docs/.

To revise this decision, write a new ADR with Status: Accepted and Supersedes: 0003,
and update this file's Status to Superseded by NNNN per CONTRIBUTING.md.
