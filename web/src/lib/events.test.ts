import { describe, expect, it } from "vitest";
// Drift guard: the kinds this UI knows must match `kind::ALL` in gitbots-core.
import eventRs from "../../../crates/gitbots-core/src/event.rs?raw";
import type { Actor, Event, EventDataMap, KnownKind } from "../api/types";
import { KNOWN_KINDS } from "../api/types";
import { buildWorld } from "../mocks/fixtures";
import { describeEvent, eventData, kindGroup } from "./events";

const ATT = "att_01K6M3C2V7QZ8Y9X0WABCDEF";
const TSK = "tsk_01K6M3C2V7QZ8Y9X0WTASK01";
const SES = "ses_01K6M3C2V7QZ8Y9X0WSESS01";
const PAR = "ses_01K6M3C2V7QZ8Y9X0WPARNT1";
const agent: Actor = { type: "agent", session: SES, agent: { provider: "anthropic", model: "claude-opus-5-5", client: "claude-code" } };

function ev<K extends KnownKind>(kind: K, data: EventDataMap[K]): Event {
  return { v: 1, id: "evt_01K6M3C2V7QZ8Y9X0W1T2EVENT", ts: "2026-10-03T10:31:00Z", actor: agent, via: "worktree", kind, data };
}

/** One sample per kind, with the description we expect. */
const SAMPLES: { [K in KnownKind]: { data: EventDataMap[K]; text: string; tone?: string; attempt?: string; task?: string; detail?: string } } = {
  "project.initialized": { data: { project: "prj_01K6M3C2V7QZ8Y9X0W1T2PROJ1", name: "billing" }, text: "initialized project billing" },
  "session.started": {
    data: {
      session: {
        id: SES,
        agent: { provider: "anthropic", model: "claude-haiku-4-5", client: "claude-code" },
        parent: PAR,
        role: "test-writer",
        label: "idempotency",
        started_at: "2026-10-03T10:00:00Z",
      },
    },
    text: "started session sess01 (claude-haiku-4-5@claude-code) as test-writer, subagent of parnt1",
    detail: "idempotency",
  },
  "session.ended": { data: { session: SES, summary: "done" }, text: "ended session sess01", detail: "done" },
  "task.created": {
    data: { task: TSK, title: "Add rate limiting", labels: ["api", "perf"], recipe: "rl@1" },
    text: "created task Add rate limiting [api, perf] from recipe rl@1",
    task: TSK,
  },
  "attempt.started": {
    data: { task: TSK, attempt: ATT, branch: "gitbots/attempt/rl-abcde", base: "main", base_commit: "abc" },
    text: "started attempt abcdef on gitbots/attempt/rl-abcde → main for Add rate limiting",
    attempt: ATT,
    task: TSK,
  },
  "attempt.submitted": {
    data: { attempt: ATT, head: "0123456789abcdef", summary: "adds /health", diff: { files: 2, insertions: 10, deletions: 3 } },
    text: "submitted attempt abcdef: adds /health (+10 −3)",
    attempt: ATT,
  },
  "attempt.handoff": {
    data: { attempt: ATT, to: { type: "human", handle: "santosh" }, note: "need access" },
    text: "handed off attempt abcdef to @santosh",
    detail: "need access",
  },
  "attempt.abandoned": { data: { attempt: ATT, reason: "out of scope" }, text: "abandoned attempt abcdef", detail: "out of scope" },
  "attempt.merged": {
    data: { attempt: ATT, into: "main", commit: "fedcba9876543210", source_commits: ["a", "b"] },
    text: "merged attempt abcdef into main at fedcba98 (2 commits)",
    tone: "good",
  },
  "review.decided": {
    data: { attempt: ATT, decision: "changes_requested", reason: "add tests" },
    text: "requested changes on attempt abcdef",
    tone: "warn",
    detail: "add tests",
  },
  "commit.recorded": {
    data: { sha: "0123456789abcdef", subject: "Fix it", attempt: ATT, diff: { files: 1, insertions: 1, deletions: 0 } },
    text: "committed 01234567 Fix it (+1 −0) in abcdef",
  },
  "action.completed": {
    data: {
      run: "run_01K6M3C2V7QZ8Y9X0W1T2RUN01",
      workflow: "ci",
      trigger: "attempt.submitted",
      attempt: ATT,
      runner: "local",
      status: "failure",
      duration_ms: 75_400,
      jobs: [
        { name: "fmt", status: "success", duration_ms: 1000 },
        { name: "test", status: "failure", duration_ms: 74_400 },
      ],
    },
    text: "ci failed in 1m 15s (test) for abcdef",
    tone: "bad",
  },
  report: {
    data: { title: "Blocked on creds", level: "blocker", task: TSK, body: "need prod access" },
    text: "reported blocker: Blocked on creds on Add rate limiting",
    tone: "bad",
    detail: "need prod access",
  },
  "tool.called": { data: { tool: "Bash", input: "cargo test", ok: false, duration_ms: 1200 }, text: "failed Bash cargo test (1.2s)", tone: "bad" },
};

const ctx = { taskTitle: (id: string) => (id === TSK ? "Add rate limiting" : undefined) };

describe("describeEvent", () => {
  it("knows exactly the kinds in gitbots-core's kind::ALL", () => {
    const rust = [...eventRs.matchAll(/=>\s*[A-Z_]+\s*=\s*"([a-z_.]+)"/g)].map((m) => m[1]);
    expect(rust.length).toBeGreaterThan(10);
    expect([...KNOWN_KINDS]).toEqual(rust);
  });

  for (const kind of KNOWN_KINDS) {
    it(`describes ${kind}`, () => {
      const s = SAMPLES[kind];
      const d = describeEvent(ev(kind, s.data as never), ctx);
      expect(d.known).toBe(true);
      expect(d.text).toBe(s.text);
      if (s.tone) expect(d.tone).toBe(s.tone);
      if (s.attempt) expect(d.attempt).toBe(s.attempt);
      if (s.task) expect(d.task).toBe(s.task);
      if (s.detail) expect(d.detail).toBe(s.detail);
      expect(eventData(ev(kind, s.data as never), kind)).not.toBeNull();
    });
  }

  it("links attempts, tasks and sessions", () => {
    const d = describeEvent(ev("attempt.started", SAMPLES["attempt.started"].data), ctx);
    const links = d.parts.filter((p) => typeof p !== "string" && p.href).map((p) => (p as { href: string }).href);
    expect(links).toEqual([`/attempts/${ATT}`, `/board#${TSK}`]);
  });

  it("falls back gracefully for unknown kinds", () => {
    const e: Event = { ...ev("report", SAMPLES.report.data), kind: "attempt.teleported", data: { to: "mars" } };
    const d = describeEvent(e);
    expect(d.known).toBe(false);
    expect(d.text).toBe('attempt.teleported {"to":"mars"}');
    expect(kindGroup("attempt.teleported")).toBe("attempt");
    expect(kindGroup("weird")).toBe("other");
  });

  it("treats a known kind with a malformed payload as unknown", () => {
    const e: Event = { ...ev("task.created", SAMPLES["task.created"].data), data: { title: 3 } };
    expect(describeEvent(e).known).toBe(false);
    const nul: Event = { ...e, data: null };
    expect(describeEvent(nul).text).toBe("task.created");
    expect(eventData(e, "task.created")).toBeNull();
  });

  it("describes every event in the mock ledger without throwing", () => {
    const w = buildWorld(Date.parse("2026-10-03T12:00:00Z"));
    const kinds = new Set(w.events.map((e) => e.kind));
    for (const k of KNOWN_KINDS) expect(kinds.has(k), `fixtures include ${k}`).toBe(true);
    for (const e of w.events) {
      const d = describeEvent(e);
      expect(d.text.length).toBeGreaterThan(0);
      expect(d.known).toBe(e.kind !== "attempt.rebased");
    }
  });
});
