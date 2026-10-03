# gitbots roadmap

Next steps, grouped by owner. Everything is Rust, except the SolidJS dashboard
in `web/`.

## Priorities after the 2026-10-03 review

Three reviews fed this list: a readiness check against the Cloudflare
competition rules, a benchmark of the local side at 1k/10k/50k events, and a
benchmark of the hosted side against the live `gitbots-dev` Worker. Numbers
are measured. "50k" means a 50,000-event ledger.

### Gate

- **Competition eligibility.** Rules §3 require legal US/Canada residency. The
  contest is "void outside of the United States and Canada", and finalists
  present in person in San Francisco on Oct 21. Confirm with
  git-competition@cloudflare.com before investing further. Deadline: Oct 14,
  11:59 PM PDT.

### P0: correctness and security (judges will probe these)

1. **Enforce identity on the server.** Today "attested" holds only on the
   client: any agent holding the owner key can mint tokens for any session,
   push hosted `main`, queue its own review via `/api`, and forge or overwrite
   indexed events.
   - Separate agent keys from human keys. An agent key mints tokens only for
     its own session's fork; it can't call `/api` POSTs or `/v1/outbox`.
   - Give each agent session its own Artifacts fork. The steward is the only
     writer to `<prj>`.
   - The indexer accepts fork events only from that fork's session family, and
     rejects a known event id that arrives with different content.
   - Expose `attested` in `/api` and show it in the UI. Pin reviews to a head
     SHA. Add a unique pending-review index.
2. **Appends lost under contention.** With 4–8 long-lived writers, 2–19 appends
   failed and 300–515 commits were orphaned: the retry loop retries a lost race
   immediately, 5 times (`gitbots-git/src/ledger.rs:261-291`). Fix with
   backoff + jitter and a ~2 s deadline, or a cross-process lock in
   `<common-dir>/gitbots/locks/`. This bites exactly in the "many agents" demo.
3. **`/api/tip` misses late events.** It returns `max(id)`, so an event with an
   older ULID (a late push, a fork) never triggers a refresh. Return a version
   counter instead.
4. **Conflicts dead-end.** An accepted attempt whose merge conflicts can't be
   reviewed or resubmitted again. Add `attempt refresh` + resubmit, warn when
   open attempts overlap on paths, and add an integration-preview MVP with
   `git merge-tree`.

### P1: performance

The main cost is re-reading every event on each read. The folds themselves are
cheap (6–8 ms at 50k).

| Area | Problem | Now → expected | Fix | Effort |
|------|---------|----------------|-----|--------|
| local | every read re-parses all events | `status` 363 ms → ~20 ms at 50k | fold cache keyed by ledger tip; tree-diff old→new tip, parse only new files | M |
| local | auto-session folds the board inside git hooks | `git commit` 743 → ~58 ms at 50k | O(1) "session ended" check via an idem marker | S |
| local | `query_events` reads everything twice | `log -n 50` 674 → ~340 ms (→ ~15 with a backwards shard walk) | read once | S |
| local | `git config` subprocesses (7 ms each, 3–5 per command) | appends 32–49 → ~10 ms; tip poll → <1 ms | read config from gix | S |
| local | `gitbots ui` reopens the project and refolds per request | 370 → <5 ms per request at 50k | one `Project` + in-memory incremental fold; ETag on tip | M |
| local | one commit per event, never gc'd | 412 MB loose → 13 MB packed per 10k events | detached `git maintenance run --auto`; batch events | S |
| hosted | every `/api` call loads all events from D1 | flat ~65 ms regardless of size | indexer writes board/inbox/stats snapshots; `LIMIT` + filters in SQL | M |
| hosted | sequential Artifacts RPCs, N+1 on commits | attempt view 512 → ~170 ms | reuse `log()` commits; `join!` independent reads | S–M |
| hosted | diff reads blobs one at a time, uncached | large diffs >10× faster | ~16 concurrent reads; Cache API keyed by (base, head) | M |
| hosted | D1 owner-key lookup on every call | −22 ms per request | per-isolate key cache with a 60 s TTL | S |
| hosted | indexer: ~17 sequential RPCs per push, 5 s batch wait | push→indexed −4 s | use the push `before`/`after`; concurrent tree reads; `max_batch_timeout = 1` | M |
| web | each tip change refetches project, board and inbox (+ diff) | −75% calls on the attempt page | targeted refetch; `pending_outbox` moves into `/api/tip` | S |
| web | 3 s polling with no backoff | 0 polling cost, <1 s updates | backoff now; later one Durable Object per project + WebSocket | S / L |
| web | lists rebuilt on every refresh; large diffs and logs fully in the DOM | smooth at 10k+ rows | `reconcile` by id, `content-visibility`, virtualize logs and diffs | S / M |
| hosted | hashed `/assets/*` served with `max-age=0`; wasm not stripped | fewer revalidations; 958 → ~870 KB gzipped | `_headers` immutable; `strip = true` | S |

Not worth it: delta-base cache features in gix, `simd-json`.

### P2: product and competition

- **Claude Code hooks:**
  - `SessionStart` starts a session and persists `GITBOTS_SESSION` via `CLAUDE_ENV_FILE`. This fixes `unknown` models and two Claude instances sharing one auto-session.
  - `PostToolUse` feeds `gitbots trace --hook` from stdin.
  - Add a Codex snippet for AGENTS.md.
- **`scripts/swarm.sh`:** real headless agents (`claude -p`, `codex exec`) on the hosted project, plus `sync --watch`. The quick sync calls `/v1/ingest`. Set `CF_API_TOKEN` on `gitbots-dev` so push subscriptions work.
- **Run instructions:** release binaries (cargo-dist), and a README with three paths: local demo, join the hosted demo, self-host the Worker.
- **Hosted actions MVP:** on `attempt.submitted`, dispatch to Celesto or Cloudflare Sandbox. Otherwise, cut the claim from the submission.
- **Agent Commons:** ship a thin "new task from recipe" picker, or cut it from the pitch.
- **Video** (5–10 min, uploaded as a file) by Oct 12. Storyboard: concurrent agents → the ledger in git → workrooms/forks → attested identity → mandate refusals → conflicts → actions → human inbox → leaderboard → architecture.

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
