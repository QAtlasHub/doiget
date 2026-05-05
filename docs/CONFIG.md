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
| `store.root` | POSIX: `$HOME/papers`. Windows: `%USERPROFILE%\papers`. |
| `cache.root` | POSIX: `$HOME/.cache/doiget`. Windows: `%LOCALAPPDATA%\doiget\cache`. |
| `log.path` | POSIX: `$HOME/.config/doiget/access.log`. Windows: `%APPDATA%\doiget\access.log`. |
| `log.retention_days` | `90` |
| `network.user_agent` | `doiget/<version> (+https://github.com/sotashimozono/doiget)` |
| `network.unpaywall_email` | unset; if unset, Unpaywall calls go to non-polite pool |
| `network.connect_timeout_sec` | `10` |
| `network.read_timeout_sec` | `60` |
| `network.total_timeout_sec` | `300` |
| `output.mode` | `quiet` (see [`ARCHITECTURE.md`](ARCHITECTURE.md) §personas) |
| `output.color` | `auto` (honors `NO_COLOR` env) |
| `output.progress` | `false` |
| `output.emoji` | `false` |

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
user_agent = "doiget/0.1.0 (+https://github.com/sotashimozono/doiget; user=alice@example.org)"
unpaywall_email = "alice@example.org"
connect_timeout_sec = 10
read_timeout_sec = 60
total_timeout_sec = 300

[output]
mode = "human"          # human | json | quiet | mcp
color = "auto"          # auto | always | never
progress = false
emoji = false
```

doiget reads only the keys it knows about. Unknown keys cause a startup warning but do
not fail.

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
| `DOIGET_UNPAYWALL_EMAIL` | `network.unpaywall_email` |
| `DOIGET_MODE` | `output.mode` |
| `NO_COLOR` | Forces `output.color = "never"` (xdg standard). |
| `HTTPS_PROXY` / `HTTP_PROXY` / `NO_PROXY` | reqwest honors these (system standard). |

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
