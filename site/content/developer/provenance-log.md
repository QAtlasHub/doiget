+++
title = "Provenance log"
description = "```"
weight = 150
+++

# Provenance log

> **Status: NORMATIVE.** Defines the on-disk audit log format. The log is fail-closed:
> a fetch that cannot be logged MUST NOT proceed.

## 1. Location

```
~/.config/doiget/access.log               # current
~/.config/doiget/access.log.<DATE>.gz     # rotated
```

Override path via `DOIGET_LOG_PATH` env or `[log] path` in `config.toml` (see
[`CONFIG.md`](CONFIG.md)).

## 2. Format

JSON Lines. One JSON object per line, terminated by `\n` (LF). UTF-8. Timezone is UTC
in all timestamps.

## 3. Row schema

```json
{
  "ts":               "2026-05-05T08:30:12.345Z",
  "ts_seq":           1234,
  "event":            "fetch",
  "ref":              "10.1234/example",
  "source":           "unpaywall",
  "result":           "ok",
  "license":          "CC-BY-4.0",
  "size_bytes":       1234567,
  "store_path":       "papers/doi_10.1234_example.pdf",
  "capability":       "oa",
  "session_id":       "01JCKZ7Q...",
  "schema_version":   "v2",
  "canonical_digest": "6b86b273ff34fce19d6b804eff5a3f5747ada4eaa22f1d49c01e52ddb7875b4b",
  "prev_hash":        "9f86d081884c7d659a2feaa0c55ad015...",
  "this_hash":        "a948904f2f0f479b8f8197694b30184b..."
}
```

| Field | Type | Required | Notes |
|---|---|---|---|
| `ts` | RFC3339 UTC, millisecond precision | yes | |
| `ts_seq` | `u64` | yes | Per-session monotonic sequence number. |
| `event` | enum | yes | `session_start`, `capability_resolved`, `resolve`, `fetch`, `store_write`, `session_end` |
| `ref` | string | event-dependent | DOI or arXiv id (validated; no log injection). |
| `source` | enum | event-dependent | `crossref`/`unpaywall`/`arxiv`/`openalex`/`s2`/`doaj`/`tdm-elsevier`/`tdm-aps`/`tdm-springer` |
| `result` | enum | yes | `ok` / `err` / `denied` |
| `license` | string | event=fetch ok | OA license string, or `"unknown"` |
| `size_bytes` | `u64` | event=fetch ok | |
| `store_path` | string | event=fetch ok | Relative to store root. |
| `capability` | enum | yes | `oa` / `metadata` / `tdm-elsevier` / `tdm-aps` / `tdm-springer` |
| `session_id` | ULID (26 chars) | yes | One per process invocation. |
| `schema_version` | string | yes | Always the literal `"v2"` for rows written by current builds (ADR-0024). v1 rows (pre-Slice-4) lack this field; the migration tool in §"Schema migration" below brings them onto the v2 shape. |
| `canonical_digest` | hex SHA-256 (64 lowercase chars) | event-dependent | ADR-0021 §1 canonical-digest of the `(source_type, source_id, resolver_profile, version)` tuple. Present on rows with a `ref` (`fetch` / `resolve` / `store_write`); `null` on session bookend rows. Two fetches of the same DOI through Crossref vs. Unpaywall produce two distinct digests. |
| `prev_hash` | hex SHA-256 | yes (except first row) | Equals previous row's `this_hash`. First row of a fresh log uses literal `"GENESIS"`. |
| `this_hash` | hex SHA-256 | yes | SHA-256 of the canonical-JSON of this row excluding `this_hash`. |

## 3.1 Schema migration (v1 → v2)

> **Status: NORMATIVE.** Implemented per
> [ADR-0024](DECISIONS/0024-canonical-ref-impl.md), which supersedes the
> spec-only posture of [ADR-0021](DECISIONS/0021-canonical-tuple-identity.md).

Pre-Slice-4 logs are **v1**: rows have neither a `schema_version`
field nor a `canonical_digest` field, and `#[serde(deny_unknown_fields)]`
plus the non-defaulted `schema_version` mean v1 rows fail to parse
under v2 with a clear `corrupted log at line N` error. Operators MUST
migrate before the v2 binary will read their existing log.

The migration is exposed via the CLI as
`doiget provenance migrate [--dry-run]` and via the library as
`doiget_core::provenance::migrate_v1_to_v2(log_path, dry_run)`.

**Algorithm.**

1. Read all v1 rows.
2. For each row with a `ref`: derive `canonical_digest` by promoting
   `(source_type from ref shape, source_id = ref, resolver_profile = source, version = None)`
   through [ADR-0021](DECISIONS/0021-canonical-tuple-identity.md) §1.
   Rows without a `ref` (session bookends) keep `canonical_digest = null`.
3. Recompute the SHA-256 hash chain across the new row payloads — the
   v1 chain is invalidated by the schema change (the v2
   canonical-JSON includes the two new fields, so old `this_hash`
   values no longer match).
4. **Idempotency**: if the input file already contains v2 rows, the
   migrator re-parses them via a v2 fallback and produces
   byte-equivalent output. Re-running on a v2 log is a no-op.
5. **Dry-run**: `--dry-run` returns a `MigrationReport` summarizing
   `rows_rewritten`, the first-row v1 chain anchor, and the first-row
   v2 chain anchor without touching disk.
6. **Live**: on success, the original is preserved at
   `<log_path>.v1-backup` and the migrated v2 log is atomically
   renamed onto `<log_path>` from a staged `<log_path>.v2-migrated`.
   The staged file MUST pass `verify()` before the rename; failure
   aborts the migration without touching the live log.

Test vectors: synthetic v1 fixture at
`tests/fixtures/provenance/migration_v1_to_v2.json`; the migration
end-to-end suite lives at
`crates/doiget-core/tests/provenance_migration_e2e.rs`.

## 4. Hash chain

Canonical JSON = compact (no whitespace), keys sorted lexicographically, no trailing
whitespace.

```
this_hash = hex(SHA256(canonical_json(row \ {this_hash})))
prev_hash[N] = this_hash[N-1]  (chain)
```

Tampering is detected by `doiget audit-log --verify`, which recomputes every row's
`this_hash` and validates the chain.

## 5. Failure mode (fail-closed at the operation boundary)

If the log writer cannot append a row (disk full, permission denied, fsync error),
the caller MUST receive `Err(LogError)` and the surrounding fetch MUST NOT proceed.
The user can recover by clearing the obstruction (free disk, fix permissions) and
retrying — `LogError` is classified as recoverable in
[`ERRORS.md`](ERRORS.md) §2.

This makes the log a **best-effort tamper-evident local audit record**, not a
cryptographic enforcement mechanism: a determined local attacker with write access
to `~/.config/doiget/` can still modify or replay the log (see §8 below). The
fail-closed behavior closes the easiest evasion path (silently dropping rows
without the user noticing), but the legal posture in
[`LEGAL.md`](LEGAL.md) does not depend on the log being unforgeable. The log is
documentary evidence of intent and operation, evaluated as such.

If the log directory is missing at startup, doiget creates it (`mkdir -p`, `0700`
on POSIX) and writes a `session_start` row.

## 6. Rotation and retention

- A log file rotates when its size exceeds **100 MB**: it is gzip-compressed and renamed
  to `access.log.<YYYY-MM-DD-HHMMSS>.gz`. A new `access.log` opens.
- The new file's first row uses `prev_hash = "GENESIS"` (chain restart).
- Files older than **90 days** (default) are auto-deleted at startup. Configurable via
  `DOIGET_LOG_RETENTION_DAYS=N`. `N=0` disables auto-deletion.

## 7. Audit tool

```sh
doiget audit-log --verify
doiget audit-log --since 2026-01-01
doiget audit-log --source elsevier
doiget audit-log --session 01JCKZ7Q...
```

`--verify` recomputes the hash chain and reports any mismatches.

## 8. Tamper resistance

doiget makes a best-effort attempt to set the log file append-only on Linux:

- `chattr +a access.log` (ignored if not root or unsupported).
- On macOS / Windows: no equivalent generally available; rely on hash chain detection.
- A determined local attacker with write access to `~/.config/doiget/` can still rewrite
  history. The hash chain ensures rewrites are detectable, not impossible.

## 9. Privacy

- The log is **local only**. doiget does not transmit log contents anywhere.
- The log contains DOIs/arXiv ids the user has fetched. Users who consider that
  sensitive should use file-system encryption (e.g., per-user disk encryption) at the OS
  level.
- API keys are NEVER logged in any field. The capability resolution log row records
  which env var name granted access (e.g., `agree_env_var: "DOIGET_AGREE_TDM_ELSEVIER"`)
  but never the key value.
