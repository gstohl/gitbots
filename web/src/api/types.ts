// Mirror of the types in docs/API.md (gitbots HTTP API v0). Keep this file in
// lockstep with the contract: change docs/API.md first, then the Rust server,
// then this file. Optional fields may be absent (not null), exactly as below.

export type AgentDescriptor = { provider: string; model: string; client: string; client_version?: string };
export type Actor =
  | { type: "human"; handle: string; email?: string }
  | { type: "agent"; session: string; agent: AgentDescriptor; parent?: string }
  | { type: "system"; component: string }
  | { type: "unknown" };
export type Via = "flag" | "env" | "worktree" | "mcp" | "git_config" | "tty" | "system" | "ui" | "other";

export type Session = {
  id: string;
  agent: AgentDescriptor;
  parent?: string;
  role?: string;
  operator?: string;
  external_id?: string;
  label?: string;
  started_at: string;
};
export type SessionView = { session: Session; ended: boolean };

export type DiffStat = { files: number; insertions: number; deletions: number };
export type RunStatus = "success" | "failure" | "timed_out" | "cancelled" | "skipped";
export type LogRef = { branch: string; path: string };
export type JobResult = {
  name: string;
  status: RunStatus;
  duration_ms: number;
  exit_code?: number;
  failed_step?: string;
  log?: LogRef;
};
export type ActionRun = {
  run: string;
  workflow: string;
  trigger: string;
  attempt?: string;
  commit?: string;
  runner: string;
  status: RunStatus;
  duration_ms: number;
  jobs: JobResult[];
};

export type HandoffTarget =
  | { type: "session"; session: string }
  | { type: "role"; role: string }
  | { type: "human"; handle: string };
export type ReviewDecision = "accept" | "reject" | "changes_requested";
export type ReportLevel = "info" | "warning" | "blocker";
export type Report = { title: string; body?: string; level: ReportLevel; task?: string; attempt?: string };

export type Event = {
  v: number;
  id: string;
  ts: string;
  actor: Actor;
  via?: Via;
  producer?: string;
  idem?: string;
  on?: string;
  kind: string;
  data: unknown; // see crates/gitbots-core/src/event.rs per kind (typed below as EventData)
};

export type TaskStatus = "open" | "in_progress" | "accepted" | "done";
export type TaskView = {
  id: string;
  title: string;
  body: string | null;
  recipe: string | null;
  labels: string[];
  created_by: Actor;
  created_at: string;
  attempts: string[];
};
export type AttemptState =
  | "active"
  | "submitted"
  | "changes_requested"
  | "accepted"
  | "rejected"
  | "merged"
  | "abandoned";
export type RunSummary = { run: string; workflow: string; status: RunStatus; commit: string | null };
export type ReviewView = { decision: ReviewDecision; reason: string | null; by: Actor; at: string };
export type AttemptView = {
  id: string;
  task: string;
  branch: string;
  base: string;
  base_commit: string;
  session: string | null;
  started_by: Actor;
  started_at: string;
  state: AttemptState;
  head: string | null;
  summary: string | null;
  diff: DiffStat | null;
  submitted_by: Actor | null;
  /** Latest submission time; absent from older servers. */
  submitted_at?: string | null;
  holder: HandoffTarget | null;
  runs: RunSummary[];
  review: ReviewView | null;
  merged_commit: string | null;
  updated_at: string;
};
export type ReportView = { id: string; at: string; by: Actor; report: Report };

export type BoardTask = TaskView & { status: TaskStatus };
export type BoardAttempt = AttemptView & { checks_passed: boolean | null };

export type BoardResponse = {
  tasks: BoardTask[];
  attempts: BoardAttempt[];
  sessions: SessionView[];
  awaiting_review: string[]; // attempt ids
  orphans: number;
};
export type Inbox = { awaiting_review: AttemptView[]; reports: ReportView[] }; // reports: blockers first, max 20

export type ActorStats = {
  sessions: number;
  subagent_sessions: number;
  tasks_created: number;
  attempts_started: number;
  attempts_submitted: number;
  accepted: number;
  rejected: number;
  changes_requested: number;
  merged: number;
  abandoned: number;
  handoffs: number;
  commits: number;
  lines_added: number;
  lines_removed: number;
  runs: number;
  runs_passed: number;
  reports: number;
  tool_calls: number;
  tool_failures: number;
};
export type Stats = { by_actor: Record<string, ActorStats> }; // key: "provider/model@client" or "@handle"

export type Approver = "any" | "reviewer" | "maintainer" | "owner";
export type PrincipalRole = "reviewer" | "maintainer" | "owner";
export type Manifest = {
  version: number;
  project: { id: string; name: string; description?: string };
  tenancy: { owner: { kind: "user" | "org"; handle: string }; team: string | null; workspace: string | null };
  mandate: {
    goal?: string;
    autonomy: "supervised" | "assisted" | "autonomous";
    principals: { handle: string; email?: string; role: PrincipalRole }[];
    agents: { allowed_paths: string[]; denied_paths: string[]; protected_branches: string[] };
    approvals: {
      accept_attempt: Approver;
      merge_protected: Approver;
      change_mandate: Approver;
      run_hosted_action: Approver;
    };
  };
  ledger: { activity_branch: string; logs_branch: string };
  workrooms: { branch_prefix: string };
};
export type ManifestSource = { type: "trusted"; branch: string; oid: string } | { type: "working_tree" };
export type ProjectInfo = {
  manifest: Manifest;
  manifest_source: ManifestSource;
  trusted_branch: string;
  viewer: Actor | null; // the git-config human the UI acts as
  can_decide: boolean;
  producer: string; // "gitbots/0.1.0"
  // Hosted (Cloudflare Worker, docs/CLOUD.md "Dashboard API") only:
  hosted?: boolean;
  pending_outbox?: number; // decisions queued in the outbox, applied by the next `gitbots sync`
};

export type CommitInfo = { sha: string; subject: string; trailers: [string, string][] };
export type AttemptDetail = {
  attempt: BoardAttempt;
  task: BoardTask;
  workroom: string | null;
  violations: { path: string; reason: string }[];
  commits: CommitInfo[]; // base..head, oldest first
  runs: ActionRun[]; // full results, newest last
  events: Event[]; // everything about this attempt, oldest first
  session_chain: Session[]; // bound session, then its ancestors
};
export type ReviewOutcome = { attempt: string; decision: ReviewDecision; merged: string | null };

// ---- request bodies and small responses -----------------------------------

export type TipResponse = { activity: string | null };
export type NewTask = { title: string; body?: string; labels?: string[] };
export type NewTaskResponse = { task: string };
export type ReviewRequest = { decision: ReviewDecision; reason?: string; merge?: boolean };
export type WorkflowsResponse = {
  workflows: { path: string; workflow: unknown }[];
  invalid: { path: string; error: string }[];
};
export type RecipesResponse = ({ path: string; recipe: unknown } | { path: string; error: string })[];
export type ApiErrorBody = { error: string };
/** Hosted only: a write was stored in the outbox (HTTP 202) and is applied by the next `gitbots sync`. */
export type QueuedResponse = { queued: true; outbox: string };

export type EventQuery = { kind?: string; session?: string; task?: string; attempt?: string; limit?: number };

// ---- per-kind event payloads (crates/gitbots-core/src/event.rs) ---------------

export type ProjectInitialized = { project: string; name: string };
export type SessionStarted = { session: Session };
export type SessionEnded = { session: string; summary?: string };
export type TaskCreated = { task: string; title: string; body?: string; recipe?: string; labels?: string[] };
export type AttemptStarted = {
  task: string;
  attempt: string;
  branch: string;
  base: string;
  base_commit: string;
  session?: string;
};
export type AttemptSubmitted = { attempt: string; head: string; summary?: string; diff?: DiffStat; mandate?: string };
export type AttemptHandoff = { attempt: string; to: HandoffTarget; note?: string };
export type AttemptAbandoned = { attempt: string; reason?: string };
export type AttemptMerged = { attempt: string; into: string; commit: string; source_commits?: string[] };
export type ReviewDecided = { attempt: string; decision: ReviewDecision; reason?: string; mandate?: string };
export type CommitRecorded = { sha: string; subject: string; branch?: string; attempt?: string; diff?: DiffStat };
export type ToolCalled = { tool: string; input?: string; ok: boolean; duration_ms?: number; log?: LogRef };

/** `kind` -> payload, for every kind in `kind::ALL`. */
export type EventDataMap = {
  "project.initialized": ProjectInitialized;
  "session.started": SessionStarted;
  "session.ended": SessionEnded;
  "task.created": TaskCreated;
  "attempt.started": AttemptStarted;
  "attempt.submitted": AttemptSubmitted;
  "attempt.handoff": AttemptHandoff;
  "attempt.abandoned": AttemptAbandoned;
  "attempt.merged": AttemptMerged;
  "review.decided": ReviewDecided;
  "commit.recorded": CommitRecorded;
  "action.completed": ActionRun;
  report: Report;
  "tool.called": ToolCalled;
};
export type KnownKind = keyof EventDataMap;

/** Every kind this build understands, in `kind::ALL` order. */
export const KNOWN_KINDS = [
  "project.initialized",
  "session.started",
  "session.ended",
  "task.created",
  "attempt.started",
  "attempt.submitted",
  "attempt.handoff",
  "attempt.abandoned",
  "attempt.merged",
  "review.decided",
  "commit.recorded",
  "action.completed",
  "report",
  "tool.called",
] as const satisfies readonly KnownKind[];

/** A fixture/test helper type: an event whose `data` matches its `kind`. */
export type TypedEvent<K extends KnownKind = KnownKind> = K extends KnownKind
  ? Omit<Event, "kind" | "data"> & { kind: K; data: EventDataMap[K] }
  : never;
