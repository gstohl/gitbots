# gitbots HTTP API (v0)

The JSON API behind the human frontend (`web/`). Today `gitbots ui` serves it
locally; later the hosted Cloudflare layer serves the same contract.
Payload types are the `serde` shapes of the Rust types in `gitbots-core`, so the
CLI's `--json` output, the MCP tools and this API agree.

## Transport and auth

- `gitbots ui [--port 7777] [--assets DIR]` binds **127.0.0.1 only** and prints
  `http://127.0.0.1:7777/#token=<hex>`.
- Every `/api/*` request needs `Authorization: Bearer <token>`. The frontend
  reads the token from `location.hash` on first load, keeps it in
  `sessionStorage`, and removes it from the URL.
- Requests whose `Host` isn't `127.0.0.1:<port>`, `localhost:<port>` or
  `[::1]:<port>` are rejected (DNS-rebinding guard).
- Errors come back with a non-2xx status and the body `{"error": "message"}`,
  including malformed requests (400, 415, 422). 401 means a missing or bad
  token; 403 means a refused decision (mandate, self-review, read-only server);
  404 means unknown; 409 means a wrong state (e.g. reviewing an attempt that
  isn't `submitted`) or a merge conflict; 422 means invalid input.
- Writes act as the git-config human, recorded with `via: "ui"`. If `gitbots ui`
  was started by an agent (`CLAUDECODE`, `CODEX_*` or `GITBOTS_SESSION` in its
  env), `project.can_decide` is false and every write returns 403.

## Endpoints

| method | path                         | response                                         |
|--------|------------------------------|--------------------------------------------------|
| GET    | `/api/project`               | `ProjectInfo`                                    |
| GET    | `/api/tip`                   | `{ "activity": string \| null }` (poll it; refetch when it changes) |
| GET    | `/api/board`                 | `BoardResponse`                                  |
| GET    | `/api/inbox`                 | `Inbox`                                          |
| GET    | `/api/events`                | `Event[]`, oldest first. Query: `kind`, `session`, `task`, `attempt`, `limit` (default 200) |
| GET    | `/api/stats`                 | `Stats`                                          |
| GET    | `/api/attempts/{id}`         | `AttemptDetail` (`id` may be a full id, a short id or a branch name; percent-encode the `/` in branch names) |
| GET    | `/api/attempts/{id}/diff`    | `text/plain` unified diff `base...head`, capped at 2 MB |
| GET    | `/api/logs?path=…`           | `text/plain` blob from the logs branch (`LogRef.path`) |
| GET    | `/api/workflows`             | `{ workflows: {path, workflow}[], invalid: {path, error}[] }` |
| GET    | `/api/recipes`               | `({path, recipe} \| {path, error})[]`            |
| POST   | `/api/tasks`                 | body `{title, body?, labels?}` → `{ task: string }` |
| POST   | `/api/attempts/{id}/review`  | body `{decision, reason?, merge?}` → `ReviewOutcome` |

`decision` is `"accept" | "reject" | "changes_requested"`.

## Types

TypeScript notation. Ids are prefixed ULID strings: `ses_`, `tsk_`, `att_`,
`evt_`, `run_` and `prj_`. A short id is the last 6 characters, lowercased.
Times are RFC 3339 strings. Optional fields may be **absent**, not `null`.

```ts
type AgentDescriptor = { provider: string; model: string; client: string; client_version?: string };
type Actor =
  | { type: "human"; handle: string; email?: string }
  | { type: "agent"; session: string; agent: AgentDescriptor; parent?: string }
  | { type: "system"; component: string }
  | { type: "unknown" };
type Via = "flag" | "env" | "worktree" | "mcp" | "git_config" | "tty" | "system" | "ui" | "other";

type Session = {
  id: string; agent: AgentDescriptor; parent?: string; role?: string;
  operator?: string; external_id?: string; label?: string; started_at: string;
};
type SessionView = { session: Session; ended: boolean };

type DiffStat = { files: number; insertions: number; deletions: number };
type RunStatus = "success" | "failure" | "timed_out" | "cancelled" | "skipped";
type LogRef = { branch: string; path: string };
type JobResult = { name: string; status: RunStatus; duration_ms: number; exit_code?: number; failed_step?: string; log?: LogRef };
type ActionRun = {
  run: string; workflow: string; trigger: string; attempt?: string; commit?: string;
  runner: string; status: RunStatus; duration_ms: number; jobs: JobResult[];
};

type HandoffTarget =
  | { type: "session"; session: string } | { type: "role"; role: string } | { type: "human"; handle: string };
type ReviewDecision = "accept" | "reject" | "changes_requested";
type ReportLevel = "info" | "warning" | "blocker";
type Report = { title: string; body?: string; level: ReportLevel; task?: string; attempt?: string };

type Event = {
  v: number; id: string; ts: string; actor: Actor; via?: Via; producer?: string;
  idem?: string; on?: string; kind: string; data: unknown;   // see crates/gitbots-core/src/event.rs per kind
};

type TaskStatus = "open" | "in_progress" | "accepted" | "done";
type TaskView = {
  id: string; title: string; body: string | null; recipe: string | null; labels: string[];
  created_by: Actor; created_at: string; attempts: string[];
};
type AttemptState = "active" | "submitted" | "changes_requested" | "accepted" | "rejected" | "merged" | "abandoned";
type RunSummary = { run: string; workflow: string; status: RunStatus; commit: string | null };
type ReviewView = { decision: ReviewDecision; reason: string | null; by: Actor; at: string };
type AttemptView = {
  id: string; task: string; branch: string; base: string; base_commit: string; session: string | null;
  started_by: Actor; started_at: string; state: AttemptState; head: string | null; summary: string | null;
  diff: DiffStat | null; submitted_by: Actor | null; submitted_at: string | null;
  holder: HandoffTarget | null; runs: RunSummary[];
  review: ReviewView | null; merged_commit: string | null; updated_at: string;
};
type ReportView = { id: string; at: string; by: Actor; report: Report };

type BoardResponse = {
  tasks: (TaskView & { status: TaskStatus })[];
  attempts: (AttemptView & { checks_passed: boolean | null })[];
  sessions: SessionView[];
  awaiting_review: string[];          // attempt ids
  orphans: number;
};
type Inbox = { awaiting_review: AttemptView[]; reports: ReportView[] };   // reports: blockers first, max 20

type ActorStats = {
  sessions: number; subagent_sessions: number; tasks_created: number; attempts_started: number;
  attempts_submitted: number; accepted: number; rejected: number; changes_requested: number;
  merged: number; abandoned: number; handoffs: number; commits: number; lines_added: number;
  lines_removed: number; runs: number; runs_passed: number; reports: number; tool_calls: number; tool_failures: number;
};
type Stats = { by_actor: Record<string, ActorStats> };   // key: "provider/model@client" or "@handle"

type Approver = "any" | "reviewer" | "maintainer" | "owner";
type Manifest = {
  version: number;
  project: { id: string; name: string; description?: string };
  tenancy: { owner: { kind: "user" | "org"; handle: string }; team: string | null; workspace: string | null };
  mandate: {
    goal?: string; autonomy: "supervised" | "assisted" | "autonomous";
    principals: { handle: string; email?: string; role: "reviewer" | "maintainer" | "owner" }[];
    agents: { allowed_paths: string[]; denied_paths: string[]; protected_branches: string[] };
    approvals: { accept_attempt: Approver; merge_protected: Approver; change_mandate: Approver; run_hosted_action: Approver };
  };
  ledger: { activity_branch: string; logs_branch: string };
  workrooms: { branch_prefix: string };
};
type ProjectInfo = {
  manifest: Manifest;
  manifest_source: { type: "trusted"; branch: string; oid: string } | { type: "working_tree" };
  trusted_branch: string;
  viewer: Actor | null;     // the git-config human the UI acts as
  can_decide: boolean;
  producer: string;         // "gitbots/0.1.0"
};

type CommitInfo = { sha: string; subject: string; trailers: [string, string][] };
type AttemptDetail = {
  attempt: AttemptView & { checks_passed: boolean | null };
  task: TaskView & { status: TaskStatus };
  workroom: string | null;
  violations: { path: string; reason: string }[];
  commits: CommitInfo[];        // base..head, oldest first
  runs: ActionRun[];            // full results, newest last
  events: Event[];              // everything about this attempt, oldest first
  session_chain: Session[];     // bound session, then its ancestors
};
type ReviewOutcome = { attempt: string; decision: ReviewDecision; event: string; merged: string | null };
// Hosted only (docs/CLOUD.md): POST /api/tasks and /api/attempts/{id}/review may
// instead answer 202 with this, and ProjectInfo gains `hosted?: true` and
// `pending_outbox?: number`.
type Queued = { queued: true; outbox: string };
```
