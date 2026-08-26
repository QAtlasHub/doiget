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

> **The file does not exist by default.** A fresh install has no
> `config.toml` at all, and three of the four settings that most often decide the
> outcome of a session — store location, the two allowlist flags — fail *silently*
> when it is absent. Run **`doiget config init`** to write a fully commented
> template to the path `doiget config path` reports; every line is commented out,
> so it documents the choices without changing behaviour until you edit it.
> `--force` overwrites an existing file (without it, `init` refuses, so it can
> never silently discard a hand-written allowlist).


```toml
# ~/.config/doiget/config.toml — all fields optional

[store]
# Overridden by DOIGET_STORE_ROOT and --store-root (one rung above).
# A leading `~` is expanded; the env var relies on the shell for that.
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
trust_oa_registries  = false   # true = also allow the curated OA registries (SciELO, Zenodo, ...)

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
| `trust_academic_repos` | bool | `false` | Adds 15 curated single-suffix academic wildcards — where institutions host their own **Green OA**. |
| `trust_oa_registries` | bool | `false` | Adds the curated **OA registries / repositories** — where cross-publisher **Gold OA** is indexed or hosted. |
| `[[network.additional_hosts]]` | array of tables | empty | Adds individual hosts. `host` is required; `note` is optional and free-text. |

**Prefer the registrable-domain wildcard.** A publisher often redirects across its own
subdomains — `www.ams.org` → `pubs.ams.org` — so an entry for the exact host that was
refused buys you one hop and another denial. `*.ams.org` covers the publisher in one
line, and matches how the built-in list is written (ADR-0027 bounds the trusted surface
to registrable-domain wildcards for established publishers). Add the apex alongside it
when the publisher redirects there too: a single-suffix wildcard does **not** match
`ams.org` itself, which is why the built-in list carries both forms for `doaj.org`,
`arxiv.org` and `europepmc.org`.

The denial message suggests all three, most specific first (#443).

The two flags are separate because the trust arguments differ — "this institution
publishes its own work here" is not "this registry indexes open content across
publishers" — so you can take either without the other.

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

`trust_oa_registries = true` activates the OA-registry set:

```
scielo.org    *.scielo.org    *.scielo.br
zenodo.org    *.zenodo.org    osf.io        *.osf.io
hal.science   *.hal.science   core.ac.uk
```

Every entry is a registry or repository whose *purpose* is open distribution, never a
publisher platform — turning this on must not become a way to reach paywalled content.
Note that both the apex and the wildcard are listed where the apex serves content: a
single-suffix wildcard does **not** match the apex.

**DOAJ is not in this set — it needs no flag.** `doaj.org` is on the *default*
allowlist as of 0.8.8 (ADR-0037), because the project already trusted it under the
`doaj` metadata source key and the two keys simply disagreed. So a Gold-OA article
routed through DOAJ — e.g. IEEE Access `10.1109/access.2024.3495502` — works on a
stock install with no configuration at all.

`[[network.additional_hosts]]` is for anything outside either set. A pattern is either a
literal FQDN (`ruj.uj.edu.pl`) or a **single-suffix wildcard** (`*.uj.edu.pl`). Multi-segment
globs (`*.edu.*`), a bare `*`, and a misplaced `*` (`foo.*.org`) are rejected at load time —
this table uses `deny_unknown_fields`, so a typo such as `hsot = "..."` fails loudly rather
than being silently ignored.

```toml
[[network.additional_hosts]]
host = "*.uj.edu.pl"
note = "Jagiellonian University repository"

[[network.additional_hosts]]
host = "repository.example.edu"
note = "a repository outside both curated sets"
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
| `--force` | `config init` only: overwrite an existing `config.toml` |
| `--network` | `config doctor` only: also run the outbound report (§6.1) |

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

# Requires a build with `--features tdm-ieee`. The endpoint and response
# shape are INFERRED from IEEE's public developer portal, not confirmed
# against a live programme key (#430) — a response in another shape is
# reported as a schema error naming the field and quoting the body,
# rather than silently returning nothing.
[tdm.ieee]
api_key = "..."
agreed = true
```

If both env var and credentials.toml provide the same key, env var wins.

## 6.1 Institutional networks: what works and what does not

Being on a subscribing university network does **not** make paywalled content
fetchable. Three things sit in the way, in this order — and until 0.8.11 this section
listed only the last two, which meant it sent readers after fixes that could not help.

0. **Nothing is attempted at all**, and this comes first. The fetch path carries only
   OA locations an enabled source reported. For a closed work there are none, so the
   leg ends before any host is chosen: not refused, **never attempted**. The run exits
   **0** with `metadata-only: no OA PDF available`, which reads as "this paper has no
   OA copy".

   Both blockers below sit behind this one and are never reached, so widening the
   allowlist or obtaining TDM credentials does not change the outcome for a closed
   DOI. Until 0.8.11 this section listed only (1) and (2), and therefore sent readers
   after two fixes that could not help.

   **This is not a bug awaiting a fix.** #517 asked whether Crossref's publisher link
   could fill the gap. Measured across six live DOIs and eight captured responses,
   every `link[]` entry Crossref returns is scoped to Similarity Check, syndication or
   a TDM programme, and none is general-purpose - so following one would mean using a
   licensed route without the licence. See [`LEGAL.md`](LEGAL.md) §2a(a-i) and
   ADR-0052. **The supported route for a closed work is a TDM credential (§6)**, which
   is the licensed version of exactly that.

1. **The allowlist.** IEEE, ACM, SIAM and AMS are not on the default `oa-publisher`
   allowlist, so the attempt is refused at the redirect policy with
   `error[CAPABILITY_DENIED] ... redirect_not_in_allowlist` naming the host. IEEE has
   a TDM route instead (`[tdm.ieee]` above, `--features tdm-ieee`) — a different host
   (`ieeexploreapi.ieee.org`, the API) under a different source key, not a widening of
   `oa-publisher` (ADR-0039).
2. **The publisher's bot wall.** Even from a subscribing address, a scripted client
   commonly gets `202 Accepted` with an **empty body** — a challenge holding response,
   not a paywall and not a 403. The subscription is not the binding constraint; being a
   program is.

So widening the allowlist alone would not fix such a fetch; it would move the failure
one step later. That is now demonstrable rather than asserted: the request is formed
and refused by name, so you can see which of (1) and (2) you are hitting.
`HTTPS_PROXY` is honoured (§4), but a proxy fixes *addressing*, never the bot wall —
and if you are already on the subscribing network, tunnelling elsewhere routes you
away from your entitlement.

**Where this does work.** For a closed work whose publisher **is** on the
`oa-publisher` allowlist — the physics societies and diamond-OA hosts of ADR-0027, or
a host you added yourself via `[[network.additional_hosts]]` / `trust_academic_repos`
/ `trust_oa_registries` — the attempt now goes out, and on an entitled network it can
succeed. That is not circumvention: the URL is the one Crossref reports, the request
carries no credential doiget invented, and any access control on the far side applies
unchanged.

The two routes that do work:

- **Per-publisher TDM credentials** (§6). This is the interface publishers intend
  programs to use, and it sidesteps the WAF by not being the web front end.
- **A real browser** on the subscribing network.

`doiget config doctor --network` reports which of your publishers will actually talk to
this client:

```
$ doiget config doctor --network
network (--network):
  egress          not probed (needs a third-party echo service; try `curl ifconfig.me`)
                  a proxy fixes addressing, never a bot wall
  unpaywall       polite pool as you@institution.edu
  oa-publisher    22 host patterns allowlisted
  probe link.springer.com      200 3100 bytes    ok
  probe arxiv.org              200 5854 bytes    ok
  probe ieeexplore.ieee.org    not allowlisted   no request sent; ...
  probe doaj.org               200 793 bytes     ok
```

The flag is opt-in because it makes real outbound requests: one GET per listed host, no
retries, and only against hosts already on the allowlist — the probe enforces the same
allowlist a fetch would, so it cannot be pointed at an arbitrary host. Hosts that are
**not** allowlisted are still listed, reported as `not allowlisted` with no request
sent; that line is usually the answer.

`202` with an empty body is called out as a bot challenge rather than reported as a
success, because a status code alone cannot distinguish the two.

## 7. Inspecting effective config

```sh
doiget config show          # prints the resolved config (with API keys redacted as ****)
doiget config path          # prints the path of the config file in use
doiget config doctor        # checks file permissions, reachability, sanity
```
