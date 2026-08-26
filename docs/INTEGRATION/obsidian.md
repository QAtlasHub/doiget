# Obsidian

> **Status: NOT IMPLEMENTED, and this page says what does work instead.**
> There is no Obsidian backend export and none is in progress. The
> "Phase 7 (optional)" marker in the planned-files table is the only
> commitment, and it is not a schedule. Last checked 2026-08-26.

Obsidian does not host MCP servers, so there is nothing to register. What
exists today is that doiget's store is **plain files**, so a vault can sit on
top of it with no integration at all.

## What doiget writes

One TOML sidecar and one PDF per entry, under `DOIGET_STORE_ROOT`. The layout
and the field contract are in [`../STORE.md`](../STORE.md).

Point the store at a folder inside your vault:

```sh
export DOIGET_STORE_ROOT="$HOME/Vault/papers"
doiget fetch 10.1103/PhysRevLett.130.200601
```

The PDFs are then openable from Obsidian, and the sidecars are readable text.
They are **TOML, not Markdown with YAML frontmatter**, so Obsidian will not
index them as notes and Dataview will not see them.

## What is missing, precisely

A Markdown-with-frontmatter projection of the sidecar. That is the whole of
the "Obsidian backend export" idea, and it is not implemented — not the wiring,
not the frontmatter schema, not a `doiget` subcommand.

`doiget csl <ref>` and `doiget bib <ref>` emit CSL-JSON and BibTeX from the
same records, which is enough to script a projection yourself.

## Contributing

If you build one, open a
[GitHub Discussion](https://github.com/QAtlasHub/doiget/discussions) with the
vault layout and the frontmatter shape before a PR — the schema is the part
worth agreeing on first.
