+++
title = "BiblioFetch.jl migration"
description = "This document will cover migration scenarios between doiget and"
weight = 130
+++

# Migration

> **Status: INFORMATIVE (placeholder).** Full migration recipes will be filled in during
> Phase 2 once the round-trip test suite (`cross-tool-compat.yml`) passes. The
> binding contract underlying these scenarios is in [`STORE.md`](STORE.md).

This document will cover migration scenarios between doiget and
[BiblioFetch.jl](https://github.com/sotashimozono/BiblioFetch.jl), and between
machines.

## Scenarios (to be expanded)

### 1. BiblioFetch.jl user trying doiget for the first time

```sh
cargo install doiget
doiget info <DOI of an existing entry>      # should read what BiblioFetch wrote
```

Expected outcome: doiget reads the existing TOML metadata and PDF without modification.

### 2. doiget user installing BiblioFetch.jl alongside

Add BiblioFetch.jl to a Julia project and point both tools at the same `~/papers/`.
BiblioFetch.jl will see doiget's `[doiget]` extension table and ignore it.

### 3. Moving a store between hosts

```sh
rsync -av ~/papers/ remote:/home/alice/papers/
```

The store layout is portable. The provenance log is **not** synced — it is per-host.

### 4. Recovering from a corrupted lock file

If a `.toml.lock` file persists after a crash:

```sh
rm ~/papers/.metadata/<safekey>.toml.lock
```

doiget will recreate the lock on the next operation. There is no risk of data loss
because the lock is advisory and the entry's `.toml` itself is written atomically.

### 5. Forward migration on a `schema_version` major bump

A future `schema_version = "2.0"` would require both implementations to ship the new
version before either writes `2.0` files. doiget will provide:

```sh
doiget store migrate --to 2.0
```

at that time. Until then, doiget never writes anything other than `1.x`.

### 6. Removing doiget while keeping the store

The store is just files under `~/papers/`. Uninstalling doiget (`cargo uninstall
doiget`) does not touch the store. BiblioFetch.jl can continue to read and write it.

## Future content

This file will expand to:

- Step-by-step recipes with expected output.
- Failure-mode walkthroughs.
- Cross-platform path handling (Windows ↔ POSIX) notes.
- Troubleshooting `cross-tool-compat.yml` failures.

Until then, see [`STORE.md`](STORE.md) for the binding spec.
