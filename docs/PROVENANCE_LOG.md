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
  "ts":          "2026-05-05T08:30:12.345Z",
  "ts_seq":      1234,
  "event":       "fetch",
  "ref":         "10.1234/example",
  "source":      "unpaywall",
  "result":      "ok",
  "license":     "CC-BY-4.0",
  "size_bytes":  1234567,
  "store_path":  "papers/doi_10.1234_example.pdf",
  "capability":  "oa",
  "session_id":  "01JCKZ7Q...",
  "prev_hash":   "9f86d081884c7d659a2feaa0c55ad015...",
  "this_hash":   "a948904f2f0f479b8f8197694b30184b..."
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
| `prev_hash` | hex SHA-256 | yes (except first row) | Equals previous row's `this_hash`. First row of a fresh log uses literal `"GENESIS"`. |
| `this_hash` | hex SHA-256 | yes | SHA-256 of the canonical-JSON of this row excluding `this_hash`. |

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
