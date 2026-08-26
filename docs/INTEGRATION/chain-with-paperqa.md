# Chaining with paper-qa

> **Status: UNTESTED as a chain.** Both halves are real — doiget writes PDFs to
> a directory, paper-qa reads a directory of PDFs — but nobody has run the
> composition end to end and reported back. Treat the shape below as the
> intended design, not a verified recipe. Last checked 2026-08-26.

[paper-qa](https://github.com/Future-House/paper-qa) does retrieval-augmented
question answering over scientific papers. It handles embedding, retrieval and
answer synthesis; doiget handles getting the papers, legally and with a
provenance trail. Neither needs to know about the other, because the interface
is a directory.

## The shape

```sh
export DOIGET_STORE_ROOT="$HOME/papers"
export DOIGET_CONTACT_EMAIL="you@institution.edu"

# 1. Resolve and fetch a bibliography's references (OA only).
doiget batch refs.bib

# 2. Point paper-qa at the store.
pqa -i "$HOME/papers" ask "what do these papers say about X?"
```

`doiget batch` reports per-reference outcomes; the ones it could not fetch are
named rather than dropped, so the corpus paper-qa sees is one you can account
for.

## Why compose rather than integrate

doiget's job stops at "the bytes are on disk, and here is where they came
from". Embedding and retrieval are a different problem with different
dependencies, and duplicating either inside doiget would widen its network
surface and its supply chain for no gain — see [`../SCOPE.md`](../SCOPE.md).

The same argument means doiget will not ship a paper-qa plugin. The directory
is the integration.

## What doiget gives you that a scraper does not

- **Provenance.** Every fetch is a hash-chained row in the append-only log
  ([`../PROVENANCE_LOG.md`](../PROVENANCE_LOG.md)), so the corpus behind an
  answer is auditable.
- **Licence, per entry.** The sidecar records it, which matters as soon as an
  answer is quoted.
- **A refusal you can read.** A blocked paper produces a named reason and a
  remediation, not a silent gap in the corpus.

## Contributing

If you run this, the useful thing to report is the orchestration script and
any paper-qa-side quirks — open a
[GitHub Discussion](https://github.com/QAtlasHub/doiget/discussions).
