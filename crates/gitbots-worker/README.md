# gitbots-worker

The Cloudflare control plane for gitbots (`docs/CLOUD.md`), in Rust
(workers-rs) on [Cloudflare Artifacts](https://developers.cloudflare.com/artifacts/):

- `/v1/*`: provisioning, per-session Artifacts tokens, forks, ingest, outbox;
- `/api/*`: the dashboard API of `docs/API.md`, folded from D1 with `gitbots-core`;
- a Queue consumer for Artifacts push events (re-indexes the pushed repo);
- everything else: the SolidJS build from `web/dist` (SPA fallback).

The pure logic (indexer, tree diff, commit ranges, naming, `/api` JSON views)
lives in `gitbots-cloud` and is unit-tested natively against an in-memory
`TreeSource`. This crate is the IO around it:

| file | what |
|------|------|
| `src/artifacts.rs` | wasm-bindgen shim over the Artifacts binding (workers-rs has none); implements `TreeSource` |
| `src/v1.rs` | `/v1` handlers, owner/admin auth |
| `src/dash.rs` | `/api` handlers |
| `src/indexer.rs` | D1-backed `EventStore`, per-repo indexing with a CAS checkpoint |
| `src/subscribe.rs` | optional per-repo push subscription (needs `CF_API_TOKEN`) |
| `src/db.rs`, `migrations/` | D1 schema and queries |
| `src/lib.rs` | router, Queue consumer, `GET /v1/admin/inspect` |

## Build and check

The crate is excluded from the root workspace: it builds only for wasm32, has
its own `Cargo.lock`, and needs Rust >= 1.91 (workers-rs 0.8), pinned to
1.93.0 by `rust-toolchain.toml`.

```sh
cd crates/gitbots-worker
cargo fmt --check
cargo clippy --locked --target wasm32-unknown-unknown --all-targets -- -D warnings
cargo build --locked --target wasm32-unknown-unknown --release   # what CI runs
```

`wrangler.toml`'s build command copies `../../web/dist` (or `static/index.html`
when the frontend isn't built) into `public/`, installs `worker-build` 0.8.7
into `.tools/` once (it must match the `worker` crate; the 0.7 one refuses it),
and runs `worker-build --release`.

## Setup on a fresh account

Uses the official `cf` CLI (1.0.0-beta.12; `cf auth login` grants the
`artifacts.*` scopes) for everything it can do, and wrangler (>= 4.145) only to
deploy. Artifacts needs the Workers Paid plan. The dev deployment lives in
the "gitbots" account; for your own, use your account id and replace the ids
in `wrangler.toml` with the ones the commands print.

```sh
export CLOUDFLARE_ACCOUNT_ID=756c15173deb0a7688cc9515ae584823   # "gitbots"
cf auth whoami

# 1. Resources (names are fixed in wrangler.toml; ids go into it).
cf artifacts namespaces create --namespace gitbots-dev
cf d1 create --name gitbots-dev --primary-location-hint weur    # -> database_id
cf queues create --queue-name gitbots-dev-artifact-events       # -> EVENTS_QUEUE_ID
cf d1 migrations apply <database_id> --dir migrations

# 2. Deploy. `cf deploy` would convert the project to a cloudflare.config.ts
#    and needs cloudflare-rs-dev-server, so deploy with wrangler. wrangler can
#    reuse the cf login for the new account:
export CLOUDFLARE_API_TOKEN=$(python3 -c 'import json,os; print(json.load(open(os.path.expanduser("~/Library/Preferences/cloudflare/config/default.json")))["oauth_token"])')
npx wrangler deploy        # runs the [build] command, uploads ../../web/dist
unset CLOUDFLARE_API_TOKEN

# 3. Admin key: only in ~/.config/gitbots/dev-admin-key, never in the repo.
mkdir -p ~/.config/gitbots && chmod 700 ~/.config/gitbots
(umask 077; openssl rand -hex 32 | tr -d '\n' > ~/.config/gitbots/dev-admin-key)
cf workers secrets update GITBOTS_ADMIN_KEY --script-name gitbots-dev \
  --type secret_text --text "$(cat ~/.config/gitbots/dev-admin-key)"
```

Deployed: `https://gitbots-dev.gitbots-worker.workers.dev` (Worker
`gitbots-dev`, D1 `gitbots-dev` = `d0a56eed-5bfb-485f-998c-bbb86f9dc251`,
Queue `gitbots-dev-artifact-events` = `cf1fa3f7cac0400bb98f64a9c183034b`,
Artifacts namespace `gitbots-dev`).

### Push events (one subscription per repo)

Artifacts subscriptions are per repo (`source.namespace` and
`source.repo_name` are required). `cf queues subscriptions create` has no
flags for them, but takes a raw `--body`:

```sh
REPO=prj_01k...   # <prj> or a fork; -logs repos are never indexed
cf queues subscriptions create --body "{\"name\":\"gitbots-dev-$REPO-pushed\",\"enabled\":true,
  \"source\":{\"type\":\"artifacts.repo\",\"namespace\":\"gitbots-dev\",\"repo_name\":\"$REPO\"},
  \"destination\":{\"type\":\"queues.queue\",\"queue_id\":\"cf1fa3f7cac0400bb98f64a9c183034b\"},
  \"events\":[\"pushed\"]}"
cf queues subscriptions list
```

To have the Worker do this itself for every new `<prj>` and fork, give it an
API token that may manage event subscriptions (Queues Write):
`cf workers secrets update CF_API_TOKEN --script-name gitbots-dev --type secret_text --text <token>`.
Without it, new repos are indexed by `POST /v1/ingest` (which `gitbots sync`
calls) until subscribed by hand.

## Live end-to-end test

What was run against the deployment (2026-10-03). `$B` is the Worker URL,
`gitbots` is `target/debug/gitbots` (`cargo build -p gitbots`). Human
commands run with a clean env (`env -i PATH=$PATH HOME=$HOME ...`) so the
CLI does not detect an agent harness.

```sh
B=https://gitbots-dev.gitbots-worker.workers.dev
ADMIN=$(cat ~/.config/gitbots/dev-admin-key)

# A scratch repo with a real ledger.
git init -b main e2e && cd e2e && ...two commits...
gitbots init --name "gitbots e2e demo" --commit           # -> prj_<ULID>

# Provision (201; a second call is 409).
curl -s -X POST $B/v1/projects -H "Authorization: Bearer $ADMIN" \
  -d '{"project_id":"prj_<ULID>","name":"gitbots e2e demo"}'   # -> owner_key, remotes
OK=<owner_key>; H="Authorization: Bearer $OK"

# Session, task, attempt with a commit, submit, a blocker report, a workflow run.
SES=$(gitbots session start --provider anthropic --model claude-opus-5-5 --client claude-code --json | jq -r .id)
TSK=$(gitbots task create "Make greet say hello" --label demo --json | jq -r .task)
GITBOTS_SESSION=$SES gitbots attempt start $TSK --json     # -> workroom, branch
(cd <workroom> && edit && git commit -am "Make greet say hello" && gitbots attempt submit --summary "...")
gitbots report "Need a decision on naming" --level blocker
gitbots actions run ci --attempt <att>                     # .gitbots/actions/ci.toml on main

# Tokens (the session is not indexed yet: still 200) and pushes.
curl -s -X POST $B/v1/tokens -H "$H" -d "{\"repo\":\"main\",\"scope\":\"write\",\"session\":\"$SES\",\"ttl_secs\":3600}"
curl -s -X POST $B/v1/tokens -H "$H" -d '{"repo":"logs","scope":"write"}'
git -c http.extraHeader="Authorization: Bearer $TM" push $MAIN_REMOTE main <attempt branch> gitbots/activity
git -c http.extraHeader="Authorization: Bearer $TL" push $LOGS_REMOTE gitbots/logs

# Index and read.
curl -s -X POST $B/v1/ingest -H "$H" -d '{}'               # 8 new events; again: 0
for ep in board inbox stats events "events?attempt=<short>" "attempts/<id>" "attempts/<id>/diff" \
          recipes workflows project "logs?path=<LogRef.path>" tip; do
  diff <(curl -s localhost:7792/api/$ep -H "Authorization: Bearer $UI_TOKEN") \
       <(curl -s $B/api/$ep -H "$H")                         # vs `gitbots ui --port 7792`
done

# Dashboard decisions -> outbox -> the real steward.
curl -s -X POST $B/api/attempts/<short>/review -H "$H" -d '{"decision":"accept","merge":true}'  # 202
curl -s -X POST $B/api/tasks -H "$H" -d '{"title":"Add a farewell test"}'                      # 202
curl -s $B/v1/outbox -H "$H"
GITBOTS_CLOUD_KEY=$OK gitbots cloud init --url $B && GITBOTS_CLOUD_KEY=$OK gitbots sync

# Forks.
curl -s -X POST $B/v1/forks -H "$H" -d '{"attempt":"att_...","session":"ses_<unindexed>"}'
git -c http.extraHeader="Authorization: Bearer $TF" push $FORK_REMOTE gitbots/activity
curl -s -X POST $B/v1/ingest -H "$H" -d '{}'
```

Results:

- Provisioning, tokens (including one for a session the indexer has never
  seen), pushes, ingest (8 events, then 0): OK.
- `board`, `inbox`, `stats`, `events` (all filters), `attempts/{id|short}`,
  `/diff`, `recipes`, `workflows`, `logs`: identical to `gitbots ui` on the
  same repo, before and after the merge. The only differences are by design:
  `workroom: null`, `/api/project` `producer`/`hosted`/`pending_outbox`/
  `outbox_errors`, and `/api/tip` returning an event id.
- Review and task POSTs: 202; a second review of the same attempt: 409; bad
  decision: 422; non-JSON: 400. `gitbots sync` applied both (accepted and
  merged, task created), pushed, and the items were acked `done`.
- An outbox event pushed with `idem: "outbox:<id>"` and never acked: the
  Queue consumer marked the item `done` ~13 s later. An ack with
  `{event, error}` keeps it `done` and shows the error in
  `/api/project.outbox_errors`; `{error}` alone makes it `failed`; a
  second ack is a no-op.
- Queue: a push to `<prj>` was indexed by the consumer ~10 s later (no
  ingest call). The very first push, 28 s after the subscription was
  created, was not delivered.
- Fork: created with all branches, the creation token revoked, a session
  token minted and recorded; its agent event was indexed (`new_events: 1`),
  a human `task.created` pushed to it was dropped.

## Teardown

Scratch data of a test project (keeps the Worker, D1, Queue and namespace):

```sh
export CLOUDFLARE_ACCOUNT_ID=756c15173deb0a7688cc9515ae584823
P=prj_<ULID>; R=$(echo $P | tr A-Z a-z)
for r in $R $R-logs $R-<attempt short>; do cf artifacts namespaces repos delete $r --namespace gitbots-dev --force; done
cf queues subscriptions list && cf queues subscriptions delete <subscription id> --force
cf d1 query d0a56eed-5bfb-485f-998c-bbb86f9dc251 --sql "DELETE FROM events WHERE project_id='$P';
  DELETE FROM outbox WHERE project_id='$P'; DELETE FROM tokens WHERE project_id='$P';
  DELETE FROM repos WHERE project_id='$P'; DELETE FROM projects WHERE id='$P'"
rm -rf ~/.local/share/gitbots/workrooms/$P          # the test's workrooms
```

The whole deployment. A Worker that consumes a Queue must lose the consumer
before it can be deleted. These wrangler commands are the ones used to remove
the old 12e deployment; `cf` has equivalents (`cf queues consumers delete
<consumer-id>`, `cf workers delete`, `cf queues delete <queue-id>`,
`cf d1 delete <id>`) that were not exercised.

```sh
npx wrangler queues consumer remove gitbots-dev-artifact-events gitbots-dev
npx wrangler delete gitbots-dev --force
npx wrangler queues delete gitbots-dev-artifact-events
npx wrangler d1 delete gitbots-dev --skip-confirmation
cf artifacts namespaces delete gitbots-dev --force    # after deleting its repos
```

The old `agit-dev` deployment on the "12e" account was removed this way on
2026-10-03 (subscription, consumer, queue, Worker, D1, three repos, and the
namespace via `DELETE /accounts/<id>/artifacts/namespaces/agit-dev`, since
wrangler has no namespace delete).

## Notes

- `GET /v1/admin/inspect?repo=<name>&ref=<branch>` (admin key) returns the raw
  binding results (`info`, `listTokens`, `log` for several ref spellings, the
  tip's root tree), for checking field names against a live repo.
- Facts about the binding, tokens, refs and event payloads are in
  `docs/CLOUD.md` (Indexing, Identity and tokens).
