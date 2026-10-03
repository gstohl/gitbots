import type { Actor, AgentDescriptor, HandoffTarget } from "../api/types";

/** Short id: the last 6 characters, lowercased (matches `Id::short()` in gitbots-core). */
export function shortId(id: string | null | undefined): string {
  if (!id) return "";
  return id.slice(-6).toLowerCase();
}

/** First 8 characters of a sha (matches `short_sha` in gitbots-core). */
export function shortSha(sha: string | null | undefined): string {
  if (!sha) return "";
  return sha.slice(0, 8);
}

/** `provider/model@client`, the stats key for an agent. */
export function agentKey(a: AgentDescriptor): string {
  return `${a.provider}/${a.model}@${a.client}`;
}

/**
 * Best-effort split of a stats key. Keys are display strings, not a format
 * (model ids may contain `/` or `@`), so this splits at the first `/` and the
 * last `@` and is only used for display.
 */
export function parseAgentKey(key: string): AgentDescriptor | null {
  const slash = key.indexOf("/");
  const at = key.lastIndexOf("@");
  if (slash <= 0 || at <= slash + 1 || at === key.length - 1) return null;
  return { provider: key.slice(0, slash), model: key.slice(slash + 1, at), client: key.slice(at + 1) };
}

export function actorLabel(a: Actor | null | undefined): string {
  if (!a) return "nobody";
  switch (a.type) {
    case "human":
      return `@${a.handle}`;
    case "agent":
      return `${a.agent.model}@${a.agent.client} (${shortId(a.session)})`;
    case "system":
      return `system:${a.component}`;
    default:
      return "unknown";
  }
}

export function handoffLabel(t: HandoffTarget): string {
  switch (t.type) {
    case "session":
      return shortId(t.session);
    case "role":
      return `role:${t.role}`;
    case "human":
      return `@${t.handle}`;
  }
}

const UNITS: [Intl.RelativeTimeFormatUnit, number][] = [
  ["year", 365 * 24 * 3600],
  ["month", 30 * 24 * 3600],
  ["week", 7 * 24 * 3600],
  ["day", 24 * 3600],
  ["hour", 3600],
  ["minute", 60],
];

/** Compact relative time: "just now", "4m ago", "3h ago", "2d ago", "in 5m". */
export function relativeTime(iso: string, now: number = Date.now()): string {
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return iso;
  const diff = Math.round((t - now) / 1000);
  const abs = Math.abs(diff);
  if (abs < 45) return "just now";
  const short: Record<string, string> = { year: "y", month: "mo", week: "w", day: "d", hour: "h", minute: "m" };
  for (const [unit, secs] of UNITS) {
    if (abs >= secs) {
      const n = Math.floor(abs / secs);
      return diff < 0 ? `${n}${short[unit]} ago` : `in ${n}${short[unit]}`;
    }
  }
  return diff < 0 ? "1m ago" : "in 1m";
}

/** Short age without "ago" (for dense cells): "4m", "3h", "2d". */
export function age(iso: string, now: number = Date.now()): string {
  const r = relativeTime(iso, now);
  return r === "just now" ? "now" : r.replace(/ ago$/, "");
}

const absFmt = new Intl.DateTimeFormat(undefined, {
  year: "numeric",
  month: "short",
  day: "numeric",
  hour: "2-digit",
  minute: "2-digit",
  second: "2-digit",
  timeZoneName: "short",
});

export function absoluteTime(iso: string): string {
  const t = Date.parse(iso);
  return Number.isNaN(t) ? iso : absFmt.format(new Date(t));
}

/** 850ms, 4.2s, 3m 07s, 1h 02m. */
export function formatDuration(ms: number | null | undefined): string {
  if (ms === null || ms === undefined || !Number.isFinite(ms)) return "—";
  if (ms < 1000) return `${Math.round(ms)}ms`;
  const s = ms / 1000;
  if (s < 60) return `${s < 10 ? s.toFixed(1) : Math.round(s)}s`;
  const m = Math.floor(s / 60);
  const rs = Math.round(s % 60);
  if (m < 60) return `${m}m ${String(rs).padStart(2, "0")}s`;
  const h = Math.floor(m / 60);
  return `${h}h ${String(m % 60).padStart(2, "0")}m`;
}

export function formatPercent(r: number | null): string {
  return r === null ? "—" : `${Math.round(r * 100)}%`;
}

const nf = new Intl.NumberFormat();
export function formatNumber(n: number): string {
  return nf.format(n);
}

export function plural(n: number, one: string, many = `${one}s`): string {
  return `${n} ${n === 1 ? one : many}`;
}
