// Contract test: real responses captured from `gitbots ui` (npm run
// snapshot:real) must match src/api/types.ts.
//
// Type level: each JSON is assigned to its API type with string-literal
// unions widened to `string` (JSON imports lose literal types), so a missing
// field or a wrong primitive type fails `npm run typecheck`.
// Runtime: the literal unions (states, kinds, actor types...) are checked
// here, and so are keys the contract doesn't know about.

import { describe, expect, it } from "vitest";
import type {
  ActorStats,
  AttemptDetail,
  BoardResponse,
  Event,
  Inbox,
  NewTaskResponse,
  ProjectInfo,
  RecipesResponse,
  ReviewOutcome,
  Stats,
  TipResponse,
  WorkflowsResponse,
} from "../../api/types";
import { KNOWN_KINDS } from "../../api/types";
import { parseUnifiedDiff, diffTotals } from "../../lib/diff";
import { describeEvent } from "../../lib/events";
import { logLines } from "../../lib/log";
import { statsRows } from "../../lib/stats";
import attemptActive from "./attempt-active.json";
import attemptChanges from "./attempt-changes_requested.json";
import attemptMerged from "./attempt-merged.json";
import attemptSubmitted from "./attempt-submitted.json";
import diffText from "./attempt.diff?raw";
import board from "./board.json";
import errors from "./errors.json";
import events from "./events.json";
import inbox from "./inbox.json";
import jobLog from "./job.log?raw";
import postReview from "./post-review.json";
import postTask from "./post-task.json";
import projectReadonly from "./project-readonly.json";
import project from "./project.json";
import recipes from "./recipes.json";
import stats from "./stats.json";
import tip from "./tip.json";
import workflows from "./workflows.json";

/** T with string-literal unions widened to string (what a JSON import is typed as). */
type Widen<T> = T extends string
  ? string
  : T extends number
    ? number
    : T extends boolean
      ? boolean
      : T extends null | undefined
        ? T
        : T extends readonly (infer U)[]
          ? Widen<U>[]
          : T extends object
            ? { [K in keyof T]: Widen<T[K]> }
            : T;

// ---- type level (checked by tsc) ----
const typed = {
  project: project satisfies Widen<ProjectInfo>,
  projectReadonly: projectReadonly satisfies Widen<ProjectInfo>,
  tip: tip satisfies Widen<TipResponse>,
  board: board satisfies Widen<BoardResponse>,
  inbox: inbox satisfies Widen<Inbox>,
  events: events satisfies Widen<Event[]>,
  stats: stats satisfies Widen<Stats>,
  workflows: workflows satisfies Widen<WorkflowsResponse>,
  recipes: recipes satisfies Widen<RecipesResponse>,
  attempts: [attemptActive, attemptChanges, attemptMerged, attemptSubmitted] satisfies Widen<AttemptDetail>[],
  postTask: postTask satisfies Widen<NewTaskResponse>,
  postReview: postReview satisfies Widen<ReviewOutcome>,
};

// ---- runtime: literal unions ----
const UNIONS = {
  actorType: ["human", "agent", "system", "unknown"],
  via: ["flag", "env", "worktree", "mcp", "git_config", "tty", "system", "ui", "other"],
  attemptState: ["active", "submitted", "changes_requested", "accepted", "rejected", "merged", "abandoned"],
  taskStatus: ["open", "in_progress", "accepted", "done"],
  runStatus: ["success", "failure", "timed_out", "cancelled", "skipped"],
  decision: ["accept", "reject", "changes_requested"],
  level: ["info", "warning", "blocker"],
  approver: ["any", "reviewer", "maintainer", "owner"],
  role: ["reviewer", "maintainer", "owner"],
  autonomy: ["supervised", "assisted", "autonomous"],
  ownerKind: ["user", "org"],
  sourceType: ["trusted", "working_tree"],
  holderType: ["session", "role", "human"],
};
type U = keyof typeof UNIONS;
const ok = (u: U, v: unknown) => expect(UNIONS[u], `${u}: ${String(v)}`).toContain(v);

type Any = Record<string, unknown> & { [k: string]: any }; // eslint-disable-line @typescript-eslint/no-explicit-any
function actor(a: Any | null) {
  if (a) ok("actorType", a.type);
}
function attemptView(a: Any) {
  ok("attemptState", a.state);
  actor(a.started_by);
  actor(a.submitted_by);
  if (a.holder) ok("holderType", a.holder.type);
  for (const r of a.runs) ok("runStatus", r.status);
  if (a.review) {
    ok("decision", a.review.decision);
    actor(a.review.by);
  }
}
function event(e: Any) {
  actor(e.actor);
  if (e.via !== undefined) ok("via", e.via);
}

// ---- runtime: keys outside the contract ----
// Keys each object may carry per docs/API.md. Anything else is reported.
const KEYS = {
  project: ["manifest", "manifest_source", "trusted_branch", "viewer", "can_decide", "producer", "hosted", "pending_outbox"],
  attempt: [
    "id", "task", "branch", "base", "base_commit", "session", "started_by", "started_at", "state", "head", "summary",
    "diff", "submitted_by", "holder", "runs", "review", "merged_commit", "updated_at", "checks_passed",
  ],
  task: ["id", "title", "body", "recipe", "labels", "created_by", "created_at", "attempts", "status"],
  event: ["v", "id", "ts", "actor", "via", "producer", "idem", "on", "kind", "data"],
  detail: ["attempt", "task", "workroom", "violations", "commits", "runs", "events", "session_chain"],
  reviewOutcome: ["attempt", "decision", "merged"],
};
/**
 * Known server extras not (yet) in docs/API.md. Reported to the API owner;
 * listed here so they're visible rather than silently ignored.
 */
const REPORTED_EXTRAS: Partial<Record<keyof typeof KEYS, string[]>> = {
  reviewOutcome: ["event"],
};
function keys(kind: keyof typeof KEYS, o: object) {
  const allowed = new Set([...KEYS[kind], ...(REPORTED_EXTRAS[kind] ?? [])]);
  const extra = Object.keys(o).filter((k) => !allowed.has(k));
  expect(extra, `${kind} has keys outside docs/API.md`).toEqual([]);
}

describe("real gitbots ui responses", () => {
  it("project", () => {
    for (const p of [typed.project, typed.projectReadonly] as Any[]) {
      keys("project", p);
      ok("sourceType", p.manifest_source.type);
      ok("autonomy", p.manifest.mandate.autonomy);
      ok("ownerKind", p.manifest.tenancy.owner.kind);
      for (const v of Object.values(p.manifest.mandate.approvals)) ok("approver", v);
      for (const pr of p.manifest.mandate.principals) ok("role", pr.role);
      actor(p.viewer);
    }
    expect(typed.project.can_decide).toBe(true);
    expect(typed.projectReadonly.can_decide).toBe(false);
  });

  it("board and inbox", () => {
    const b = typed.board as Any;
    for (const t of b.tasks) {
      keys("task", t);
      ok("taskStatus", t.status);
      actor(t.created_by);
    }
    for (const a of b.attempts) {
      keys("attempt", a);
      attemptView(a);
    }
    for (const id of b.awaiting_review) expect(b.attempts.some((a: Any) => a.id === id)).toBe(true);
    const i = typed.inbox as Any;
    for (const a of i.awaiting_review) attemptView(a);
    for (const r of i.reports) {
      ok("level", r.report.level);
      actor(r.by);
    }
    // Blockers first.
    const levels = i.reports.map((r: Any) => r.report.level as string);
    expect(levels.indexOf("blocker")).toBeLessThanOrEqual(Math.max(0, levels.lastIndexOf("blocker")));
    if (levels.includes("blocker")) expect(levels[0]).toBe("blocker");
  });

  it("events are oldest first, known kinds and describable", () => {
    const es = typed.events as unknown as Event[];
    expect(es.length).toBeGreaterThan(0);
    const ids = es.map((e) => e.id);
    expect(ids).toEqual([...ids].sort());
    for (const e of es) {
      keys("event", e);
      event(e as unknown as Any);
      expect(KNOWN_KINDS as readonly string[]).toContain(e.kind);
      expect(describeEvent(e).known, `${e.kind} payload parses`).toBe(true);
    }
  });

  it("attempt details", () => {
    for (const d of typed.attempts as Any[]) {
      keys("detail", d);
      keys("attempt", d.attempt);
      attemptView(d.attempt);
      for (const r of d.runs) {
        ok("runStatus", r.status);
        for (const j of r.jobs) ok("runStatus", j.status);
      }
      for (const e of d.events) {
        event(e);
        expect(e.data.attempt ?? d.attempt.id).toBe(d.attempt.id);
      }
      for (const c of d.commits) expect(Array.isArray(c.trailers) && c.trailers.every((t: unknown[]) => t.length === 2)).toBe(true);
      // Bound session first, then its ancestors.
      if (d.session_chain.length) expect(d.session_chain[0].id).toBe(d.attempt.session);
    }
  });

  it("diff and log text", () => {
    const files = parseUnifiedDiff(diffText);
    expect(files.length).toBeGreaterThan(0);
    const s = (typed.attempts[3] as Any).attempt.diff;
    const t = diffTotals(files);
    expect({ files: t.files, insertions: t.additions, deletions: t.deletions }).toEqual(s);
    expect(logLines(jobLog).length).toBeGreaterThan(0);
  });

  it("stats", () => {
    const rows = statsRows(typed.stats as unknown as Stats);
    expect(rows.some((r) => r.kind === "agent")).toBe(true);
    for (const r of rows) {
      for (const k of Object.keys(r.stats)) expect(typeof (r.stats as unknown as Record<string, unknown>)[k]).toBe("number");
      const s: ActorStats = r.stats;
      expect(s.accepted + s.rejected + s.changes_requested).toBeGreaterThanOrEqual(0);
      if (r.acceptance !== null) expect(r.acceptance).toBeLessThanOrEqual(1);
    }
  });

  it("writes", () => {
    expect(typeof typed.postTask.task).toBe("string");
    keys("reviewOutcome", typed.postReview);
    ok("decision", typed.postReview.decision);
  });

  it("errors use {error} JSON, except where reported", () => {
    // Framework-level rejections (bad JSON, wrong content type, bad enum,
    // missing query param) come back as text/plain, not {error}. Reported.
    const textOnly = ["review_bad_decision", "task_bad_json", "task_no_content_type", "log_missing_path"];
    for (const [name, r] of Object.entries(errors as Record<string, { status: number; body: unknown }>)) {
      expect(r.status).toBeGreaterThanOrEqual(400);
      if (textOnly.includes(name)) expect(typeof r.body).toBe("string");
      else expect(r.body, name).toMatchObject({ error: expect.any(String) });
    }
  });
});
