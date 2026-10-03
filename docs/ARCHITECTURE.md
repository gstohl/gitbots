# gitbots architecture

`gitbots` is agent-native git: a layer on top of plain git that
gives every change an accountable agent identity, records what agents do in
the repo itself, and lets humans steer by mandate instead of by hand.

Two product surfaces sit on this base:

- **Workrooms**: isolated task attempts, handoffs, integration previews and
  result review. This is where teams run and accept agent-produced work.
- **Agent Commons**: publishable task recipes with permissions, environments
  and evaluation fixtures, which people can discover, fork and improve.

The human is meant to be almost zero-touch. Agents do the work and report;
the human sets the mandate, reads the inbox, and makes the decisions the
mandate reserves for humans.

## Principles

1. **Git is the database.** Everything an agent does is recorded on git
   branches that clone, push and fetch like any other. No server is needed to
   get the full history. A hosted layer (Cloudflare) indexes it later; it does
   not own it.
2. **Append-only and conflict-free by construction.** Ledger writes never edit
   existing files, so concurrent agents and machines merge by tree union.
3. **Identity on everything.** Every event and every agent commit names the
   agent (provider/model/client) and the session (one chat or run) that made it,
   including the parent session for subagents.
4. **The mandate is data.** Permissions, autonomy and the human decision
   hierarchy live in `.gitbots/manifest.json`, read from the trusted branch, and
   are enforced by the tools.
5. **Pure core.** Domain types and folds (`gitbots-core`) do no IO and build for
   `wasm32-unknown-unknown`, so the same code runs in a Cloudflare Worker.
6. **Local enforcement is advisory.** An agent with a shell can bypass gitbots.
   Anything that must hold is mirrored to the git host (rulesets, CODEOWNERS).

## Crates

| crate          | role                                                                  | IO      |
|----------------|-----------------------------------------------------------------------|---------|
| `gitbots-core`    | ids, identity, events, ledger layout, manifest/mandate, recipes, redaction, board + stats folds | none |
| `gitbots-git`     | ledger branches (gix), workrooms (worktrees), hooks, sync, union merge | git     |
| `gitbots-actions` | workflow spec, engine, runners (`local`, `celesto`)                   | process, HTTP |
| `gitbots`         | `Project` facade + `gitbots` CLI + `gitbots mcp` stdio server               | all     |

Dependency direction: `gitbots-core` <- `gitbots-git`, `gitbots-actions` <- `gitbots`.
`gitbots-actions` does not depend on `gitbots-git`; it writes logs through a
`LogSink` trait so a hosted runner can reuse it unchanged.

## `gitbots init`: the agentic `git init`

`gitbots init` creates:

```
.gitbots/
  manifest.json      # project meta, tenancy, mandate (committed)
  actions/           # workflow definitions (*.toml)
  recipes/           # task recipes (<name>/recipe.toml)
```

It also creates the two ledger branches and installs the git hooks.
Workrooms do not live in the repo (see [Workrooms](#workrooms)).

### Manifest (`.gitbots/manifest.json`)

```json
{
  "version": 1,
  "project": { "id": "prj_01K...", "name": "demo" },
  "tenancy": { "owner": { "kind": "user", "handle": "gstohl" }, "team": null, "workspace": null },
  "mandate": {
    "goal": "Ship the gitbots base",
    "autonomy": "assisted",
    "principals": [ { "handle": "gstohl", "email": "dominik@gstohl.com", "role": "owner" } ],
    "agents": {
      "allowed_paths": ["**"],
      "denied_paths": [".gitbots/**", ".github/**", ".claude/**", ".codex/**", ".cursor/**",
                       ".mcp.json", "**/CLAUDE.md", "**/AGENTS.md", "CLAUDE.md", "AGENTS.md"],
      "protected_branches": ["main", "master"]
    },
    "approvals": {
      "accept_attempt": "any",
      "merge_protected": "maintainer",
      "change_mandate": "owner",
      "run_hosted_action": "maintainer"
    }
  },
  "ledger": { "activity_branch": "gitbots/activity", "logs_branch": "gitbots/logs" },
  "workrooms": { "branch_prefix": "gitbots/attempt" }
}
```

- Tenancy (user or org, then team, then workspace, then repo) is recorded now
  and enforced by the hosted layer later.
- `autonomy` only picks the default `approvals` at init. After that the
  explicit `approvals` map is the source of truth.
- `approvals` values are `any | reviewer | maintainer | owner`. `any` lets an
  agent or a human decide; the others need a human principal with at least that
  role (`reviewer < maintainer < owner`). An agent asking for a human-gated
  decision gets `NeedsHuman { role }`.
- The `denied_paths` above are the defaults: files that grant permissions to
  future agents or runners. Matching is case-insensitive (`.Gitbots/Manifest.json`
  is the same file on macOS).

### Trusted mandate

The mandate and `.gitbots/actions/*.toml` are read from the tip of the **trusted
branch** (git config `gitbots.trustedBranch`, default `main`), never from the
working tree. An agent therefore can't grant itself permissions by editing the
manifest inside its workroom. The working tree is used only before the first
commit.

Because local enforcement is advisory, mirror it to GitHub: branch protection
or rulesets for `protected_branches`, and a CODEOWNERS rule for `.gitbots/**`.

### No escalation

- An approval-gated decision never resolves to the git-config human unless
  stdin is an interactive TTY **and** no agent marker env var is set
  (`CLAUDECODE`, `CODEX_*`, `GITBOTS_SESSION`, ...). Otherwise the actor stays the
  agent and the decision returns `NeedsHuman`.
- The MCP server never resolves to a human.
- No member of an attempt's session family may approve it. The family
  (`Board::session_family`) is the root ancestor session plus all of its
  descendants, so a parent can't spawn a subagent to rubber-stamp its own work.

## Identity

- **AgentDescriptor**: `{provider, model, client, client_version?}`, for
  example `anthropic/claude-opus-5-5@claude-code`. This is "which AI", and it is
  the unit for benchmarks.
- **Session** `ses_<ulid>`: one chat or run of an agent. It records
  `{agent, parent?, role?, operator?, external_id?, label?, started_at}`.
  `parent` links a subagent to the session that spawned it; `operator` is the
  human the agent reports to.
- **Actor** on every event is one of `human {handle, email?}`,
  `agent {session, agent, parent?}` or `system {component}`.

How the current actor is resolved, in order (the `via` it is recorded with in
brackets):

1. the `--session` flag (`flag`)
2. the `GITBOTS_SESSION` env var (`env`)
3. the session bound to the current workroom, `<git-dir>/gitbots/session` (`worktree`)
4. the human from git config (`git_config`); for approval-gated decisions only
   on an interactive TTY with no agent marker (`tty`)

Under `gitbots mcp`, the MCP `clientInfo` seeds the agent (`mcp`). gitbots's own
writes (hooks, actions engine) use `system`.

Every workroom is bound to the session that started it. An agent working
there is therefore attributed even when its shell does not keep env vars
between calls.

### Commit trailers

The `prepare-commit-msg` hook adds trailers to agent commits with
`git interpret-trailers`, so they join the existing trailer block and a
`Co-Authored-By` line stays intact:

```
Gitbots-Session: ses_01K...
Gitbots-Provider: anthropic
Gitbots-Model: claude-opus-5-5
Gitbots-Client: claude-code
Gitbots-Parent-Session: ses_01K...   (subagents only)
```

`Gitbots-Session` is authoritative; the others are a readable copy. The
`post-commit` hook records a `commit.recorded` event with diffstat (idempotency
key `commit:<oid>`). Hooks fail open (a broken gitbots never blocks a commit) and
honor `core.hooksPath`.

**Trust model (base):** identity is self-asserted, like git author names. `via`
tells readers how it was resolved. The hosted layer will attest identity: it
knows which credential ran which model. Signed events are planned.

## Ledger branches

Two orphan branches, pushed and fetched like any other:

| branch          | content                                                    | pushed by default |
|-----------------|------------------------------------------------------------|-------------------|
| `gitbots/activity` | structured events + session records (small)                | yes               |
| `gitbots/logs`     | raw logs: action output, transcripts, traces (large)       | no (`gitbots sync --logs`) |

Layout (spec: module docs in `gitbots-core/src/ledger.rs`):

```
gitbots/activity
  LEDGER.json                                       # {format: 1, kind: "activity", project, epoch: 0}
  sessions/<YYYY>/<MM>/<ses>.json                   # Session record, written once
  events/<YYYY>/<MM>/<DD>/<HH>/<ses|_>/<evt>.json   # one Event per file; `_` = no session
  idem/<xx>/<sha256>                                # idempotency markers (content = key)
  conflicts/...                                     # union-merge quarantine
gitbots/logs
  LEDGER.json
  runs/<YYYY>/<MM>/<DD>/<run>/<job>.log
  sessions/<YYYY>/<MM>/<DD>/<ses>/<ulid>-<name>.log
  ...*.ptr.json                                     # {sha256, size, uri} of an out-of-git blob
```

- Every path is derived from an id. Date segments are UTC and come from the
  id's ULID, never from the wall clock.
- Sharding by hour and session keeps trees small, and two writers never touch
  the same file.
- Commit boundaries carry no meaning: a writer may batch any number of files
  into one ledger commit.

How writes work:

- Each ledger commit is built in memory with no working tree.
- The ref update is compare-and-swap: it expects the old tip and retries on a
  race.
- New ULIDs are bumped to sort after the newest event at the ledger tip,
  capped at 5 minutes, to tolerate clock skew between machines.
- An event with an `idem` key is skipped if `idem/<xx>/<sha256(key)>` exists.
  Identical writers write identical markers, so markers never conflict.
- Logs are redacted (`gitbots_core::redact`) before writing and capped at 25 MB.
  A log may be stored out of git as a `*.ptr.json` pointer.

### Sync and union merge

`gitbots sync` fetches, merges and pushes `gitbots/activity` (and `gitbots/logs` with
`--logs`):

- Fast-forward when possible.
- Otherwise build a **deterministic** merge commit: sorted parents, fixed
  signature, commit time = max parent time, tree = union of both. Two machines
  merging the same tips produce the same commit, so they converge.
- Same path with different blobs: the smaller oid stays at the path, both
  blobs are copied to `conflicts/<path>/<oid>`, and the conflict is reported.
- Different `LEDGER.json` (`project`, `format`, `kind` or `epoch`): refuse.
  `epoch` is reserved for approved rewrites, e.g. purging a leaked secret.

### Rewrite detection

gitbots keeps the last-seen ledger tips in `<common-dir>/gitbots/tips/`. A ledger
that is not a fast-forward of its last-seen tip has been rewritten, and sync
refuses it. Never force-push a ledger branch; add a GitHub ruleset that blocks
force-push and deletion on `gitbots/*`. Rulesets are a key reason the ledgers are
real branches rather than `refs/gitbots/*` or git notes: hosts can protect
branches, and branches clone and fetch by default.

### Event

```json
{
  "v": 1,
  "id": "evt_01K...",
  "ts": "2026-10-03T10:31:00Z",
  "actor": { "type": "agent", "session": "ses_01K...", "agent": { "provider": "openai", "model": "gpt-5-codex", "client": "chatgpt" } },
  "via": "mcp",
  "producer": "gitbots/0.1.0",
  "idem": "mcp:ses_01K...:42",
  "on": "evt_01K...",
  "kind": "review.decided",
  "data": { "attempt": "att_01K...", "decision": "accept", "mandate": "<manifest blob oid>" }
}
```

- `via`: how the actor was resolved
  (`flag | env | worktree | mcp | git_config | tty | system`), so the indexer
  knows how far to trust it.
- `producer`: the writer, e.g. `gitbots/0.1.0`.
- `idem`: idempotency key (`commit:<oid>`, `mcp:<ses>:<rpc>`), backed by the
  `idem/` markers.
- `on`: the event acted upon, e.g. the submission a review decides.
- Order and paths come from `id`; `ts` is the writer's clock.

Compatibility rules (the ledger outlives any one binary):

- readers ignore unknown fields;
- kinds a build doesn't know, or can't parse, become `Unknown{kind, data}` and
  are kept verbatim;
- existing kinds only gain optional fields; a breaking change gets a new kind
  name;
- `v` versions the envelope, not the payloads.

Event kinds:

| group     | kinds                                                                                         |
|-----------|-----------------------------------------------------------------------------------------------|
| project   | `project.initialized`                                                                         |
| session   | `session.started`, `session.ended`                                                            |
| tasks     | `task.created`                                                                                |
| attempts  | `attempt.started`, `attempt.submitted`, `attempt.handoff`, `attempt.abandoned`, `attempt.merged` |
| review    | `review.decided`                                                                              |
| commits   | `commit.recorded`                                                                             |
| actions   | `action.completed`                                                                            |
| reporting | `report`, `tool.called`                                                                       |

`attempt.submitted` and `review.decided` carry `mandate`: the blob oid of the
manifest they were checked against. `attempt.merged` carries `source_commits`,
so attribution survives squash merges.

## Workrooms

A **task** has many **attempts**. Each attempt has:

- a branch, `gitbots/attempt/<slug>-<short>` (`workrooms.branch_prefix`)
- a worktree, `$GITBOTS_HOME/workrooms/<project_id>/<slug>-<short>`
- a bound session

Workrooms live **outside** the repo. `$GITBOTS_HOME` defaults to
`$XDG_DATA_HOME/gitbots`, else `~/.local/share/gitbots`; git config `gitbots.workrooms`
overrides the location. A checkout nested inside the repo breaks tools:
`CLAUDE.md` is loaded twice, `node_modules` resolution walks into the parent,
file watchers fire on every attempt.

The attempt lifecycle:

```
task.created
  -> attempt.started (branch + worktree)
  -> agent commits (trailers + commit.recorded)
  -> attempt.submitted -> actions on "attempt.submitted" -> action.completed
  -> [attempt.handoff -> another session continues]
  -> review.decided (accept | reject | changes_requested), authorized by mandate
  -> attempt.merged
```

When an attempt is submitted, its changed paths are checked against
`mandate.agents.allowed_paths/denied_paths` of the trusted mandate. A violation
blocks the submit. A review is authorized against the trusted mandate and the
session-family rule (see [No escalation](#no-escalation)).

The board (task and attempt state) and the stats are pure folds over events
in `gitbots-core`. CLI, MCP and a future web UI compute the same views from the
same ledger.

## Actions

Workflows live in `.gitbots/actions/*.toml` and are loaded from the trusted
branch, so an attempt can't change the checks that judge it:

```toml
name = "ci"
on = ["attempt.submitted", "manual"]

[env]
RUST_BACKTRACE = "1"

[jobs.test]
runs-on = "local"
timeout-secs = 900
steps = [
  { name = "fmt",  run = "cargo fmt --check" },
  { name = "test", run = "cargo test" },
]

[jobs.lint]
needs = ["test"]
steps = [{ run = "cargo clippy -- -D warnings" }]
```

- Triggers are event kinds; `manual` is reserved for running by hand.
- Jobs run in dependency order. A job whose `needs` failed is `skipped`.
- Job output is redacted and goes to
  `gitbots/logs:runs/<YYYY>/<MM>/<DD>/<run>/<job>.log`.
- Each run records one `action.completed` event that links to those logs.

Runners (`runs-on`), behind one `Runner` trait:

- `local`: runs each step with `sh` in the attempt's workroom.
- `celesto`: the Celesto Computers REST API (`https://api.celesto.ai/v1`).
  Creates a microVM, clones the attempt commit from a reachable remote, execs
  each step (at most 300 s per step) and always deletes the VM. Needs
  `CELESTO_API_KEY`; private repos also need `GITBOTS_GIT_TOKEN`.
- later: Cloudflare Containers.

Celesto CI, the GitHub Actions runner product
(`runs-on: [self-hosted, celesto-cloud]`), is an option for this repo's own CI
(see `.github/workflows/ci.yml`). It is not an gitbots runner.

## Interfaces

- **CLI** (`gitbots`): for humans and for agents with a shell.
- **MCP** (`gitbots mcp`, stdio): for ChatGPT, Codex, Claude and others without a
  workspace of their own. MCP `clientInfo` seeds the agent identity. It never
  resolves to a human.
- **Hosted** (later): Cloudflare Workers that index ledgers into D1/R2, keep
  live state in a Durable Object per project, serve the dashboard and the
  Commons, and dispatch hosted actions.
