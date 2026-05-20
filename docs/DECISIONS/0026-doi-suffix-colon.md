# 0026 - DOI suffix charset extension: permit `:` (SECURITY.md §1.1)

- **Date:** 2026-05-19
- **Status:** Accepted
- **Supersedes:** - (amends the `docs/SECURITY.md` §1.1 NORMATIVE charset; §1.1 remains binding with the widened set)
- **Source:** #194 (dogfood of an Ising-model RG corpus, classical → modern)

## Context

`docs/SECURITY.md` §1.1 mandates the strict DOI-suffix regex
`^10\.\d{4,9}/[A-Za-z0-9._/()-]+$` to block path traversal in the
untrusted DOI suffix. `Doi::parse` (`crates/doiget-core/src/lib.rs`)
mirrors this charset via `is_doi_suffix_char`.

The charset omitted `:`, but `:` **is a valid DOI suffix character** (the
DOI Handbook defines the suffix as an opaque string) and two large, real
publisher DOI families use it:

- **Legacy Kluwer / Springer**: `10.1023/A:1019601218492` — thousands of
  pre-2003 Kluwer journal DOIs use the `10.1023/A:NNNNNNNNNN` form.
- **EDP Sciences / Journal de Physique**: `10.1051/jphys:0198900500120136500`
  — the entire legacy *Journal de Physique* corpus.

Both resolve at `https://doi.org/` and via Crossref. Dogfooding a
historically deep Ising-model renormalization-group corpus via the
citation graph reached the older literature and lost 3/38 niche papers
purely to this parser restriction (Nelson–Fisher hyperscaling 1985,
Le Guillou–Zinn-Justin critical exponents 1989, the 2002 DMRG-QPT
review). Because §1.1 is posture, the charset is not widened
unilaterally in code — hence this ADR.

## Decision

Add `:` to the DOI-suffix charset. The §1.1 regex becomes
`^10\.\d{4,9}/[A-Za-z0-9._/():-]+$` and `is_doi_suffix_char` permits
`:` alongside the existing set. No other validation rule changes
(registrant shape, `DOI_SUFFIX_MAX_LEN = 256`, anchored/deterministic
regex, empty-suffix rejection all unchanged).

### Why `:` is safe

- **No new traversal capability.** Path traversal is driven by `/` and
  `..`, **both already permitted** by the pre-existing charset; `:`
  adds nothing a `/`-based attacker did not already have.
- **`safekey` escapes it anyway.** Per `docs/SAFEKEY.md`, the `safekey`
  algorithm independently escapes every character outside
  `[A-Za-z0-9._-]` before any filesystem use, so `:` never reaches a
  path literally regardless of this regex.
- **No URL-encoding hazard.** In a URL path segment `:` is an
  unreserved `pchar` (RFC 3986) — the fetch leg is unaffected.
- **Length still bounded.** `DOI_SUFFIX_MAX_LEN = 256` continues to
  bound the suffix.

This is the strictly-additive option (#194 option 1), preferred over
"document the exclusion + clearer error" (option 2) because the colon
DOIs are legitimate, resolvable, and common in the physics corpora the
tool targets.

## Consequences

**Positive.**

- Legacy Kluwer and the full legacy *Journal de Physique* DOI corpora
  are now fetchable; the dogfood Ising-RG corpus recovers the 3 lost
  niche papers.
- `Doi::parse` is a strict superset of its prior behaviour — every
  previously-accepted DOI is still accepted; no caller (38 call sites)
  changes shape.

**Negative.**

- The accepted-input surface widens by one character. Mitigated above:
  no traversal delta, `safekey` escaping is the real filesystem guard,
  length bound unchanged.
- **Acknowledged `safekey` collision dimension.** `safekey` maps every
  character outside `[A-Za-z0-9._-]` to `_`, and the SHA-256 hash
  disambiguator is appended **only** when the trimmed key exceeds 192
  chars (`crates/doiget-core/src/lib.rs` `Ref::safekey` Step 4). For
  short DOIs (the common case, including the Kluwer / EDP examples
  above), no hash is appended — so any two suffixes whose escaped
  forms collapse identically map to the same safekey. This is a
  **pre-existing** lossy property: `10.X/A/NNN` and `10.X/A_NNN`
  already collided before this ADR (both `/`→`_` and `/` is a legal
  suffix char). Adding `:` to the charset adds `:` to the same
  equivalence class (`A:NNN`, `A/NNN`, `A_NNN` all share one safekey).
  The practical collision rate is near-zero in the real Kluwer / EDP
  corpora — those families specifically use `A:` and `jphys:`, not
  `_`-equivalents — but the property is acknowledged here so the
  next maintainer is not surprised. Closing it (e.g. unconditional
  hash suffix) is a separate ADR with broader scope than #194.

**Out of scope.**

- Removing the `safekey` escape for `:` (rejected — `safekey` is the
  load-bearing filesystem guard and stays maximally conservative
  independent of the parser charset).
- Any other suffix character (e.g. `;`, `<`, space) — not requested by
  a real corpus; revisit per-character via a new ADR if a real DOI
  family needs it.

To revise this decision, write a new ADR with `Supersedes: 0026` and
update this file's `Status:` per `CONTRIBUTING.md`.
