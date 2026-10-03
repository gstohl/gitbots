// Human-readable descriptions of ledger events, one per kind in
// `kind::ALL` (crates/gitbots-core/src/event.rs). Payloads are validated at
// runtime: an unknown kind, or a known kind whose payload doesn't parse, gets
// a generic description instead of crashing the feed (the ledger outlives any
// one build, exactly like `EventBody::Unknown` in Rust).

import type {
  ActionRun,
  DiffStat,
  Event,
  EventDataMap,
  HandoffTarget,
  KnownKind,
  ReviewDecision,
  RunStatus,
} from "../api/types";
import { KNOWN_KINDS } from "../api/types";
import { formatDuration, handoffLabel, shortId, shortSha } from "./format";

export type EventTone = "neutral" | "good" | "bad" | "warn" | "info";
export type EventLink = { text: string; href?: string; mono?: boolean; title?: string };
export type EventPart = string | EventLink;

export type EventDescription = {
  /** Plain one-line text (what the parts spell out). */
  text: string;
  /** The same line with links to tasks, attempts and sessions. */
  parts: EventPart[];
  tone: EventTone;
  /** Secondary text: a report body, review reason, handoff note... */
  detail?: string;
  task?: string;
  attempt?: string;
  session?: string;
  /** False for kinds this build doesn't know (or can't parse). */
  known: boolean;
};

export type DescribeContext = {
  /** Resolve a task id to its title, if known. */
  taskTitle?: (id: string) => string | undefined;
};

export const KIND_GROUPS = ["session", "task", "attempt", "review", "commit", "action", "report", "tool"] as const;
export type KindGroup = (typeof KIND_GROUPS)[number] | "project" | "other";

export function kindGroup(kind: string): KindGroup {
  const head = kind.split(".")[0] ?? "";
  if ((KIND_GROUPS as readonly string[]).includes(head) || head === "project") return head as KindGroup;
  return "other";
}

export function isKnownKind(kind: string): kind is KnownKind {
  return (KNOWN_KINDS as readonly string[]).includes(kind);
}

// ---- runtime guards ----------------------------------------------------------

type Obj = Record<string, unknown>;
const isObj = (v: unknown): v is Obj => typeof v === "object" && v !== null && !Array.isArray(v);
const isStr = (v: unknown): v is string => typeof v === "string";
const optStr = (v: unknown): v is string | undefined => v === undefined || v === null || isStr(v);
const isNum = (v: unknown): v is number => typeof v === "number" && Number.isFinite(v);

function isDiffStat(v: unknown): v is DiffStat {
  return isObj(v) && isNum(v.files) && isNum(v.insertions) && isNum(v.deletions);
}
function isHandoff(v: unknown): v is HandoffTarget {
  if (!isObj(v)) return false;
  return (
    (v.type === "session" && isStr(v.session)) ||
    (v.type === "role" && isStr(v.role)) ||
    (v.type === "human" && isStr(v.handle))
  );
}
const DECISIONS: readonly string[] = ["accept", "reject", "changes_requested"];
const STATUSES: readonly string[] = ["success", "failure", "timed_out", "cancelled", "skipped"];

/** Per-kind payload validators. */
const validators: { [K in KnownKind]: (d: Obj) => boolean } = {
  "project.initialized": (d) => isStr(d.project) && isStr(d.name),
  "session.started": (d) => isObj(d.session) && isStr(d.session.id) && isObj(d.session.agent),
  "session.ended": (d) => isStr(d.session) && optStr(d.summary),
  "task.created": (d) => isStr(d.task) && isStr(d.title),
  "attempt.started": (d) => isStr(d.task) && isStr(d.attempt) && isStr(d.branch) && isStr(d.base),
  "attempt.submitted": (d) =>
    isStr(d.attempt) && isStr(d.head) && optStr(d.summary) && (d.diff === undefined || d.diff === null || isDiffStat(d.diff)),
  "attempt.handoff": (d) => isStr(d.attempt) && isHandoff(d.to),
  "attempt.abandoned": (d) => isStr(d.attempt) && optStr(d.reason),
  "attempt.merged": (d) => isStr(d.attempt) && isStr(d.into) && isStr(d.commit),
  "review.decided": (d) => isStr(d.attempt) && isStr(d.decision) && DECISIONS.includes(d.decision),
  "commit.recorded": (d) =>
    isStr(d.sha) && isStr(d.subject) && (d.diff === undefined || d.diff === null || isDiffStat(d.diff)),
  "action.completed": (d) =>
    isStr(d.run) && isStr(d.workflow) && isStr(d.status) && STATUSES.includes(d.status) && Array.isArray(d.jobs),
  report: (d) => isStr(d.title),
  "tool.called": (d) => isStr(d.tool) && typeof d.ok === "boolean",
};

/** The event's payload, typed by kind, or null if the kind is unknown or the payload malformed. */
export function eventData<K extends KnownKind>(e: Event, kind: K): EventDataMap[K] | null {
  if (e.kind !== kind || !isObj(e.data)) return null;
  return validators[kind](e.data) ? (e.data as EventDataMap[K]) : null;
}

// ---- links -----------------------------------------------------------------

export const hrefs = {
  attempt: (id: string) => `/attempts/${encodeURIComponent(id)}`,
  task: (id: string) => `/board#${encodeURIComponent(id)}`,
  session: (id: string) => `/sessions#${encodeURIComponent(id)}`,
};

const attemptLink = (id: string): EventLink => ({ text: shortId(id), href: hrefs.attempt(id), mono: true, title: id });
const sessionLink = (id: string): EventLink => ({ text: shortId(id), href: hrefs.session(id), mono: true, title: id });
function taskLink(id: string, ctx?: DescribeContext): EventLink {
  const title = ctx?.taskTitle?.(id);
  return title ? { text: title, href: hrefs.task(id), title: id } : { text: shortId(id), href: hrefs.task(id), mono: true, title: id };
}
const code = (text: string, title?: string): EventLink => (title ? { text, mono: true, title } : { text, mono: true });

function diffText(d: DiffStat | undefined): string | null {
  return d ? `+${d.insertions} −${d.deletions}` : null;
}

const decisionVerb: Record<ReviewDecision, string> = {
  accept: "accepted",
  reject: "rejected",
  changes_requested: "requested changes on",
};
const decisionTone: Record<ReviewDecision, EventTone> = { accept: "good", reject: "bad", changes_requested: "warn" };
export const runTone = (s: RunStatus): EventTone =>
  s === "success" ? "good" : s === "skipped" || s === "cancelled" ? "neutral" : "bad";
export const runLabel: Record<RunStatus, string> = {
  success: "passed",
  failure: "failed",
  timed_out: "timed out",
  cancelled: "was cancelled",
  skipped: "was skipped",
};

function finish(
  parts: EventPart[],
  rest: Omit<EventDescription, "parts" | "text" | "known"> & { known?: boolean },
): EventDescription {
  const text = parts
    .map((p) => (typeof p === "string" ? p : p.text))
    .join("")
    .replace(/\s+/g, " ")
    .trim();
  const out: EventDescription = { text, parts, tone: rest.tone, known: rest.known ?? true };
  if (rest.detail) out.detail = rest.detail;
  if (rest.task) out.task = rest.task;
  if (rest.attempt) out.attempt = rest.attempt;
  if (rest.session) out.session = rest.session;
  return out;
}

function compactJson(v: unknown, max = 140): string {
  let s: string;
  try {
    s = JSON.stringify(v) ?? "";
  } catch {
    s = String(v);
  }
  return s.length > max ? `${s.slice(0, max - 1)}…` : s;
}

function describeUnknown(e: Event): EventDescription {
  const parts: EventPart[] = [code(e.kind)];
  const data = e.data === undefined || e.data === null ? "" : compactJson(e.data);
  if (data && data !== "{}") parts.push(" ", data);
  return finish(parts, { tone: "neutral", known: false });
}

/** One-line description of any event. Never throws. */
export function describeEvent(e: Event, ctx?: DescribeContext): EventDescription {
  if (!isKnownKind(e.kind) || !isObj(e.data) || !validators[e.kind](e.data)) return describeUnknown(e);
  return describeKnown(e as Event & { kind: KnownKind; data: Obj }, ctx);
}

function describeKnown(e: Event & { kind: KnownKind }, ctx?: DescribeContext): EventDescription {
  switch (e.kind) {
    case "project.initialized": {
      const d = e.data as EventDataMap["project.initialized"];
      return finish(["initialized project ", code(d.name, d.project)], { tone: "info" });
    }
    case "session.started": {
      const { session: s } = e.data as EventDataMap["session.started"];
      const parts: EventPart[] = ["started session ", sessionLink(s.id), ` (${s.agent.model}@${s.agent.client})`];
      if (s.role) parts.push(` as ${s.role}`);
      if (s.parent) parts.push(", subagent of ", sessionLink(s.parent));
      const out: Omit<EventDescription, "parts" | "text" | "known"> = { tone: "info", session: s.id };
      if (s.label) out.detail = s.label;
      return finish(parts, out);
    }
    case "session.ended": {
      const d = e.data as EventDataMap["session.ended"];
      const out: Omit<EventDescription, "parts" | "text" | "known"> = { tone: "neutral", session: d.session };
      if (d.summary) out.detail = d.summary;
      return finish(["ended session ", sessionLink(d.session)], out);
    }
    case "task.created": {
      const d = e.data as EventDataMap["task.created"];
      const parts: EventPart[] = ["created task ", { text: d.title, href: hrefs.task(d.task), title: d.task }];
      if (d.labels && d.labels.length) parts.push(` [${d.labels.join(", ")}]`);
      if (d.recipe) parts.push(" from recipe ", code(d.recipe));
      const out: Omit<EventDescription, "parts" | "text" | "known"> = { tone: "info", task: d.task };
      if (d.body) out.detail = d.body;
      return finish(parts, out);
    }
    case "attempt.started": {
      const d = e.data as EventDataMap["attempt.started"];
      const parts: EventPart[] = ["started attempt ", attemptLink(d.attempt), " on ", code(d.branch), ` → ${d.base}`];
      parts.push(" for ", taskLink(d.task, ctx));
      const out: Omit<EventDescription, "parts" | "text" | "known"> = { tone: "info", task: d.task, attempt: d.attempt };
      if (d.session) out.session = d.session;
      return finish(parts, out);
    }
    case "attempt.submitted": {
      const d = e.data as EventDataMap["attempt.submitted"];
      const parts: EventPart[] = ["submitted attempt ", attemptLink(d.attempt)];
      if (d.summary) parts.push(`: ${d.summary}`);
      else parts.push(" at ", code(shortSha(d.head), d.head));
      const diff = diffText(d.diff);
      if (diff) parts.push(` (${diff})`);
      return finish(parts, { tone: "info", attempt: d.attempt });
    }
    case "attempt.handoff": {
      const d = e.data as EventDataMap["attempt.handoff"];
      const target: EventPart = d.to.type === "session" ? sessionLink(d.to.session) : code(handoffLabel(d.to));
      const out: Omit<EventDescription, "parts" | "text" | "known"> = { tone: "info", attempt: d.attempt };
      if (d.note) out.detail = d.note;
      if (d.to.type === "session") out.session = d.to.session;
      return finish(["handed off attempt ", attemptLink(d.attempt), " to ", target], out);
    }
    case "attempt.abandoned": {
      const d = e.data as EventDataMap["attempt.abandoned"];
      const out: Omit<EventDescription, "parts" | "text" | "known"> = { tone: "neutral", attempt: d.attempt };
      if (d.reason) out.detail = d.reason;
      return finish(["abandoned attempt ", attemptLink(d.attempt)], out);
    }
    case "attempt.merged": {
      const d = e.data as EventDataMap["attempt.merged"];
      const parts: EventPart[] = [
        "merged attempt ",
        attemptLink(d.attempt),
        " into ",
        code(d.into),
        " at ",
        code(shortSha(d.commit), d.commit),
      ];
      if (d.source_commits && d.source_commits.length) parts.push(` (${d.source_commits.length} commits)`);
      return finish(parts, { tone: "good", attempt: d.attempt });
    }
    case "review.decided": {
      const d = e.data as EventDataMap["review.decided"];
      const out: Omit<EventDescription, "parts" | "text" | "known"> = { tone: decisionTone[d.decision], attempt: d.attempt };
      if (d.reason) out.detail = d.reason;
      return finish([`${decisionVerb[d.decision]} attempt `, attemptLink(d.attempt)], out);
    }
    case "commit.recorded": {
      const d = e.data as EventDataMap["commit.recorded"];
      const parts: EventPart[] = ["committed ", code(shortSha(d.sha), d.sha), ` ${d.subject}`];
      const diff = diffText(d.diff);
      if (diff) parts.push(` (${diff})`);
      if (d.attempt) parts.push(" in ", attemptLink(d.attempt));
      else if (d.branch) parts.push(" on ", code(d.branch));
      const out: Omit<EventDescription, "parts" | "text" | "known"> = { tone: "neutral" };
      if (d.attempt) out.attempt = d.attempt;
      return finish(parts, out);
    }
    case "action.completed": {
      const d = e.data as ActionRun;
      const failed = d.jobs.filter((j) => j.status !== "success" && j.status !== "skipped").map((j) => j.name);
      const parts: EventPart[] = [code(d.workflow), ` ${runLabel[d.status]} in ${formatDuration(d.duration_ms)}`];
      if (failed.length) parts.push(` (${failed.join(", ")})`);
      if (d.attempt) parts.push(" for ", attemptLink(d.attempt));
      const out: Omit<EventDescription, "parts" | "text" | "known"> = { tone: runTone(d.status) };
      if (d.attempt) out.attempt = d.attempt;
      return finish(parts, out);
    }
    case "report": {
      const d = e.data as EventDataMap["report"];
      const level = d.level ?? "info";
      const parts: EventPart[] = [level === "info" ? "reported " : `reported ${level}: `, d.title];
      if (d.attempt) parts.push(" on ", attemptLink(d.attempt));
      else if (d.task) parts.push(" on ", taskLink(d.task, ctx));
      const out: Omit<EventDescription, "parts" | "text" | "known"> = {
        tone: level === "blocker" ? "bad" : level === "warning" ? "warn" : "info",
      };
      if (d.body) out.detail = d.body;
      if (d.task) out.task = d.task;
      if (d.attempt) out.attempt = d.attempt;
      return finish(parts, out);
    }
    case "tool.called": {
      const d = e.data as EventDataMap["tool.called"];
      const parts: EventPart[] = [d.ok ? "ran " : "failed ", code(d.tool)];
      if (d.input) parts.push(` ${d.input.length > 120 ? `${d.input.slice(0, 119)}…` : d.input}`);
      if (d.duration_ms !== undefined) parts.push(` (${formatDuration(d.duration_ms)})`);
      return finish(parts, { tone: d.ok ? "neutral" : "bad" });
    }
  }
}
