// In-browser stand-in for `gitbots ui`, used when VITE_GITBOTS_MOCK=1. It serves the
// fixtures through real `Response` objects so the client's code paths (auth,
// error parsing, text bodies) are exercised exactly as against the server.
// Writes (new task, review) mutate the in-memory world and move the tip.

import type {
  Actor,
  AttemptDetail,
  AttemptState,
  AttemptView,
  BoardResponse,
  Event,
  Inbox,
  NewTask,
  PrincipalRole,
  ReportLevel,
  ReviewDecision,
  ReviewOutcome,
  ReviewRequest,
  Session,
  TaskStatus,
} from "../api/types";
import { globMatch } from "../lib/mandate";
import { buildWorld, type MockWorld } from "./fixtures";
import { idAt, sha, stableId } from "./ids";

export type Scenario = "default" | "hosted" | "agent-viewer" | "reviewer-viewer" | "inbox-zero" | "unauthorized" | "slow";
export const SCENARIOS: { id: Scenario; label: string }[] = [
  { id: "default", label: "Default (you are @gstohl, owner)" },
  { id: "hosted", label: "Hosted (writes queue, steward every 10 s)" },
  { id: "reviewer-viewer", label: "Viewer is @priya (reviewer)" },
  { id: "agent-viewer", label: "Started by an agent (read-only)" },
  { id: "inbox-zero", label: "Inbox zero" },
  { id: "slow", label: "Slow network (1.5 s)" },
  { id: "unauthorized", label: "Bad token (401)" },
];
const SCENARIO_KEY = "gitbots.mock.scenario";

export function getScenario(): Scenario {
  try {
    const v = sessionStorage.getItem(SCENARIO_KEY);
    if (v && SCENARIOS.some((s) => s.id === v)) return v as Scenario;
  } catch {
    /* ignore */
  }
  return "default";
}

export function setScenario(s: Scenario): void {
  try {
    sessionStorage.setItem(SCENARIO_KEY, s);
  } catch {
    /* ignore */
  }
}

let world: MockWorld | null = null;
let scenario: Scenario = "default";
let ticker: ReturnType<typeof setInterval> | undefined;

/** Hosted only: the Worker's outbox (docs/CLOUD.md), applied by `gitbots sync`. */
type OutboxItem =
  | { id: string; kind: "task.create"; body: NewTask; actor: Actor }
  | { id: string; kind: "review"; attempt: string; body: ReviewRequest; actor: Actor };
let outbox: OutboxItem[] = [];

/** Replace the world (tests). Pass a scenario to apply its tweaks. */
export function resetWorld(w: MockWorld = buildWorld(), s: Scenario = "default"): MockWorld {
  scenario = s;
  outbox = [];
  world = applyScenario(w, s);
  return world;
}

function applyScenario(w: MockWorld, s: Scenario): MockWorld {
  if (s === "hosted") {
    w.project.hosted = true;
    // One decision someone queued earlier from another browser.
    outbox.push({
      id: "obx_01K6PRELOADED0000000000001",
      kind: "task.create",
      body: { title: "Bump tokio to 1.48", labels: ["deps"] },
      actor: { type: "human", handle: "santosh", email: "santosh@gitbots.dev" },
    });
    w.project.pending_outbox = outbox.length;
  }
  if (s === "agent-viewer") w.project.can_decide = false;
  if (s === "reviewer-viewer") w.project.viewer = { type: "human", handle: "priya" };
  if (s === "inbox-zero") {
    for (const a of w.attempts) {
      if (a.state === "submitted") {
        a.state = "accepted";
        a.review = { decision: "accept", reason: null, by: { type: "human", handle: "gstohl" }, at: a.updated_at };
      }
    }
    w.reports = [];
  }
  return w;
}

function getWorld(): MockWorld {
  if (!world) {
    resetWorld(buildWorld(), getScenario());
    startTicker();
    startSteward();
  }
  return world!;
}

/**
 * Hosted: plays `gitbots sync --watch`, applying the oldest outbox item. Exported
 * for tests. Returns the applied item's id, if any.
 */
export function runSteward(): string | null {
  const w = world;
  const item = outbox.shift();
  if (!w || !item) return null;
  if (item.kind === "task.create") applyTask(w, item.body, item.actor);
  else applyReview(w, item.attempt, item.body, item.actor);
  w.project.pending_outbox = outbox.length;
  return item.id;
}

function startSteward() {
  if (scenario !== "hosted" || typeof window === "undefined" || import.meta.env.MODE === "test") return;
  setInterval(() => {
    if (outbox.length) runSteward();
  }, 10_000);
}

/** Simulated agent activity every 20 s so the live indicator has something to show. */
function startTicker() {
  if (ticker || typeof window === "undefined" || import.meta.env.MODE === "test") return;
  const tools = ["Read", "Edit", "Bash", "Grep", "mcp__gitbots__report"];
  let i = 0;
  ticker = setInterval(() => {
    const w = world;
    if (!w || scenario === "inbox-zero") return;
    const open = w.sessions.filter((s) => !s.ended);
    const s = open[i % Math.max(open.length, 1)]?.session;
    if (!s) return;
    const tool = tools[i % tools.length]!;
    pushEvent(w, actorOf(s), "worktree", "tool.called", {
      tool,
      input: tool === "Bash" ? "cargo test --workspace" : `src/${["main.rs", "config.rs", "webhooks/retry.rs"][i % 3]}`,
      ok: i % 5 !== 3,
      duration_ms: 200 + ((i * 7919) % 9000),
    });
    i++;
  }, 20_000);
}

// ---- helpers -----------------------------------------------------------------------

const json = (status: number, body: unknown) =>
  new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
const text = (status: number, body: string) =>
  new Response(body, { status, headers: { "Content-Type": "text/plain; charset=utf-8" } });
const fail = (status: number, error: string) => json(status, { error });

function actorOf(s: Session): Actor {
  return s.parent
    ? { type: "agent", session: s.id, agent: s.agent, parent: s.parent }
    : { type: "agent", session: s.id, agent: s.agent };
}

function pushEvent(w: MockWorld, actor: Actor, via: Event["via"], kind: string, data: unknown, on?: string): Event {
  const now = Date.now();
  const e: Event = { v: 1, id: idAt("evt", now, `${kind}:${now}:${Math.random()}`), ts: new Date(now).toISOString(), actor, producer: "gitbots/0.1.0", kind, data };
  if (via) e.via = via;
  if (on) e.on = on;
  w.events.push(e);
  return e;
}

function tip(w: MockWorld): string | null {
  const last = w.events[w.events.length - 1];
  return last ? `${sha(last.id)}` : null;
}

function findAttempt(w: MockWorld, q: string) {
  const lower = q.toLowerCase();
  return w.attempts.find((a) => a.id === q || a.branch === q || a.id.slice(-6).toLowerCase() === lower || a.id.toLowerCase().endsWith(lower));
}

function dataField(e: Event, key: string): unknown {
  return typeof e.data === "object" && e.data !== null ? (e.data as Record<string, unknown>)[key] : undefined;
}

function eventSession(e: Event): string | undefined {
  if (e.actor.type === "agent") return e.actor.session;
  return undefined;
}

function eventAttempt(e: Event): string | undefined {
  const a = dataField(e, "attempt");
  return typeof a === "string" ? a : undefined;
}

function eventTask(w: MockWorld, e: Event): string | undefined {
  const t = dataField(e, "task");
  if (typeof t === "string") return t;
  const a = eventAttempt(e);
  return a ? w.attempts.find((x) => x.id === a)?.task : undefined;
}

function strip(a: MockWorld["attempts"][number]): AttemptView {
  const { checks_passed: _ignored, ...rest } = a;
  return rest;
}

const LEVEL_ORDER: Record<ReportLevel, number> = { blocker: 0, warning: 1, info: 2 };
const RANK: Record<PrincipalRole, number> = { reviewer: 1, maintainer: 2, owner: 3 };

function role(w: MockWorld): PrincipalRole | null {
  const v = w.project.viewer;
  if (!v || v.type !== "human") return null;
  return w.project.manifest.mandate.principals.find((p) => p.handle === v.handle)?.role ?? null;
}

function needs(w: MockWorld, approver: "any" | PrincipalRole): string | null {
  if (approver === "any") return null;
  const r = role(w);
  if (r && RANK[r] >= RANK[approver]) return null;
  return `needs a human with role ${approver}`;
}

function recomputeTaskStatus(w: MockWorld, taskId: string) {
  const t = w.tasks.find((x) => x.id === taskId);
  if (!t) return;
  const states = w.attempts.filter((a) => a.task === taskId).map((a) => a.state);
  let status: TaskStatus = "open";
  if (states.includes("merged")) status = "done";
  else if (states.includes("accepted")) status = "accepted";
  else if (states.some((s) => s === "active" || s === "submitted" || s === "changes_requested")) status = "in_progress";
  t.status = status;
}

// ---- writes (applied directly, or by the steward when hosted) ----------------------------

function applyTask(w: MockWorld, t: NewTask, actor: Actor): string {
  const id = stableId("tsk", `new-${Date.now()}-${Math.random()}`);
  const now = new Date().toISOString();
  w.tasks.push({ id, title: t.title, body: t.body ?? null, recipe: null, labels: t.labels ?? [], created_by: actor, created_at: now, attempts: [], status: "open" });
  const data: Record<string, unknown> = { task: id, title: t.title };
  if (t.body) data.body = t.body;
  if (t.labels?.length) data.labels = t.labels;
  pushEvent(w, actor, "ui", "task.created", data);
  return id;
}

function applyReview(w: MockWorld, attemptId: string, r: ReviewRequest, actor: Actor): ReviewOutcome {
  const a = w.attempts.find((x) => x.id === attemptId);
  if (!a || a.state !== "submitted") return { attempt: attemptId, decision: r.decision, merged: null };
  const submitted = [...w.events].reverse().find((e) => e.kind === "attempt.submitted" && eventAttempt(e) === a.id);
  const data: Record<string, unknown> = { attempt: a.id, decision: r.decision, mandate: sha("manifest") };
  if (r.reason) data.reason = r.reason;
  pushEvent(w, actor, "ui", "review.decided", data, submitted?.id);
  const now = new Date().toISOString();
  const next: Record<ReviewDecision, AttemptState> = { accept: "accepted", reject: "rejected", changes_requested: "changes_requested" };
  a.state = next[r.decision];
  a.review = { decision: r.decision, reason: r.reason ?? null, by: actor, at: now };
  a.updated_at = now;
  let merged: string | null = null;
  if (r.decision === "accept" && r.merge) {
    merged = sha(`merge-${a.id}-${now}`);
    a.state = "merged";
    a.merged_commit = merged;
    pushEvent(w, actor, "ui", "attempt.merged", { attempt: a.id, into: a.base, commit: merged });
  }
  recomputeTaskStatus(w, a.task);
  return { attempt: a.id, decision: r.decision, merged };
}

// ---- routes -----------------------------------------------------------------------------

async function route(w: MockWorld, method: string, url: URL, body: unknown): Promise<Response> {
  const p = url.pathname;
  const q = url.searchParams;

  if (method === "GET") {
    if (p === "/api/project") return json(200, w.project);
    if (p === "/api/tip") return json(200, { activity: tip(w) });
    if (p === "/api/board") {
      const board: BoardResponse = {
        tasks: w.tasks,
        attempts: w.attempts,
        sessions: w.sessions,
        awaiting_review: w.attempts.filter((a) => a.state === "submitted").map((a) => a.id),
        orphans: w.orphans,
      };
      return json(200, board);
    }
    if (p === "/api/inbox") {
      const inbox: Inbox = {
        awaiting_review: w.attempts.filter((a) => a.state === "submitted").map(strip),
        reports: [...w.reports]
          .sort((a, b) => LEVEL_ORDER[a.report.level] - LEVEL_ORDER[b.report.level] || b.at.localeCompare(a.at))
          .slice(0, 20),
      };
      return json(200, inbox);
    }
    if (p === "/api/events") {
      const kind = q.get("kind");
      const session = q.get("session");
      const task = q.get("task");
      const attempt = q.get("attempt");
      const limit = Number(q.get("limit") ?? 200) || 200;
      const out = w.events.filter(
        (e) =>
          (!kind || e.kind === kind || e.kind.startsWith(`${kind}.`)) &&
          (!session || eventSession(e) === session || dataField(e, "session") === session) &&
          (!task || eventTask(w, e) === task) &&
          (!attempt || eventAttempt(e) === attempt),
      );
      return json(200, out.slice(-limit));
    }
    if (p === "/api/stats") return json(200, w.stats);
    if (p === "/api/workflows") return json(200, w.workflows);
    if (p === "/api/recipes") return json(200, w.recipes);
    if (p === "/api/logs") {
      const path = q.get("path") ?? "";
      const log = w.logs[path];
      return log === undefined ? fail(404, `no log at gitbots/logs:${path}`) : text(200, log);
    }
    const diffM = /^\/api\/attempts\/([^/]+)\/diff$/.exec(p);
    if (diffM) {
      const a = findAttempt(w, decodeURIComponent(diffM[1]!));
      if (!a) return fail(404, `no attempt matches \`${decodeURIComponent(diffM[1]!)}\``);
      return text(200, w.diffs[a.id] ?? "");
    }
    const attM = /^\/api\/attempts\/([^/]+)$/.exec(p);
    if (attM) {
      const a = findAttempt(w, decodeURIComponent(attM[1]!));
      if (!a) return fail(404, `no attempt matches \`${decodeURIComponent(attM[1]!)}\``);
      const task = w.tasks.find((t) => t.id === a.task);
      if (!task) return fail(500, "task missing");
      const ex = w.extras[a.id] ?? { commits: [], runs: [], violations: [], workroom: null };
      const chain: Session[] = [];
      let sid: string | undefined = a.session ?? undefined;
      while (sid) {
        const s = w.sessions.find((x) => x.session.id === sid)?.session;
        if (!s || chain.includes(s)) break;
        chain.push(s);
        sid = s.parent;
      }
      const detail: AttemptDetail = {
        attempt: a,
        task,
        workroom: ex.workroom,
        violations: ex.violations,
        commits: ex.commits,
        runs: ex.runs,
        events: w.events.filter((e) => eventAttempt(e) === a.id),
        session_chain: chain,
      };
      return json(200, detail);
    }
    return fail(404, `no route for GET ${p}`);
  }

  if (method === "POST") {
    if (!w.project.can_decide) {
      return fail(403, "gitbots ui was started from an agent session (CLAUDECODE is set); decisions need a human");
    }
    const viewer: Actor = w.project.viewer ?? { type: "unknown" };
    const queue = (item: OutboxItem) => {
      outbox.push(item);
      w.project.pending_outbox = outbox.length;
      return json(202, { queued: true, outbox: item.id });
    };
    if (p === "/api/tasks") {
      const b = (body ?? {}) as Partial<NewTask>;
      const title = typeof b.title === "string" ? b.title.trim() : "";
      if (!title) return fail(422, "title must not be empty");
      const labels = Array.isArray(b.labels) ? b.labels.filter((l): l is string => typeof l === "string" && !!l) : [];
      const task: NewTask = { title, ...(b.body ? { body: b.body } : {}), ...(labels.length ? { labels } : {}) };
      if (w.project.hosted) return queue({ id: stableId("obx", `${Date.now()}-${Math.random()}`), kind: "task.create", body: task, actor: viewer });
      return json(201, { task: applyTask(w, task, viewer) });
    }
    const revM = /^\/api\/attempts\/([^/]+)\/review$/.exec(p);
    if (revM) {
      const a = findAttempt(w, decodeURIComponent(revM[1]!));
      if (!a) return fail(404, `no attempt matches \`${decodeURIComponent(revM[1]!)}\``);
      const b = (body ?? {}) as Partial<ReviewRequest>;
      const decision = b.decision as ReviewDecision | undefined;
      if (decision !== "accept" && decision !== "reject" && decision !== "changes_requested") {
        return fail(422, "decision must be one of accept, reject, changes_requested");
      }
      if (b.merge && decision !== "accept") return fail(422, "merge is only allowed with decision accept");
      if (a.state !== "submitted") return fail(409, `attempt ${a.id.slice(-6).toLowerCase()} is ${a.state}, not submitted`);
      const approvals = w.project.manifest.mandate.approvals;
      const gate = needs(w, approvals.accept_attempt);
      if (gate) return fail(403, `${decision === "accept" ? "accepting" : "deciding on"} an attempt ${gate} (mandate: approvals.accept_attempt = ${approvals.accept_attempt})`);
      const isProtected = w.project.manifest.mandate.agents.protected_branches.some((g) => globMatch(g, a.base));
      if (b.merge && isProtected) {
        const mg = needs(w, approvals.merge_protected);
        if (mg) return fail(403, `merging into ${a.base} ${mg} (mandate: approvals.merge_protected = ${approvals.merge_protected})`);
      }
      const req: ReviewRequest = { decision, ...(b.reason?.trim() ? { reason: b.reason.trim() } : {}), ...(b.merge ? { merge: true } : {}) };
      if (w.project.hosted) return queue({ id: stableId("obx", `${Date.now()}-${Math.random()}`), kind: "review", attempt: a.id, body: req, actor: viewer });
      return json(200, applyReview(w, a.id, req, viewer));
    }
    return fail(404, `no route for POST ${p}`);
  }
  return fail(405, `method ${method} not allowed`);
}

export async function mockFetch(path: string, init: RequestInit): Promise<Response> {
  const w = getWorld();
  const delay = scenario === "slow" ? 1500 : import.meta.env.MODE === "test" ? 0 : 60 + Math.random() * 160;
  if (delay) await new Promise((r) => setTimeout(r, delay));
  if (scenario === "unauthorized") return fail(401, "missing or invalid token");
  const url = new URL(path, "http://127.0.0.1:7777");
  const method = (init.method ?? "GET").toUpperCase();
  let body: unknown;
  if (typeof init.body === "string") {
    try {
      body = JSON.parse(init.body);
    } catch {
      return fail(422, "body is not valid JSON");
    }
  }
  return route(w, method, url, body);
}
