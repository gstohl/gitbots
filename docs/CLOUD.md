# gitbots on Cloudflare (Artifacts + Workers)

Git stays the source of truth. [Cloudflare Artifacts](https://developers.cloudflare.com/artifacts/)
hosts the repos, and a Rust Worker (`crates/gitbots-worker`) is the control plane.

```
 agents (Claude Code, Codex, subagents)        humans (browser)
        │  gitbots CLI / gitbots mcp                       │  SolidJS (Worker static assets)
        ▼                                            ▼
  local repo + workrooms ── git push/fetch ──►  Artifacts repos ◄── readTree/readBlob ── gitbots-worker (Rust)
        ▲   (bearer token per session)          <prj>  <prj>-logs          │  D1: index, tokens, outbox
        └──────────── gitbots sync (steward): applies the outbox, merges, pushes ◄──┘  Queue: push events
```

## Repos (one Artifacts namespace per deployment, e.g. `gitbots-dev`)

| repo           | holds                                                         |
|----------------|---------------------------------------------------------------|
| `<prj>`        | the code (`main` and attempt branches) plus `gitbots/activity`   |
| `<prj>-logs`   | `gitbots/logs`, kept separate because of the 1 GB per-repo cap   |
| `<prj>-<att>`  | optional fork per hosted attempt (`fork()`), for untrusted or hosted agents |

`<prj>` is the project id lowercased (`prj_01k…`). Artifacts repo names must
match `^[a-zA-Z0-9][a-zA-Z0-9._-]*$`, so that is valid as is. `<att>` is the
attempt's short id (last 6 characters, lowercased), so a fork is
`prj_01k…-nr4r8w`. `gitbots-worker` owns the mapping (`gitbots_cloud::naming`);
clients take repo names and remotes from the `/v1` responses and never build
them. `<prj>` defaults to branch `main`, `<prj>-logs` to `gitbots/logs`.
A fork copies **all** branches of `<prj>` (`defaultBranchOnly: false`), so it
carries `gitbots/activity` and the base branch.

Remote URL:
`https://<ACCOUNT_ID>.artifacts.cloudflare.net/git/<namespace>/<repo>.git`. Git auth
sets `http.extraHeader="Authorization: Bearer <token>"` for each command through
the git child's environment (`GIT_CONFIG_COUNT`, `GIT_CONFIG_KEY_<n>`,
`GIT_CONFIG_VALUE_<n>`; git >= 2.31), so the token is not on the command line
where `ps` shows it; tokens are **never** written to `.git/config` and are
redacted from errors. Push uses the git v1
receive-pack protocol (Artifacts has no v2 push). Limits: 1 GB per repo, 32 MB
per file, 2,000 git requests per 10 s per repo.

The namespace needs no setup: the first repo created in it creates it.

## Identity and tokens

- Artifacts tokens are per repo, `read` or `write`, and expire after a TTL
  (default 86,400 s; `ttl_secs` must be 60..=31,536,000, else 422). Treat the
  token as an opaque string: the docs show `art_v1_<hex>?expires=<unix>`, live
  ones are `art_v2_x_…`. Send it whole as the bearer.
  The Worker mints one per agent session and records `token id ↔ session ↔ repo`
  in D1. A fork token proves which session pushed to that fork; this is
  *attested* identity, not self-asserted. The extra write token that
  `create()`/`fork()` return is revoked at once, so every live token belongs
  to a recorded session. `POST /v1/forks` always mints a fresh session token;
  if the fork is still being copied after ~10 s it answers 409 (retry).
- The CLI pushes with the token of the acting session: `--session`,
  `GITBOTS_SESSION`, the workroom binding, or, for an agent harness that names
  none (`CLAUDECODE`, `CODEX_*`, ...), a session started for it (`via: "env"`,
  label `auto: <marker>`, reused in that worktree while the same agent runs).
  Its events, commit trailers and pushes all name that session; only the
  human's own process uses a session-less (human) token.
- Events in a fork whose actor is a human are not indexed: human decisions
  only reach the index through `<prj>` (the steward pushes them).
- The project **owner key** (returned once at provisioning, stored as SHA-256 in
  D1) authenticates the human's CLI and the dashboard (`#token=` link, the
  same flow as `gitbots ui`).
- Provisioning a project needs the deployment's `GITBOTS_ADMIN_KEY` secret.

## Writes go through the outbox

The Workers binding has no write API, so the Worker never writes git. A human
decision made in the dashboard is stored in the D1 **outbox**. The next
`gitbots sync` (the "steward": the human's machine, `gitbots sync --watch`, or later
a Container) applies it to the ledger as the human, with `via: "ui"`, does any
merge, pushes, and acks the item with the resulting event id. The dashboard
shows queued items as pending.

The steward records each applied item under the idempotency key
`outbox:<item id>`. When the indexer stores an event of `<prj>` with that
`idem`, it marks the item done, so a lost ack never leaves a decision pending
(or gets it applied twice). An ack may carry both `event` and `error` (the
review was recorded but the merge failed): the item is done and the error is
kept as `last_error`; with only `error` it is `failed`. Acking twice is a
no-op. `/api/project` lists the latest such errors as `outbox_errors`.

The resulting event (`task.created` or `review.decided`) carries the
idempotency key `idem: "outbox:<id>"`, so an item that was applied but not
acked (the push or the ack failed, or two stewards raced) is acked with that
event by the next sync and never applied twice. Acks:

- `{event}`: applied and already on `<prj>` (after a merge, the trusted
  branch is pushed before the activity ledger).
- `{error}`: a failure a retry can't fix: unknown or not-submitted attempt,
  refused by the mandate, malformed body, non-human actor.
- `{event, error}`: the review is recorded but its merge failed; the human
  merges by hand.
- no ack: a transient failure (network, a trusted branch that diverged from
  `<prj>`, a `kind` this CLI doesn't know) leaves the item pending for the
  next sync; `gitbots sync --json` marks it `"retry": true`.

Only the human's own `gitbots sync` applies the outbox: never an agent
session, and never a process that an agent harness runs.

## Control-plane API (`/v1`, JSON, `Authorization: Bearer <key>`)

| method | path                     | key   | body → response |
|--------|--------------------------|-------|-----------------|
| POST   | `/v1/projects`           | admin | `{project_id, name}` → `{project_id, owner_key, namespace, remotes: {main, logs}}` (creates both repos) |
| GET    | `/v1/project`            | owner | → `{project_id, name, namespace, remotes, created_at}` |
| POST   | `/v1/tokens`             | owner | `{repo: "main" \| "logs" \| <fork repo>, scope: "read" \| "write", session?: ses_id, ttl_secs?: number}` → `{token, expires_at, remote}`. `session` need not be indexed yet (a new session pushes its own `session.started` with this token) |
| POST   | `/v1/forks`              | owner | `{attempt: att_id, session?: ses_id}` → `{repo, remote, token, expires_at}` |
| POST   | `/v1/ingest`             | owner | `{}` → `{repos: [{repo, tip, new_events}]}`: index `gitbots/activity` of `<prj>` and all its forks now |
| GET    | `/v1/outbox`             | owner | → `[{id, created_at, kind: "task.create" \| "review", body, actor}]`, pending only |
| POST   | `/v1/outbox/{id}/ack`    | owner | `{event?: evt_id, error?: string}` (either or both) → `{}` |

`body` for `task.create` is `{title, body?, labels?}`; for `review` it is
`{attempt, decision, reason?, merge?}`. These are the same shapes as the
`docs/API.md` POSTs.

Errors are `{"error": "..."}` with 400, 401, 403, 404, 409 or 422 (502 when
Artifacts itself fails). A body that is not JSON is 400; JSON of the wrong
shape is 422, as in `gitbots ui`.

## Dashboard API (`/api`, the docs/API.md contract)

The Worker serves the same read endpoints as `gitbots ui`, folded from the
indexed events with `gitbots-core` (wasm). The differences when hosted:

- auth is the owner key (`#token=` link);
- `POST /api/tasks` and `POST /api/attempts/{id}/review` return
  **202** `{queued: true, outbox: "<id>"}`;
- `/api/project` adds `"hosted": true`, `"pending_outbox": number` and
  `"outbox_errors": [{id, kind, status, event, error, acked_at}]` (latest 10);
- `/api/project` is 404 until `<prj>` has `.gitbots/manifest.json` on the
  trusted branch (`main`); `viewer` (and the `actor` of outbox items) is the
  manifest's first `owner` principal;
- `/api/attempts/{id}` has `workroom: null`; commits and diffs come from the
  attempt's fork if it has one, else from `<prj>`;
- `/api/attempts/{id}/diff` is computed in the Worker from the trees of
  `base` and `head` (readTree/readBlob) and needs the attempt branch pushed.
  The binding's `log()` is first-parent only, so the merge base is the newest
  commit on head's first-parent chain that is on base's; renames show as a
  delete plus an add (no `-M`); files over 1 MB get a one-line note;
- `/api/workflows` lists each `.gitbots/actions/*.toml` converted to JSON with
  the spec's defaults filled in, but not validated (`gitbots-actions` does not
  build for wasm32);
- `/api/logs` reads from `<prj>-logs`;
- `/api/tip` returns the newest indexed event id.

## Indexing

Artifacts event subscriptions are **per repo**: the `artifacts.repo` source
requires `namespace` and `repo_name` (there is no namespace-wide `pushed`
event; the account-level `artifacts` source only has `repo.*` lifecycle
events). So every `<prj>` and fork needs its own subscription of `pushed` to
the events Queue. The Worker creates it when it creates the repo if it has a
`CF_API_TOKEN` secret; otherwise create it by hand (see
`crates/gitbots-worker/README.md`). A push message looks like:

```json
{"type": "cf.artifacts.repo.pushed",
 "source": {"type": "artifacts.repo", "namespace": "gitbots-dev", "repoName": "prj_01k…"},
 "payload": {"ref": "refs/heads/gitbots/activity", "before": "<sha>", "after": "<sha>", "commits": […]},
 "metadata": {"accountId": "…", "eventSubscriptionId": "…", "eventTimestamp": "…"}}
```

The consumer re-indexes the repo that was pushed when `ref` is
`refs/heads/gitbots/activity` (`<prj>` or a fork; `-logs` repos are
skipped). `/v1/ingest` does the same on demand. In the live tests a push was
indexed 10 to 15 s later; a push made 28 s after its subscription was created
was never delivered, so `gitbots sync` keeps calling `/v1/ingest`.
Indexing walks the `events/` tree, pruning subtrees whose hash matches the last
indexed tip (the layout is sharded by hour and session, so this stays cheap),
parses each new file as an `gitbots_core::Event`, and upserts it by id
(idempotent). Unparseable files are counted and skipped. Files already stored
with the same blob are not re-read, so a fork only costs its new events. One
run reads at most `INDEX_BUDGET` blobs; the checkpoint (last commit and root
tree, compare-and-swap in D1) only moves when a run completes, and the next
run continues.

Binding facts the indexer relies on (checked live): `log({ref})` resolves a
short branch name (`gitbots/activity`, `main`) or a commit SHA, but returns
`[]` for `refs/heads/…` and for `HEAD`; commits are
`{hash, treeHash, message, parents, author, committer, authoredAt, committedAt}`;
tree entries are `{name, mode: "100644" | "40000" | …, hash, type: "blob" | "tree" | "symlink" | "gitlink" | "exec"}`;
`readBlob`/`readFile` return a `Blob` or `null`; `info().lastPushAt` stayed
`null` after pushes.

## Local development

`gitbots ui` serves the same `/api` locally from the git checkout with no cloud.
`wrangler dev` runs the Worker locally against D1, Queues and the real
Artifacts binding (remote bindings; there is no local Artifacts emulator).
Setup, deploy, a live end-to-end test and teardown are in
`crates/gitbots-worker/README.md`.
