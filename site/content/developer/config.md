+++
title = "Configuration"
description = "```"
weight = 130
+++

# Configuration

> **Status: NORMATIVE.** Defines configuration sources and their precedence.

## 1. Sources, in priority order

```
1. CLI flags                        (highest)
2. Environment variables
3. User config file ~/.config/doiget/config.toml
4. Built-in defaults                (lowest)
```

A value set higher in the chain overrides any value set lower.

## 2. Built-in defaults

| Key | Default |
|---|---|
| `store.root` | `./papers` — `papers/` under the current working directory (ADR-0036). Set `DOIGET_STORE_ROOT` for a central library. |
| `cache.root` | POSIX: `$HOME/.cache/doiget`. Windows: `%LOCALAPPDATA%\doiget\cache`. |
| `log.path` | POSIX: `$HOME/.config/doiget/access.log`. Windows: `%APPDATA%\doiget\access.log`. |
| `log.retention_days` | `90` |
| `network.user_agent` | `doiget/<version> (+https://github.com/QAtlasHub/doiget)` |
| `network.unpaywall_email` | unset; if unset, Unpaywall calls go to non-polite pool |
| `network.connect_timeout_sec` | `10` |
| `network.read_timeout_sec` | `60` |
| `network.total_timeout_sec` | `300` |
| `output.mode` | `quiet` (see [`ARCHITECTURE.md`](ARCHITECTURE.md) §personas) |
| `output.color` | `auto` (honors `NO_COLOR` env) |
| `output.progress` | `false` |
| `output.emoji` | `false` |
| `verify.on_missing_id` | `warn` |
| `verify.strict` | `false` |

## 3. config.toml schema

```toml
# ~/.config/doiget/config.toml — all fields optional

[store]
root = "/home/alice/papers"

[cache]
root = "/home/alice/.cache/doiget"

[log]
path = "/home/alice/.config/doiget/access.log"
retention_days = 90

[network]
user_agent = "doiget/0.1.0 (+https://github.com/QAtlasHub/doiget; user=alice@example.org)"
unpaywall_email = "alice@example.org"
connect_timeout_sec = 10
read_timeout_sec = 60
total_timeout_sec = 300

# Allowlist extension — these two keys decide whether a fetch is ALLOWED.
# Without them, an OA PDF hosted off the built-in allowlist is denied with
# `error[CAPABILITY_DENIED]: ... redirect_not_in_allowlist`.
trust_academic_repos = false   # true = also allow the 15 curated academic suffixes below

[[network.additional_hosts]]
host = "*.uj.edu.pl"           # single-suffix wildcard, or a literal FQDN
note = "Jagiellonian University repository"

[output]
mode = "human"          # human | json | quiet | mcp
color = "auto"          # auto | always | never
progress = false
emoji = false

[verify]                 # consumed by `doiget verify`
on_missing_id = "warn"   # warn | error | skip — policy for id-less entries
strict = false           # also fail on unreachable (transient) ids; absent (404/410) ids fail regardless
```

doiget reads only the keys it knows about. Unknown keys cause a startup warning but do
not fail.

### 3.1 Allowlist extension — `trust_academic_repos` / `[[network.additional_hosts]]`

doiget only fetches from hosts on its allowlist, and a redirect to an off-allowlist host is
denied:

```
error[CAPABILITY_DENIED]: an OA PDF was found but its host is blocked by supply-chain
policy (redirect_not_in_allowlist): redirect target strathprints.strath.ac.uk not in
allowlist for source oa-publisher
```

The two keys below are the supported way to widen it. Both live under `[network]`, and
neither is set by default — a fresh install has no `config.toml` at all, so every fetch
runs against the built-in allowlist only.

| Key | Type | Default | Effect |
|---|---|---|---|
| `trust_academic_repos` | bool | `false` | Adds 15 curated single-suffix academic wildcards to the allowlist. |
| `[[network.additional_hosts]]` | array of tables | empty | Adds individual hosts. `host` is required; `note` is optional and free-text. |

`trust_academic_repos = true` activates exactly these patterns, which cover the national
registration blocks institutions use for Green-OA repositories:

```
*.ac.uk   *.ac.jp   *.jst.go.jp   *.edu.au   *.edu.cn
*.ac.cn   *.edu.pl  *.ac.nz       *.ac.za    *.ac.in
*.edu.br  *.edu.tw  *.edu.tr      *.edu.ar   *.edu.mx
```

So the denial above is fixed by one line — `strathprints.strath.ac.uk` is `.ac.uk`:

```toml
[network]
trust_academic_repos = true
```

`[[network.additional_hosts]]` is for anything outside that set. A pattern is either a
literal FQDN (`ruj.uj.edu.pl`) or a **single-suffix wildcard** (`*.uj.edu.pl`). Multi-segment
globs (`*.edu.*`), a bare `*`, and a misplaced `*` (`foo.*.org`) are rejected at load time —
this table uses `deny_unknown_fields`, so a typo such as `hsot = "..."` fails loudly rather
than being silently ignored.

```toml
[[network.additional_hosts]]
host = "*.uj.edu.pl"
note = "Jagiellonian University repository"

[[network.additional_hosts]]
host = "doaj.org"
note = "DOAJ — common redirect target for gold-OA journal content"
```

Run `doiget config doctor` to confirm what was loaded:

```
[ ok ] user-extension hosts loaded: 2 (trust_academic_repos=true)
```

## 4. Environment variables

All `DOIGET_*` env vars use `SCREAMING_SNAKE_CASE`. Boolean env vars accept `1` / `0`,
`true` / `false`. Path env vars take a single absolute path.

| Variable | Maps to |
|---|---|
| `DOIGET_STORE_ROOT` | `store.root` |
| `DOIGET_CACHE_ROOT` | `cache.root` |
| `DOIGET_LOG_PATH` | `log.path` |
| `DOIGET_LOG_RETENTION_DAYS` | `log.retention_days` |
| `DOIGET_USER_AGENT` | `network.user_agent` |
| `DOIGET_CONTACT_EMAIL` | Polite-pool contact address; also the default for `DOIGET_UNPAYWALL_EMAIL`. |
| `DOIGET_UNPAYWALL_EMAIL` | `network.unpaywall_email` |
| `DOIGET_MODE` | `output.mode` |
| `NO_COLOR` | Forces `output.color = "never"` (xdg standard). |
| `HTTPS_PROXY` / `HTTP_PROXY` / `NO_PROXY` | reqwest honors these (system standard). |

With neither email set, doiget still queries Unpaywall — it sends the placeholder
`doiget@localhost`, which lands in the non-polite pool and may be rate-limited or refused.
Setting `DOIGET_CONTACT_EMAIL` is therefore worth doing before any batch run, and it is what
`doiget config doctor` flags. It also makes the automatic arXiv preprint fallback reliable:
when a DOI's OA PDF is blocked, doiget retries via the arXiv preprint that Unpaywall named,
so a throttled Unpaywall response costs you that fallback too.

CapabilityProfile-related env vars are documented in [`CAPABILITY.md`](CAPABILITY.md).

## 5. CLI flags

CLI flags use `--kebab-case`. Any path-or-string value with a config equivalent is
overridable by flag:

| Flag | Maps to |
|---|---|
| `--store-root <path>` | `store.root` |
| `--log-path <path>` | `log.path` |
| `--mode <human\|json\|quiet\|mcp>` | `output.mode` |
| `--color <auto\|always\|never>` | `output.color` |
| `--progress` / `--no-progress` | `output.progress` |
| `--quiet` / `-q` | implies `--mode=quiet` |
| `--json` | implies `--mode=json` |

`doiget serve` always runs in `mcp` mode regardless of flags. Other subcommands honor the
mode resolution above.

## 6. Credentials file

```toml
# ~/.config/doiget/credentials.toml — optional, alternative to env vars
# Permissions MUST be 0600 on POSIX; doiget warns at startup otherwise.

[tdm.elsevier]
api_key = "..."
agreed = true

[tdm.aps]
api_key = "..."
agreed = true

[tdm.springer]
api_key = "..."
agreed = true
```

If both env var and credentials.toml provide the same key, env var wins.

## 7. Inspecting effective config

```sh
doiget config show          # prints the resolved config (with API keys redacted as ****)
doiget config path          # prints the path of the config file in use
doiget config doctor        # checks file permissions, reachability, sanity
```
