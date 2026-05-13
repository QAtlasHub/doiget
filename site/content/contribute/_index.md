+++
title = "Contribute"
description = "Architecture overview, phase plan, and ADRs for contributors."
sort_by = "weight"
weight = 30
+++

This section is for people who want to **send a pull request** or
otherwise contribute changes to doiget. If you want to use doiget, see
[Use]({{ get_url(path='@/use/_index.md') | safe }}); if you want to embed
doiget, see [Developer]({{ get_url(path='@/developer/_index.md') | safe }}).

## Start here

1. Read [CONTRIBUTING.md]({{ config.extra.github_url }}/blob/main/CONTRIBUTING.md) for the workflow.
2. Skim [docs/ARCHITECTURE.md]({{ config.extra.github_url }}/blob/main/docs/ARCHITECTURE.md) to understand the workspace layout (`doiget-core` / `doiget-cli` / `doiget-mcp`).
3. Check [docs/PHASES.md]({{ config.extra.github_url }}/blob/main/docs/PHASES.md) to see which phase the project is in.
4. Browse [docs/DECISIONS/]({{ config.extra.github_url }}/tree/main/docs/DECISIONS) for the 24 ADRs that bind major design choices.

## Doc class system (ADR-0014)

Every document in `docs/` carries a status header:

- **NORMATIVE** &mdash; binding contract. Changes require an ADR.
- **INFORMATIVE** &mdash; orientation / explanation. May evolve without ADR.

This site mirrors the same convention. The
[ARCHITECTURE]({{ get_url(path='@/contribute/architecture.md') | safe }})
page is INFORMATIVE; the [MCP tools]({{ get_url(path='@/developer/_index.md') | safe }}#1-mcp-server-stdio-json-rpc)
contract is NORMATIVE and revisions are gated by ADR.

## ADR-driven design

If a design discussion in
[GitHub Discussions]({{ config.extra.github_url }}/discussions) reaches
a decision, an ADR captures it in `docs/DECISIONS/`. If Discussions ever
disappears, the ADRs preserve the binding decisions in the source tree.

PRs that lock in a new design decision **must** include an ADR before
the code change lands. PRs that follow an existing ADR are fine on their
own.
