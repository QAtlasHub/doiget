# 0001 — MCP transport is stdio only

- **Date:** 2026-05-05
- **Status:** Accepted
- **Supersedes:** —
- **Source:** Discussion #4

## Context

The Model Context Protocol supports several transports (stdio, HTTP, SSE, WebSocket).
For an agent-facing companion to BiblioFetch.jl that fetches papers via official
publisher APIs, we need to choose a transport with the right legal-posture and
security-posture properties.

The candidates considered:

- **stdio** — local, single-tenant, no listening socket, integrates trivially with
  Claude Desktop / Cursor / Codex / Claude Code.
- **HTTP / SSE** — network-exposed, multi-tenant by default, requires authentication
  design, container/cloud deployments.
- **WebSocket** — streaming, network-exposed, similar concerns to HTTP.

Adopting any network transport reframes doiget from a "tool the user runs locally" into
a "service that fetches on someone's behalf". That reframing changes who is the
contract party with each upstream publisher and weakens the
[`../LEGAL.md`](../LEGAL.md) tool-neutrality posture.

## Decision

doiget MCP transport is **stdio only**, permanently.

This is recorded as a **Permanent non-goal** in [`../SCOPE.md`](../SCOPE.md) §non-goal 6.
HTTP / SSE / WebSocket transports are **not** TODO items; they are excluded from the
roadmap.

To make this enforceable structurally, not only by documentation:

- No Cargo feature for HTTP transport will be created.
- The `cargo-deny` configuration bans HTTP server crates (`axum`, `actix-web`,
  `warp`, `tide`, `hyper` server) workspace-wide.
- A CI workflow `posture-lint.yml` greps source for imports of those crates and fails
  any PR that introduces them.

## Consequences

### Positive

- doiget remains a strictly local tool. The user is unambiguously the party who holds
  any publisher API key and bears any ToS-compliance responsibility.
- Attack surface is reduced: no listening socket, no auth design needed, no SSRF
  vector, no multi-tenant resource accounting.
- Distribution is simple: one self-contained binary per platform.
- The "stdout is a JSON-RPC frame channel only" invariant is feasible to enforce via
  `clippy::print_stdout = deny` in `doiget-mcp` plus a `tracing-subscriber` writer
  redirect to stderr.

### Negative

- Users who want to share a single doiget instance across multiple machines or
  containers cannot do so directly.
- Cloud-deployed MCP host integrations require the user to bring their own thin
  wrapper.

These costs are accepted as the price of the posture and security properties above.

### Reopening procedure

If a future situation appears to motivate adding a network transport, the path is:

1. New GitHub Discussion titled `[scope-reopening] MCP HTTP transport`.
2. Successful resolution of the five barrier conditions documented in the Discussion #4
   review (legal posture re-review, formal threat model, multi-tenant log
   responsibility design, proactive publisher notification, distribution separation).
3. A new ADR superseding this one.

PRs adding any HTTP server code without the above will be closed.
