# `doiget verify` GitHub Action

Check that every DOI / arXiv reference in a bibliography file resolves to
real metadata, as a CI gate. Backed by `doiget verify` — it resolves each
id through Crossref / arXiv **without downloading PDFs** or writing to any
store.

## Usage

```yaml
- uses: actions/checkout@v4
- uses: sotashimozono/doiget/.github/actions/verify@v0
  with:
    path: docs/references.bib
    strict: "true"
    contact-email: you@example.org
```

## Inputs

| Input | Required | Default | Description |
|---|---|---|---|
| `path` | yes | — | Bibliography file to verify (`.bib` / CSL-JSON / plain refs). |
| `format` | no | `auto` | `auto` \| `refs` \| `csl-json` \| `bibtex`. Auto-detected from extension / content. |
| `strict` | no | `"false"` | Fail on unresolved and id-less entries, not just malformed ones. |
| `contact-email` | no | a no-reply address | Polite-pool `mailto` sent to Crossref / Unpaywall. Set your project's address. |

## What fails the job

| Entry status | Default | `strict: "true"` |
|---|---|---|
| `illegal` (malformed id, e.g. typo `1O.1234`, or unparseable file) | ❌ fails | ❌ fails |
| `unresolved` (well-formed id that does not resolve) | ⚠️ warns | ❌ fails |
| `unverifiable` (entry has no DOI / arXiv id) | ⚠️ warns | ❌ fails |
| `valid` | ✅ | ✅ |

`illegal` always fails because a malformed id is a definite source error,
independent of the network. `unresolved` is lenient by default so a
transient network blip does not turn CI red; enable `strict` in a
network-stable lane.

A `[verify]` section in the repo's `config.toml` can set
`on_missing_id = "error"` to fail id-less entries without `strict`, or
`skip` to ignore them. See `docs/CONFIG.md`.

## Output

One JSON-Lines record per entry on stdout:

```json
{"ok":true,"ref":"10.1103/PhysRev.65.117","status":"valid","entry_key":"Onsager1944"}
{"ok":false,"ref":"1O.1234/typo","status":"illegal","entry_key":"bad","error":{"code":"INVALID_REF","message":"…"}}
```

The job exit code is the number of failing entries, capped at 255.
