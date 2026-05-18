+++
title = "Cache"
description = "```"
weight = 120
+++

# Cache

> **Status: NORMATIVE.** Defines local cache layout and TTLs.

## 1. Layout

```
~/.cache/doiget/
├── resolver/
│   └── <safekey>.toml       # Crossref / Unpaywall response cache
└── citations/
    └── <safekey>.toml       # citation graph expansion cache
```

Override root via `DOIGET_CACHE_ROOT` env or `[cache] root` in `config.toml`.

## 2. Cache entry format

```toml
schema_version = "1.0"
fetched_at     = "2026-05-05T08:30:12Z"
ttl_seconds    = 604800            # 7 days for resolver, 30*24*3600 for citations
source         = "crossref"

[response]
# raw or normalized response body, source-specific
```

A cache entry is invalid if `now() > fetched_at + ttl_seconds`. Invalid entries are
ignored (treated as cache miss) and replaced on the next fetch.

## 3. TTLs

| Cache | Default TTL | Constant |
|---|---|---|
| `resolver/` | 7 days | `RESOLVER_CACHE_TTL_DAYS = 7` |
| `citations/` | 30 days | `CITATION_CACHE_TTL_DAYS = 30` |

These are library constants, not config. A future ADR could expose them to config if
demand arises.

## 4. Atomic writes

Cache writes follow the same atomic write protocol as the store
(see [`STORE.md`](STORE.md) §5): write to `<safekey>.toml.tmp`, fsync, rename, fsync
parent. No locking is required because cache entries are write-once-then-replace; a
concurrent loser overwrites the other's identical content.

## 5. Cache disablement

- Per-invocation: `--no-cache` flag on relevant subcommands.
- Permanent: `[cache] disabled = true` in `config.toml` or `DOIGET_CACHE_DISABLED=1`.

When disabled, doiget neither reads from nor writes to the cache. Resolver / citation
calls go directly to the source.

## 6. Cache cleanup

```sh
doiget cache prune          # delete expired entries
doiget cache clear          # delete all cache entries
doiget cache size           # report disk usage
```

The cache is safe to delete entirely at any time; doiget will rebuild it lazily.
