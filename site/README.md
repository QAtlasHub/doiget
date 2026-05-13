# site/

Public-docs Zola site for doiget.

Hosting target: GitHub Pages on `https://sotashimozono.github.io/doiget/`.

## What's in here

- `config.toml` — Zola root config (base_url, title, theme defaults, search index).
- `templates/` — Tera templates (`base.html`, `index.html`, `section.html`, `page.html`). All extend `base.html`. No JavaScript.
- `static/style.css` — minimal stylesheet (serif body, monospace code, dark-mode via `prefers-color-scheme`).
- `content/` — markdown content organized into three reader segments:
  - `content/use/` — End-user docs (CLI install, quickstart, batch, contact, legal).
  - `content/developer/` — MCP server + library integration docs.
  - `content/contribute/` — Architecture, phase plan, ADR index.

## Local build

Install Zola (one-time). The `zola` crate on crates.io is a stub, so
`cargo install zola` does NOT work — use one of these instead:

```sh
# macOS (Homebrew):
brew install zola

# Linux (Snap):
sudo snap install --edge zola

# Any platform — direct binary from GH releases:
ZOLA_VERSION="v0.19.2"
curl -fsSL "https://github.com/getzola/zola/releases/download/${ZOLA_VERSION}/zola-${ZOLA_VERSION}-x86_64-unknown-linux-gnu.tar.gz" \
  | tar -xz -C /usr/local/bin/   # adjust path for your OS
```

Then preview the site locally:

```sh
cd site
zola serve                 # http://127.0.0.1:1111, live reload
```

For a one-shot production-style build:

```sh
cd site
zola build --base-url "https://sotashimozono.github.io/doiget/"
# output lands in site/public/
```

## Syncing from `docs/`

The canonical specs live in the repo's top-level `docs/` directory. To
re-project them into `site/content/` (with TOML front-matter injected):

```sh
bash scripts/sync_docs_to_site.sh
```

This is **idempotent** and **required before committing** any `docs/`
edit — the `site.yml` workflow's projection-check step fails CI if the
committed `site/content/` differs from a fresh projection. See the
script header for the mapping table.

## Deployment

`.github/workflows/site.yml`:

- On every PR touching `site/`, `docs/`, the sync script, or this
  workflow: build the site and upload `site/public/` as an artifact (no
  deploy).
- On push to `main`: build, then publish `site/public/` to the
  `gh-pages` branch via `peaceiris/actions-gh-pages@v4`.

## Custom domain

The `codes.sota-shimozono.com` CNAME is managed at the user-site level,
not at this project-site. To switch this site to a custom domain in the
future:

1. Add a `CNAME` file under `site/static/` with the domain string.
2. Update `base_url` in `site/config.toml` accordingly.
3. Update the `--base-url` flag in `.github/workflows/site.yml`.
4. Configure the DNS A/CNAME record per
   [GitHub's docs](https://docs.github.com/en/pages/configuring-a-custom-domain-for-your-github-pages-site).

## Why Zola?

- Single Rust binary; no Node / npm / Python toolchain in CI.
- Built-in syntax highlighting (Syntect) and search index
  (elasticlunr) — sufficient for a docs site without extra plugins.
- Tera templating is close enough to Jinja2 to be readable on first
  encounter for contributors familiar with Python templating.
- The default look is minimal, matching the team preference for
  "documentation that looks like documentation" rather than a polished
  marketing site.
