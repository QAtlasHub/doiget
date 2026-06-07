# Store layout

> **Status: NORMATIVE (shared spec).** This document is binding for both doiget and
> BiblioFetch.jl. Implementations on either side MUST conform. Changes require an ADR
> coordinated across both projects.

## 1. Layout

```
~/papers/
├── <safekey>.pdf                    # PDF blob (immutable after write)
└── .metadata/
    ├── <safekey>.toml               # metadata
    └── <safekey>.toml.lock          # advisory lock file (see §3)
```

Store root path is configurable per [`CONFIG.md`](CONFIG.md). Default: `~/papers/`.
The `<safekey>` is computed from the DOI / arXiv id by the algorithm in
[`SAFEKEY.md`](SAFEKEY.md).

## 2. Schema (TOML)

```toml
schema_version = "1.0"

# --- Reserved top-level fields (shared with BiblioFetch.jl) ---
title       = "Example Paper Title"
authors     = ["Alice Researcher", "Bob Coauthor"]
year        = 2026
doi         = "10.1234/example"
arxiv_id    = "2401.12345"           # optional
abstract    = "..."                   # optional
venue       = "Phys. Rev. X"          # optional
volume      = "12"                    # optional
issue       = "3"                     # optional (BibTeX `number`)
pages       = "031001"                # optional (e.g. "477--528")
publisher   = "American Physical Society"   # optional
issn        = "2160-3308"             # optional
isbn        = ""                      # optional, for books
type        = "journal-article"       # crossref taxonomy
keywords    = ["physics", "..."]      # optional

# --- doiget extension table (BiblioFetch.jl ignores these) ---
[doiget]
fetched_at  = "2026-05-05T08:30:12Z"  # RFC3339 UTC
source      = "unpaywall"             # which Source produced this entry
license     = "CC-BY-4.0"             # OA license string, or "unknown"
oa_status   = "gold"                  # optional: gold/green/hybrid/bronze/closed (Unpaywall)
                                      #   or "green" (arXiv); omitted when not determined (#281)
size_bytes  = 1234567
mcp_call_id = "01JCKZ7Q..."           # optional, ULID, present if fetched via MCP
```

### Reserved top-level field list

```
schema_version, title, authors, year, doi, arxiv_id, abstract, venue, volume,
issue, pages, keywords, type, publisher, issn, isbn, url, pdf_path
```

Both implementations MUST NOT write top-level fields outside this list. Tool-specific
fields go in a tool-named table (`[doiget]`, `[bibliofetch]`).

## 3. Schema versioning

`schema_version` is a string of the form `<MAJOR>.<MINOR>`.

- **Minor bump** (`1.0` → `1.1`): backward-compatible additions only (new optional fields,
  new tables). Implementations on `1.x` are required to read `1.x` files for any
  `x ≤ self.MINOR`. They MUST treat unknown fields as opaque (not delete, not error).
- **Major bump** (`1.x` → `2.0`): breaking changes (renamed or removed reserved fields).
  Implementations not yet on `2.x` MUST refuse to write to `2.x` files (read-only mode,
  warn).

When an implementation encounters a file with `schema_version` greater than its own, it
enters **read-only mode** for that file: reads return `Ok(Some(metadata))` after a warn,
writes return `Err(StoreError::SchemaTooNew { theirs, ours })`.

## 4. Concurrent access (Contract: lock protocol)

Both implementations MUST use file-system advisory locking on the **separate lock file**
`<safekey>.toml.lock` for the duration of any read-modify-write or write sequence.

- POSIX: `flock(..., LOCK_EX)` (Rust: `fs2::FileExt::lock_exclusive`).
- Windows: `LockFileEx(..., LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY)`.

Lock acquisition timeout: **5 seconds**. On timeout, return `StoreError::LockTimeout`.

Locks are advisory; a third-party process that ignores the lock can corrupt the store.
Both doiget and BiblioFetch.jl honor the lock. Anyone integrating a third tool with
`~/papers/` is expected to follow the same contract.

The lock file itself is created on demand (`O_CREAT | O_RDWR`) and is never deleted
during normal operation. It may be safely deleted when no process holds it.

## 5. Atomic write (Contract)

Every metadata write MUST follow this sequence:

```text
1. Open <safekey>.toml.tmp with O_CREAT | O_TRUNC | O_WRONLY (POSIX)
   or CREATE_ALWAYS, GENERIC_WRITE  (Windows).
2. Write the full serialized TOML.
3. fsync(<safekey>.toml.tmp).
4. rename(<safekey>.toml.tmp, <safekey>.toml).
5. fsync(parent directory).
```

On Windows, step 4 is `MoveFileEx(.., MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)`.
Rust's `std::fs::rename` performs this on Windows.

PDF writes follow the same pattern: write to `<safekey>.pdf.tmp`, fsync, rename, fsync
parent.

A crash mid-write leaves either:

- The old file intact (if crash before step 4), or
- The new file fully written and visible (if crash after step 4).

It never leaves a partially-visible new file. The lone `.tmp` artifact may be reaped at
startup.

**Cross-file ordering (issue #122).** Each file is atomic *individually*; there is no
cross-file transaction across the metadata TOML and its PDF. The writer therefore renames
the **PDF first**, then the metadata that references it. Consequences of a crash *between*
the two renames:

- before the PDF rename → the previous consistent entry (or no entry) — unchanged;
- after the PDF rename, before the metadata rename → an **orphan `<safekey>.pdf`** plus
  the prior/absent metadata. `list_recent` / `search` key off metadata and ignore the
  orphan; a subsequent re-fetch overwrites it.

What is **guaranteed not to happen**: metadata becoming visible while its `pdf_path`
points at a `.pdf` that is absent or stale. A full two-file transaction is out of scope
for the MVP; this ordering is the bounded guarantee.

## 6. doiget-side write discipline

When doiget writes to a `<safekey>.toml` that already exists (e.g., a re-fetch):

- doiget MUST NOT modify reserved top-level fields written by BiblioFetch.jl. doiget may
  upgrade a missing field, but never overwrite an existing one.
- doiget writes only inside the `[doiget]` table for state it owns (`fetched_at`,
  `source`, `license`, etc.).
- Exception: `schema_version` may be bumped on a coordinated minor revision.

This rule prevents a user who runs both BiblioFetch.jl and doiget against the same store
from losing BiblioFetch-authored fields. Unknown keys inside `other` (e.g. an unknown
top-level scalar or a `[bibliofetch]` sub-key) follow the same rule: on a re-write, an
existing on-disk value WINS over whatever doiget carries (issue #123).

> **Re-fetch downgrade behaviour (issue #123).** A doiget re-fetch of an entry that
> previously had a PDF but is now metadata-only (e.g. the OA host went off-allowlist)
> rewrites the `[doiget]` table (`source`, `size_bytes`, …) in place. The PDF blob is
> immutable-after-write (§1) so the existing `.pdf` file is **not** deleted, but the
> entry's recorded state changes. This is intentional, not silent: as of issue #118 the
> blocked-PDF reason is surfaced to the caller (CLI `note:` line / MCP `pdf.status`),
> so the operator always learns the entry was downgraded and why. A guard that refuses
> to downgrade is deferred (post-MVP) — it is a policy choice, not a correctness bug.

## 7. TOML normalization

To make `bib` / `csl` / TOML output diff-stable across implementations:

- Keys at the top level appear in this order: `schema_version` first, then reserved fields
  alphabetically.
- Tool tables (`[doiget]`, `[bibliofetch]`) appear after all top-level fields, in
  alphabetical order of table name. Within a table, keys are alphabetical.
- String quoting: ASCII-safe single-line strings use `"..."`. Multi-line strings use
  `"""..."""`.
- Line endings: LF (`\n`) only. No CR.
- Trailing newline at end of file.

A reference normalizer is provided by `doiget-core::store::normalize_toml(&Metadata)
-> String`. CI uses it to detect drift.

## 8. Reading

Both implementations MUST tolerate:

- Unknown top-level fields (warn once, ignore).
- Unknown tables (ignore silently).
- Future minor `schema_version` (degrade to read-only with warn).
- Missing optional fields (`arxiv_id`, `abstract`, `venue`, `[doiget]`, etc.).

Both implementations MUST refuse:

- Missing `schema_version`, `title`, `authors`, or year-equivalent.
- Future major `schema_version` for write operations.

> **Known doiget limitation (issue #123).** doiget reads any unknown table
> into an opaque `other` map and preserves *flat* unknown tables (e.g.
> `[bibliofetch]` with scalar/array sub-keys) losslessly across a
> read→write→read cycle (proven by
> `bibliofetch_typed_table_and_unknown_scalar_survive_roundtrip`). It does
> **not** yet round-trip a *nested* unknown sub-table such as
> `[bibliofetch.history]`: doiget's TOML normalizer rejects a nested table
> inside `other`, so a doiget rewrite of an entry containing one returns
> `StoreError::Serialize` rather than silently dropping data (fail-loud,
> not data loss). BiblioFetch.jl should keep its tool table flat until
> this is lifted; tracked for a post-MVP normalizer pass.

## 9. Round-trip CI test

A CI workflow (`cross-tool-compat.yml`) exercises this every PR:

```text
1. Julia: BiblioFetch fetch DOI X            (creates <safekey>.toml + .pdf)
2. doiget info X                             (reads, asserts metadata matches expected)
3. doiget bib X | diff - expected_bibtex     (asserts bib output is bit-identical)
4. doiget fetch DOI Y                        (writes a different entry)
5. Julia: BiblioFetch info Y                 (reads doiget output, must succeed)
```

This guarantees real round-trip compatibility, not just spec conformance.

## 10. Migration story

See [`MIGRATION.md`](MIGRATION.md) for end-user migration scenarios:

- A BiblioFetch.jl user trying doiget for the first time.
- A doiget user installing BiblioFetch.jl alongside.
- Moving a store between hosts.
- Recovering from a corrupted lock file.
