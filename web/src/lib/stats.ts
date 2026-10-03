// Rates and leaderboard rows derived from `GET /api/stats`. The formulas
// mirror `ActorStats::acceptance_rate` / `run_pass_rate` in gitbots-core.

import type { ActorStats, AgentDescriptor, Stats } from "../api/types";
import { parseAgentKey } from "./format";

/** accepted / (accepted + rejected + changes_requested); null when nothing was decided. */
export function acceptanceRate(s: ActorStats): number | null {
  const decided = s.accepted + s.rejected + s.changes_requested;
  return decided > 0 ? s.accepted / decided : null;
}

/** runs_passed / runs; null when there were no runs. */
export function runPassRate(s: ActorStats): number | null {
  return s.runs > 0 ? s.runs_passed / s.runs : null;
}

/** tool_failures / tool_calls; null when there were no tool calls. */
export function toolFailureRate(s: ActorStats): number | null {
  return s.tool_calls > 0 ? s.tool_failures / s.tool_calls : null;
}

export function churn(s: ActorStats): number {
  return s.lines_added + s.lines_removed;
}

export function decided(s: ActorStats): number {
  return s.accepted + s.rejected + s.changes_requested;
}

export type StatsRow = {
  key: string;
  kind: "agent" | "human" | "other";
  agent: AgentDescriptor | null;
  handle: string | null;
  stats: ActorStats;
  acceptance: number | null;
  passRate: number | null;
  churn: number;
};

export function statsRows(stats: Stats): StatsRow[] {
  return Object.entries(stats.by_actor).map(([key, s]) => {
    const human = key.startsWith("@");
    const agent = human ? null : parseAgentKey(key);
    return {
      key,
      kind: human ? "human" : agent ? "agent" : "other",
      agent,
      handle: human ? key.slice(1) : null,
      stats: s,
      acceptance: acceptanceRate(s),
      passRate: runPassRate(s),
      churn: churn(s),
    };
  });
}

export type SortKey = "acceptance" | "passRate" | "attempts" | "commits" | "churn" | "sessions";
export type SortDir = "asc" | "desc";

const sortValue: Record<SortKey, (r: StatsRow) => number | null> = {
  acceptance: (r) => r.acceptance,
  passRate: (r) => r.passRate,
  attempts: (r) => r.stats.attempts_started,
  commits: (r) => r.stats.commits,
  churn: (r) => r.churn,
  sessions: (r) => r.stats.sessions,
};

/**
 * Sorts rows by `key`. Rows without data (`null` rate) always sink to the
 * bottom regardless of direction; ties fall back to volume (decided reviews,
 * then attempts) and finally the key, so the order is stable.
 */
export function sortRows(rows: StatsRow[], key: SortKey, dir: SortDir = "desc"): StatsRow[] {
  const get = sortValue[key];
  const sign = dir === "desc" ? -1 : 1;
  return [...rows].sort((a, b) => {
    const va = get(a);
    const vb = get(b);
    if (va === null && vb !== null) return 1;
    if (vb === null && va !== null) return -1;
    if (va !== null && vb !== null && va !== vb) return (va - vb) * sign;
    const vol = decided(b.stats) - decided(a.stats) || b.stats.attempts_started - a.stats.attempts_started;
    return vol || a.key.localeCompare(b.key);
  });
}

/** Pooled totals over a set of rows (rates are pooled, not averaged). */
export function totals(rows: StatsRow[]): { stats: ActorStats; acceptance: number | null; passRate: number | null } {
  const zero: ActorStats = {
    sessions: 0,
    subagent_sessions: 0,
    tasks_created: 0,
    attempts_started: 0,
    attempts_submitted: 0,
    accepted: 0,
    rejected: 0,
    changes_requested: 0,
    merged: 0,
    abandoned: 0,
    handoffs: 0,
    commits: 0,
    lines_added: 0,
    lines_removed: 0,
    runs: 0,
    runs_passed: 0,
    reports: 0,
    tool_calls: 0,
    tool_failures: 0,
  };
  const sum = rows.reduce<ActorStats>((acc, r) => {
    for (const k of Object.keys(acc) as (keyof ActorStats)[]) acc[k] += r.stats[k];
    return acc;
  }, zero);
  return { stats: sum, acceptance: acceptanceRate(sum), passRate: runPassRate(sum) };
}
