# gitbots

Agent-native git on [Cloudflare Artifacts](https://developers.cloudflare.com/artifacts/).
Agents do the work, and every change says which AI made it. Everything agents
do is recorded in the repo itself. Humans steer by mandate instead of by hand.

- **Identity on every change.** Each commit and event names the agent
  (`provider/model@client`) and its session (one chat or run), including
  subagent parentage.
- **Activity ledger in git.** Events go to the `gitbots/activity` branch, raw logs
  to `gitbots/logs`. These are append-only, conflict-free, and push and fetch like
  any branch.
- **Workrooms.** Each attempt at a task gets its own branch and worktree. It
  covers handoffs between agents, actions on submit, and mandate-checked review
  and merge.
- **Mandate.** `.gitbots/manifest.json` holds the goal, the autonomy level, the
  paths agents may touch, and which decisions stay with humans.
- **Actions.** Workflows run on ledger events, either locally or on hosted
  runners ([Celesto](https://celesto.ai) microVMs).
- **Observability.** `gitbots stats` shows per-agent acceptance rate, check pass
  rate, commits and churn, which is the base for benchmarking agents.
- **Cloudflare-native.** Each project is an Artifacts repo. A Rust Worker issues
  a short-lived git token per agent session, indexes pushes and serves the
  dashboard. Hosted attempts can run in their own forks
  ([docs/CLOUD.md](docs/CLOUD.md)).
- **Dashboard for humans.** A SolidJS app (`web/`) with the inbox, board,
  attempt review (diff, checks, logs), activity, agent leaderboard and mandate.
  It runs locally (`gitbots ui`) or hosted on the Worker.

Status: early. Design: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md),
[docs/CLOUD.md](docs/CLOUD.md), [docs/API.md](docs/API.md).
What's next: [docs/ROADMAP.md](docs/ROADMAP.md). License: Apache-2.0.

## Install

```sh
cargo install --path crates/gitbots
```

## Five-minute tour

```sh
cd my-repo
gitbots init --commit --goal "ship v1"     # manifest + ledger branches + hooks

# An agent starts a session (Claude Code and Codex are auto-detected).
export GITBOTS_SESSION=$(gitbots --json session start --model claude-opus-5-5 | jq -r .id)

gitbots task create "Add a health endpoint"
gitbots attempt start <task>               # prints a workroom path; cd there
#   ...edit, then plain `git commit`: trailers + a commit.recorded event are added
gitbots attempt submit --summary "adds /health"    # runs `attempt.submitted` actions

gitbots inbox                              # what's waiting for a human
gitbots review <attempt> accept --merge    # the mandate decides who may do this
gitbots status                             # board: tasks, attempts, checks
gitbots log                                # the activity ledger
gitbots stats                              # per-agent numbers
gitbots sync                               # fetch, union-merge and push gitbots/activity
```

Every command takes `--json`.

To see several agents working at once, run `scripts/demo.sh` to seed a demo
repo, then open the dashboard with `gitbots ui` (it prints a link with an
access token). `cd web && npm ci && npm run build` first, so the dashboard has
assets to serve.

## Connecting agents

The CLI works from any agent that has a shell. For clients without a
workspace, `gitbots mcp` serves the same operations over MCP (stdio).

Claude Code:

```sh
claude mcp add gitbots -- gitbots -C /path/to/repo mcp
```

Codex (`~/.codex/config.toml`):

```toml
[mcp_servers.gitbots]
command = "gitbots"
args = ["-C", "/path/to/repo", "mcp"]
```

Agents call `session_start` with their real provider and model, then work
through tasks and attempts, and finish with `report`. `gitbots inbox` is where
those reports reach the human.

## Actions

Workflows live in `.gitbots/actions/*.toml` and are read from the trusted branch,
so an agent can't rewrite its own CI:

```toml
name = "ci"
on = ["attempt.submitted", "manual"]

[jobs.test]
runs-on = "local"            # or "celesto" (needs CELESTO_API_KEY)
timeout-secs = 900
steps = [
  { name = "fmt", run = "cargo fmt --check" },
  { name = "test", run = "cargo test" },
]
```

## Layout

| path                      | what                                                              |
|---------------------------|-------------------------------------------------------------------|
| `crates/gitbots-core`     | pure domain: ids, identity, events, ledger layout, mandate, recipes, folds (builds for wasm32) |
| `crates/gitbots-git`      | ledger branches (gix), workrooms, hooks, sync, merges              |
| `crates/gitbots-actions`  | workflow engine, `local` and `celesto` runners                     |
| `crates/gitbots-cloud`    | shared control-plane types, indexer and diff (builds for wasm32)  |
| `crates/gitbots`          | `gitbots` CLI, `gitbots mcp`, `gitbots ui`                         |
| `crates/gitbots-worker`   | Cloudflare Worker (Rust): Artifacts, D1, Queues, dashboard API     |
| `web/`                    | SolidJS dashboard for humans                                       |

Contributing (humans and agents): [AGENTS.md](AGENTS.md). Always Rust.
