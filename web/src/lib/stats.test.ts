import { describe, expect, it } from "vitest";
import type { ActorStats } from "../api/types";
import { parseAgentKey } from "./format";
import { acceptanceRate, churn, runPassRate, sortRows, statsRows, totals, toolFailureRate } from "./stats";

const zero: ActorStats = {
  sessions: 0, subagent_sessions: 0, tasks_created: 0, attempts_started: 0, attempts_submitted: 0, accepted: 0,
  rejected: 0, changes_requested: 0, merged: 0, abandoned: 0, handoffs: 0, commits: 0, lines_added: 0,
  lines_removed: 0, runs: 0, runs_passed: 0, reports: 0, tool_calls: 0, tool_failures: 0,
};
const s = (p: Partial<ActorStats>): ActorStats => ({ ...zero, ...p });

describe("rates", () => {
  it("acceptance = accepted / (accepted + rejected + changes_requested)", () => {
    expect(acceptanceRate(s({ accepted: 3, rejected: 1 }))).toBe(0.75);
    expect(acceptanceRate(s({ accepted: 2, rejected: 1, changes_requested: 1 }))).toBe(0.5);
    // Merged/abandoned don't count as decisions.
    expect(acceptanceRate(s({ accepted: 1, merged: 5, abandoned: 9 }))).toBe(1);
  });

  it("is null without data, not zero", () => {
    expect(acceptanceRate(zero)).toBeNull();
    expect(runPassRate(zero)).toBeNull();
    expect(toolFailureRate(zero)).toBeNull();
    expect(acceptanceRate(s({ rejected: 2 }))).toBe(0);
  });

  it("pass rate and churn", () => {
    expect(runPassRate(s({ runs: 8, runs_passed: 6 }))).toBe(0.75);
    expect(churn(s({ lines_added: 10, lines_removed: 4 }))).toBe(14);
    expect(toolFailureRate(s({ tool_calls: 200, tool_failures: 10 }))).toBe(0.05);
  });
});

describe("leaderboard rows", () => {
  const stats = {
    by_actor: {
      "anthropic/claude-opus-5-5@claude-code": s({ accepted: 8, rejected: 2, runs: 10, runs_passed: 9, sessions: 4, commits: 50 }),
      "openai/gpt-5-codex@codex": s({ accepted: 9, rejected: 1, runs: 10, runs_passed: 7, sessions: 3, commits: 70 }),
      "openai/gpt-5@chatgpt": s({ sessions: 1 }),
      "@gstohl": s({ tasks_created: 5 }),
    },
  };

  it("splits agents and humans and parses keys", () => {
    const rows = statsRows(stats);
    expect(rows.find((r) => r.key === "@gstohl")).toMatchObject({ kind: "human", handle: "gstohl", agent: null });
    expect(rows.find((r) => r.key.startsWith("anthropic"))?.agent).toEqual({
      provider: "anthropic",
      model: "claude-opus-5-5",
      client: "claude-code",
    });
    expect(parseAgentKey("vendor/org/model@v2@cli")).toEqual({ provider: "vendor", model: "org/model@v2", client: "cli" });
    expect(parseAgentKey("nonsense")).toBeNull();
  });

  it("sorts by rate with no-data rows last in both directions", () => {
    const agents = statsRows(stats).filter((r) => r.kind === "agent");
    expect(sortRows(agents, "acceptance", "desc").map((r) => r.key)).toEqual([
      "openai/gpt-5-codex@codex",
      "anthropic/claude-opus-5-5@claude-code",
      "openai/gpt-5@chatgpt",
    ]);
    expect(sortRows(agents, "acceptance", "asc").map((r) => r.key)[2]).toBe("openai/gpt-5@chatgpt");
    expect(sortRows(agents, "passRate", "desc")[0]!.key).toBe("anthropic/claude-opus-5-5@claude-code");
    expect(sortRows(agents, "commits", "desc")[0]!.key).toBe("openai/gpt-5-codex@codex");
    expect(sortRows(agents, "sessions", "asc")[0]!.key).toBe("openai/gpt-5@chatgpt");
  });

  it("pools totals instead of averaging rates", () => {
    const t = totals(statsRows(stats).filter((r) => r.kind === "agent"));
    expect(t.stats.accepted).toBe(17);
    expect(t.acceptance).toBe(17 / 20);
    expect(t.passRate).toBe(16 / 20);
  });
});
