# gitbots roadmap

Next steps, grouped by owner. Everything is Rust.

## Dominik: actions and runners

- **Celesto runner verification** (Celesto Computers API, `https://api.celesto.ai/v1`):
  - Does VM create block until the VM is ready, or do we poll?
  - Which size ids are valid? Pick a default and validate in the spec.
  - Exec is capped at 300 s per call: split, background-and-poll, or reject
    longer steps up front.
  - Private repo auth with `GITBOTS_GIT_TOKEN`: keep it out of argv, logs and the
    clone URL that ends up in `.git/config`.
  - Confirm the VM is deleted on every exit path (failure, timeout, Ctrl-C).
- **Parallel jobs**: run independent jobs of one run concurrently, honoring `needs`.
- **Workflow parallelism**: several workflows or runs at once (one event
  triggering many workflows, many attempts), with per-runner limits.
- **Integration previews**: `git merge-tree` one or more attempts onto the base
  into a temp worktree, run `preview`-triggered actions there, record results.
- **Cloudflare Containers runner** behind the same `Runner` trait.
- **Hosted actions**: dispatch from the hosted layer, gated by
  `approvals.run_hosted_action`.

## Santosh: product and UX

- **Dashboard** on the hosted layer: inbox (reports, blockers), board, sessions.
- **Workrooms UI**: attempts, handoffs, review, integration preview results.
- **Agent Commons**: publish and fork recipes, discovery, run a recipe's
  evaluation fixtures on a fork.
- **Benchmark dashboards**: per-agent (provider/model/client) stats from
  `gitbots_core::stats`, across tasks and projects.
- **Onboarding**: `gitbots init` flow, connecting ChatGPT/Codex via MCP.

## Shared

- **Hosted Cloudflare layer** (Workers + D1/R2/Durable Objects):
  - indexer Worker that reads pushed ledgers into D1;
  - R2 for logs and `*.ptr.json` targets;
  - one Durable Object per project for live state;
  - tenancy: org/user, team, workspace, repo.
- **Signed events / attested identity**: the hosted layer knows which
  credential ran which model; sign events so `via` is no longer self-asserted.
- **GitHub mirror + rulesets**: mirror `protected_branches` as branch
  protection, CODEOWNERS for `.gitbots/**`, a ruleset blocking force-push and
  deletion on `gitbots/*`. `gitbots init` should offer to set this up.
- **Claude Code hooks**: `PostToolUse` -> `gitbots trace` records `tool.called`.
- **ChatGPT remote MCP over HTTP** (today `gitbots mcp` is stdio only).
- **Naming**: settled on `gitbots` (2026-10-03): crates, binary, `.gitbots/`,
  `gitbots/*` ledger branches, `Gitbots-*` trailers, `GITBOTS_*` env vars and
  `gitbots.*` git config keys.
