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

## Repos (one Artifacts namespace per deployment, default `gitbots`)

| repo           | holds                                                         |
|----------------|---------------------------------------------------------------|
| `<prj>`        | the code (`main` and attempt branches) plus `gitbots/activity`   |
| `<prj>-logs`   | `gitbots/logs`, kept separate because of the 1 GB per-repo cap   |
| `<prj>-<att>`  | optional fork per hosted attempt (`fork()`), for untrusted or hosted agents |

`<prj>` is the project id lowercased (`prj_01k…`), unless Artifacts repo-name
rules require otherwise; `gitbots-worker` owns the mapping. Remote URL:
`https://<ACCOUNT_ID>.artifacts.cloudflare.net/git/<namespace>/<repo>.git`. Git auth
uses `-c http.extraHeader="Authorization: Bearer <token>"` for each command;
tokens are **never** written to `.git/config`.

## Identity and tokens

- Artifacts tokens are per repo, `read` or `write`, and expire after a TTL.
  The Worker mints one per agent session and records `token id ↔ session ↔ repo`
  in D1. A fork token proves which session pushed to that fork; this is
  *attested* identity, not self-asserted.
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

## Control-plane API (`/v1`, JSON, `Authorization: Bearer <key>`)

| method | path                     | key   | body → response |
|--------|--------------------------|-------|-----------------|
| POST   | `/v1/projects`           | admin | `{project_id, name}` → `{project_id, owner_key, namespace, remotes: {main, logs}}` (creates both repos) |
| GET    | `/v1/project`            | owner | → `{project_id, name, namespace, remotes, created_at}` |
| POST   | `/v1/tokens`             | owner | `{repo: "main" \| "logs" \| <fork repo>, scope: "read" \| "write", session?: ses_id, ttl_secs?: number}` → `{token, expires_at, remote}` |
| POST   | `/v1/forks`              | owner | `{attempt: att_id, session?: ses_id}` → `{repo, remote, token, expires_at}` |
| POST   | `/v1/ingest`             | owner | `{}` → `{repos: [{repo, tip, new_events}]}`: index `gitbots/activity` of `<prj>` and all its forks now |
| GET    | `/v1/outbox`             | owner | → `[{id, created_at, kind: "task.create" \| "review", body, actor}]`, pending only |
| POST   | `/v1/outbox/{id}/ack`    | owner | `{event?: evt_id, error?: string}` → `{}` |

`body` for `task.create` is `{title, body?, labels?}`; for `review` it is
`{attempt, decision, reason?, merge?}`. These are the same shapes as the
`docs/API.md` POSTs.

Errors are `{"error": "..."}` with 400, 401, 403, 404, 409 or 422.

## Dashboard API (`/api`, the docs/API.md contract)

The Worker serves the same read endpoints as `gitbots ui`, folded from the
indexed events with `gitbots-core` (wasm). The differences when hosted:

- auth is the owner key (`#token=` link);
- `POST /api/tasks` and `POST /api/attempts/{id}/review` return
  **202** `{queued: true, outbox: "<id>"}`;
- `/api/project` adds `"hosted": true` and `"pending_outbox": number`;
- `/api/attempts/{id}/diff` is computed in the Worker from the trees of
  `base` and `head` (readTree/readBlob) and needs the attempt branch pushed;
- `/api/logs` reads from `<prj>-logs`;
- `/api/tip` returns the newest indexed event id.

## Indexing

An Artifacts event subscription sends push events for the namespace to a
Queue. The consumer re-indexes the repo that was pushed (`<prj>` or a fork;
`-logs` repos are skipped). `/v1/ingest` does the same on demand.
Indexing walks the `events/` tree, pruning subtrees whose hash matches the last
indexed tip (the layout is sharded by hour and session, so this stays cheap),
parses each new file as an `gitbots_core::Event`, and upserts it by id
(idempotent). Unparseable files are counted and skipped.

## Local development

`gitbots ui` serves the same `/api` locally from the git checkout with no cloud.
`wrangler dev` runs the Worker locally against D1, Queues and the real
Artifacts binding (remote bindings).
