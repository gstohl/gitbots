import { afterEach, describe, expect, it } from "vitest";
import { api, isQueued, setTransport } from "../api/client";
import { ALL_STATES, buildWorld, IDS } from "./fixtures";
import { mockFetch, resetWorld, runSteward } from "./server";

const NOW = Date.parse("2026-10-03T12:00:00Z");
function serve(scenario: Parameters<typeof resetWorld>[1] = "default") {
  const w = resetWorld(buildWorld(NOW), scenario);
  setTransport(mockFetch);
  return w;
}
afterEach(() => setTransport(null));

describe("mock fixtures", () => {
  it("cover every attempt state, passing and failing runs, a blocker and three agents", async () => {
    serve();
    const board = await api.board();
    expect(new Set(board.attempts.map((a) => a.state))).toEqual(new Set(ALL_STATES));
    expect(board.attempts.some((a) => a.checks_passed === true)).toBe(true);
    expect(board.attempts.some((a) => a.checks_passed === false)).toBe(true);
    expect(new Set(board.tasks.map((t) => t.status))).toEqual(new Set(["open", "in_progress", "accepted", "done"]));
    const models = new Set(board.sessions.map((s) => s.session.agent.model));
    for (const m of ["claude-opus-5-5", "gpt-5-codex", "claude-haiku-4-5"]) expect(models.has(m)).toBe(true);
    expect(board.sessions.some((s) => s.session.parent)).toBe(true);
    const inbox = await api.inbox();
    expect(inbox.reports[0]!.report.level).toBe("blocker");
    expect((await api.attemptDiff(IDS.aRate)).startsWith("diff --git")).toBe(true);
  });

  it("serves logs, filters events by kind prefix and limit, and looks up short ids", async () => {
    serve();
    const d = await api.attempt(IDS.aFlaky.slice(-6).toLowerCase());
    expect(d.attempt.id).toBe(IDS.aFlaky);
    const failing = d.runs[0]!.jobs.find((j) => j.status === "failure")!;
    expect(await api.log(failing.log!.path)).toContain("FAILED");
    const events = await api.events({ kind: "attempt", limit: 3 });
    expect(events).toHaveLength(3);
    for (const e of events) expect(e.kind.startsWith("attempt.")).toBe(true);
  });

  it("enforces the mandate on reviews", async () => {
    serve("reviewer-viewer");
    // @priya is a reviewer: may accept, may not merge into protected main.
    await expect(api.review(IDS.aRate, { decision: "accept", merge: true })).rejects.toThrow(/needs a human with role maintainer/);
    const ok = await api.review(IDS.aRate, { decision: "accept" });
    expect(ok).toMatchObject({ decision: "accept", merged: null });
    await expect(api.review(IDS.aRate, { decision: "accept" })).rejects.toMatchObject({ status: 409 });
  });

  it("refuses writes when started by an agent", async () => {
    serve("agent-viewer");
    expect((await api.project()).can_decide).toBe(false);
    await expect(api.createTask({ title: "x" })).rejects.toMatchObject({ status: 403 });
  });
});

describe("hosted mock", () => {
  it("queues writes (202) until the steward applies them", async () => {
    serve("hosted");
    const p0 = await api.project();
    expect(p0.hosted).toBe(true);
    expect(p0.pending_outbox).toBe(1);
    const tip0 = (await api.tip()).activity;

    const t = await api.createTask({ title: "Queued task" });
    expect(isQueued(t)).toBe(true);
    const r = await api.review(IDS.aRate, { decision: "accept", merge: true });
    expect(isQueued(r)).toBe(true);
    expect((await api.project()).pending_outbox).toBe(3);
    // Nothing applied yet: the ledger tip and the attempt didn't move.
    expect((await api.tip()).activity).toBe(tip0);
    expect((await api.attempt(IDS.aRate)).attempt.state).toBe("submitted");

    while (runSteward()) {
      /* drain */
    }
    expect((await api.project()).pending_outbox).toBe(0);
    expect((await api.tip()).activity).not.toBe(tip0);
    expect((await api.attempt(IDS.aRate)).attempt.state).toBe("merged");
    const titles = (await api.board()).tasks.map((x) => x.title);
    expect(titles).toContain("Queued task");
    expect(titles).toContain("Bump tokio to 1.48");
  });
});
