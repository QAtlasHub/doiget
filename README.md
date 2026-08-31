# doiget

> A single-binary CLI + stdio MCP server that turns DOIs and arXiv ids into local PDFs through official, OA-first APIs.
> Designed as the **agent-facing companion** to [BiblioFetch.jl](https://github.com/sotashimozono/BiblioFetch.jl).

[![crates.io](https://img.shields.io/crates/v/doiget-core.svg)](https://crates.io/crates/doiget-core)
[![downloads](https://img.shields.io/crates/d/doiget-core.svg)](https://crates.io/crates/doiget-core)
[![MSRV](https://img.shields.io/crates/msrv/doiget-core.svg)](https://crates.io/crates/doiget-core)
[![docs.rs](https://img.shields.io/docsrs/doiget-core)](https://docs.rs/doiget-core)
[![CI](https://github.com/QAtlasHub/doiget/actions/workflows/ci.yml/badge.svg)](https://github.com/QAtlasHub/doiget/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/QAtlasHub/doiget/branch/main/graph/badge.svg)](https://codecov.io/gh/QAtlasHub/doiget)
[![issues](https://img.shields.io/github/issues/QAtlasHub/doiget)](https://github.com/QAtlasHub/doiget/issues)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

[![docs (stable)](https://img.shields.io/badge/docs-stable-blue)](https://codes.sota-shimozono.com/doiget/)
[![docs (dev/next)](https://img.shields.io/badge/docs-dev%20%28next%29-orange)](https://codes.sota-shimozono.com/doiget/dev/)
[![API (docs.rs)](https://img.shields.io/badge/API-docs.rs-blue)](https://docs.rs/doiget-core)

**Docs:** stable = the Zola site (built from `main`); dev = rustdoc built from `next`; API = `docs.rs` (latest published release).

**Status:** Shipping on crates.io (`doiget-core`, `doiget-cli`,
`doiget-mcp`), with sigstore-signed binaries + an SBOM attached to the GitHub
Release. Tier 1 + Tier 2 sources, the stdio MCP server, citation-graph
expansion, and gated TDM sources are all implemented. Releases are cut by a
single signed git tag through the tag-driven pipeline (see
[ADR-0025](docs/DECISIONS/0025-tag-driven-release.md)); `release-plz` was
retired. See [CHANGELOG.md](CHANGELOG.md) for history and
[docs/PHASES.md](docs/PHASES.md) for the phase plan.

## Posture

doiget is a general-purpose automation tool for retrieving papers via official publisher APIs.
By design, doiget only attempts retrieval through:

1. Public Open Access sources (default — Crossref, Unpaywall, arXiv).
2. Credentials the user has personally configured for their own institutional or personal subscriptions (opt-in, compile-time gated).

**doiget does not** work around any access control mechanism, redistribute papers, host
content, operate as a SaaS, or bundle any publisher API keys.

Users are responsible for ensuring they have the right to access the content they request and
for compliance with each source's Terms of Service.

See [docs/LEGAL.md](docs/LEGAL.md) and [docs/SCOPE.md](docs/SCOPE.md).

## Documentation

| Reader | Entry point |
|---|---|
| CLI user | This README, then `doiget --help`, then [docs/CONFIG.md](docs/CONFIG.md) and [docs/ERRORS.md](docs/ERRORS.md) for non-trivial flags / exit codes |
| Agent / MCP host integrator | [docs/MCP_TOOLS.md](docs/MCP_TOOLS.md) + [docs/INTEGRATION/README.md](docs/INTEGRATION/README.md) |
| Library user (Rust) | [docs/PUBLIC_API.md](docs/PUBLIC_API.md) + crates.io rustdoc |
| Contributor | [CONTRIBUTING.md](CONTRIBUTING.md) → [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) → [docs/DECISIONS/](docs/DECISIONS/) |
| Publisher legal team | [docs/LEGAL.md](docs/LEGAL.md) + [CONTACT.md](CONTACT.md) |
| Security researcher | [docs/SECURITY.md](docs/SECURITY.md) + [docs/PROVENANCE_LOG.md](docs/PROVENANCE_LOG.md) + [docs/CAPABILITY.md](docs/CAPABILITY.md) |
| BiblioFetch.jl user | [docs/MIGRATION.md](docs/MIGRATION.md) + [docs/STORE.md](docs/STORE.md) + [docs/SAFEKEY.md](docs/SAFEKEY.md) |

Architecture: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
Threat model: [docs/SECURITY.md](docs/SECURITY.md)
Permanent non-goals: [docs/SCOPE.md](docs/SCOPE.md)
Phase plan: [docs/PHASES.md](docs/PHASES.md)
ADRs: [docs/DECISIONS/](docs/DECISIONS/)

## Installation

doiget ships a single self-contained binary — the Linux build is fully static
(musl), so it runs on old glibc / HPC boxes too. Every channel installs the
**same checksum-verified binary** from the signed GitHub Release.

> **Recommended: use a prebuilt binary (no Rust toolchain, no compiler).** The
> shell / PowerShell installers below download the signed release binary
> directly. `cargo install` (further down) instead *compiles from source* and
> therefore needs a working C/C++ build toolchain — see that section for the
> per-platform requirements.

### Shell installer (Linux / macOS)

```sh
curl -fsSL https://raw.githubusercontent.com/QAtlasHub/doiget/main/scripts/install.sh | sh
```

Installs to `~/.local/bin` (override with `DOIGET_INSTALL_DIR`); pin a version with
`DOIGET_VERSION=0.6.0`. The script verifies the published SHA-256 sidecar before installing.

### PowerShell installer (Windows)

```powershell
irm https://raw.githubusercontent.com/QAtlasHub/doiget/main/scripts/install.ps1 | iex
```

### From crates.io (Rust toolchain — compiles from source)

The published crate is **`doiget-cli`** (it produces the `doiget` binary).
`cargo install doiget` does **not** work — there is no crate by that bare name:

```sh
cargo install doiget-cli   # installs the `doiget` binary
```

Because this **compiles from source**, you need a C/C++ build toolchain (a
linker is mandatory — `cargo install` cannot link a binary without one):

- **Linux:** `gcc`/`clang` + `make` (e.g. `build-essential`).
- **macOS:** Xcode Command Line Tools (`xcode-select --install`).
- **Windows:** either the **Visual Studio Build Tools** with the *"Desktop
  development with C++"* workload (provides `link.exe`), **or** a MinGW-w64
  toolchain used via the GNU target
  (`rustup toolchain install stable-x86_64-pc-windows-gnu` +
  `cargo +stable-x86_64-pc-windows-gnu install doiget-cli`).

If you don't have (or don't want) a build toolchain, use one of the prebuilt
installers above — they need no compiler.

### npm / npx — one line in an agent config

```sh
npx -y doiget-cli serve      # MCP server, no install step
npm install -g doiget-cli    # or put `doiget` on PATH
```

The package is `doiget-cli` and the command it installs is `doiget` — the same
shape as `cargo install doiget-cli`, which is where the name comes from. npm
refuses the unscoped name `doiget` as too similar to the unrelated `giget`, and
matching the crate turned out to be the better answer regardless: one name for
the tool on both registries.

The npm packages carry the same signed release binaries as `optionalDependencies`
— npm resolves the one matching your platform, and there is **no postinstall
download**, so this works under `--ignore-scripts` and through a corporate
registry mirror. `npm view doiget version` tells you what is published; the
packages ship from tagged releases, so a very new commit may be ahead of them.

### Homebrew

```sh
brew tap QAtlasHub/doiget https://github.com/QAtlasHub/doiget
brew install doiget
```

The tap lives in this repository rather than a separate `homebrew-doiget`, which
is why the tap line carries an explicit URL. A dedicated tap repo would shorten
it to `brew tap QAtlasHub/doiget`; the formula would move across unchanged.

`Formula/doiget.rb` installs the **same signed release binary** the GitHub
Release publishes, pinned by the `sha256` from that release's own `.sha256`
asset — the file the shell installer verifies against, so the two channels
cannot disagree about what they installed. It is generated by
`scripts/update-homebrew-formula.sh`, never hand-edited, and CI fails if the
committed formula is not what the generator produces.

The formula tracks the latest **stable** release, so it can trail a very new
tag by one commit; `brew info doiget` shows which version it pins.

### Claude Code plugin

```
/plugin marketplace add QAtlasHub/doiget
/plugin install doiget@doiget
```

Reads `.claude-plugin/` from this repository's default branch. The plugin's
`.mcp.json` runs `npx -y doiget-cli serve`, so it needs **nothing installed
beforehand** — npm fetches the wrapper and the one matching platform binary on
first run. Until this it ran a bare `doiget`, which meant the plugin worked
only for people who had already installed doiget some other way; that is why it
was self-hosted rather than submitted to the Anthropic plugin directory, and
why it is now submittable.

### Channel status

| Channel | Status |
|---|---|
| Shell / PowerShell installer | shipping |
| GitHub Release binaries (signed, SBOM) | shipping |
| `cargo install doiget-cli` | shipping (needs a C linker) |
| `.mcpb` Claude Desktop extension | shipping since 0.8.4 |
| MCP Registry | listed |
| npm / npx | `doiget-cli` (installs the `doiget` command); see below for what is published |
| Claude Code plugin | self-hosted marketplace, as above |
| Homebrew | `Formula/doiget.rb` in this repo; see above for the tap line |
| Nix | `flake.nix` exposes `packages.default` / `packages.doiget`, not only a dev shell. The outputs exist; `nix profile install` has not been exercised |
| `.deb` | **not built** — low value; most Linux users take the binary or Nix |
| Docker | **not planned** — see below |

Docker was ranked second in #501 on the grounds that a container is "the only
architecture the Tier-3 features can legally be used in". That premise does not
hold: ADR-0002 decides that the default published binary contains **no TDM
source code at all**, and an image built from the published binary would ship
`oa-only,citation` like every other prebuilt channel. The architecture the
sentence describes is real, and it is served by building from source with
`--features tdm-<publisher>`, which is not a distribution channel. What remains
is "a shape enterprises can pin and scan" — worth something, but doiget is a
single statically-linked binary, so a container solves no dependency problem
here and the existing `.sha256` plus cosign bundle already give a pinnable,
verifiable artefact.

npm was the one channel whose pipeline was written and whose packages did not
exist. npm Trusted Publishing cannot perform a package's *first* publish — the
setting lives under a package's Settings, and there is no Settings page for a
package that has never been published — so the release job, which carries no
token by design, cannot create them. `scripts/bootstrap-npm.sh` does the
once-only placeholder publish that unblocks it; `CONTRIBUTING.md` has the
runbook.

As of 2026-08-27 the four per-platform packages are published as `0.0.0`
placeholders. npm points `latest` at a package's first publish whatever
`--tag` says, so those placeholders are deprecated — that notice is the only
warning until a release moves `latest` to a real version.

[#247](https://github.com/QAtlasHub/doiget/issues/247) was closed as completed
while four of its five channels did not exist; the remaining ones are tracked in
[#501](https://github.com/QAtlasHub/doiget/issues/501). Every release asset is
cosign-keyless signed (`<asset>.cosign.bundle`) for optional verification.

## Quick start

```sh
# Fetch a paper by DOI (Open Access only by default)
doiget fetch 10.1103/PhysRevLett.130.200601

# Fetch by arXiv id
doiget fetch arXiv:2401.12345

# Batch fetch
doiget batch refs.txt

# Verify a bibliography's references resolve (no PDF download) — CI gate
doiget verify docs/references.bib --strict

# Lint a .bib for structural issues (no network): missing fields,
# blank fields, $$-display-math titles. Read-only and math-aware.
doiget lint docs/references.bib

# Inspect what was fetched
doiget info 10.1103/PhysRevLett.130.200601

# Run as MCP server (stdio)
doiget serve
```

Default features fetch only Open Access PDFs through Crossref / Unpaywall / arXiv.
Institutional TDM access (Springer OA, APS Harvest, Elsevier ScienceDirect TDM) is **not** in
the default published binary; it must be opted in at build time per publisher.
See [docs/SOURCES.md](docs/SOURCES.md).

## Coexistence with BiblioFetch.jl

doiget and BiblioFetch.jl share the same on-disk store format (TOML metadata + PDF files
under a configurable store root). doiget defaults to `./papers` (under the current working
directory; ADR-0036), BiblioFetch.jl to `~/papers/`; point both at the same root (e.g.
`DOIGET_STORE_ROOT=~/papers`) to share one store. The shared schema, locking protocol, and
atomic write contract are
specified in [docs/STORE.md](docs/STORE.md). Reference test vectors for the shared safekey
algorithm are in [docs/SAFEKEY.md](docs/SAFEKEY.md).

| Tool | Best for |
|---|---|
| BiblioFetch.jl | Julia REPL, research vault, citation graph exploration |
| doiget | Agents / MCP hosts, batch operations, scripted pipelines, container deployments |

## License

MIT for the doiget source code and binaries (see [LICENSE](LICENSE)).

The license under which doiget retrieves papers is **separate** and is determined by each
paper's own license, the publisher's API Terms of Service, and the user's own access rights.
doiget does not relicense fetched content.

## Contact

Takedown requests, security disclosures, and other formal correspondence:
[CONTACT.md](CONTACT.md).
