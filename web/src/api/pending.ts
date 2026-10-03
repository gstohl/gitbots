// Hosted mode: writes answered with 202 `{queued, outbox}` sit in the
// Worker's outbox until the next `gitbots sync` applies them. We remember them
// here (per tab, sessionStorage) so the UI can show "pending sync" until the
// ledger tip moves and the board reflects the decision.

import { createSignal } from "solid-js";
import type { ReviewDecision } from "./types";

export type PendingReview = { kind: "review"; outbox: string; attempt: string; decision: ReviewDecision; merge: boolean; at: number };
export type PendingTask = { kind: "task"; outbox: string; title: string; at: number };
export type Pending = PendingReview | PendingTask;

const KEY = "gitbots.pending";

function load(): Pending[] {
  try {
    const raw = sessionStorage.getItem(KEY);
    const v: unknown = raw ? JSON.parse(raw) : [];
    return Array.isArray(v) ? (v as Pending[]) : [];
  } catch {
    return [];
  }
}

const [items, setItemsRaw] = createSignal<Pending[]>(typeof window === "undefined" ? [] : load());
export { items as pendingItems };

function setItems(next: Pending[]) {
  setItemsRaw(next);
  try {
    sessionStorage.setItem(KEY, JSON.stringify(next));
  } catch {
    /* ignore */
  }
}

export function addPending(p: Pending): void {
  setItems([...items().filter((x) => x.outbox !== p.outbox), p]);
}

export function clearPending(): void {
  setItems([]);
}

export function pendingReview(attempt: string): PendingReview | undefined {
  return items().find((p): p is PendingReview => p.kind === "review" && p.attempt === attempt);
}

export function pendingTasks(): PendingTask[] {
  return items().filter((p): p is PendingTask => p.kind === "task");
}

/** A queued review is done once its attempt left `submitted`. */
export function reconcileAttempts(attempts: { id: string; state: string }[]): void {
  const state = new Map(attempts.map((a) => [a.id, a.state]));
  const next = items().filter((p) => p.kind !== "review" || (state.get(p.attempt) ?? "submitted") === "submitted");
  if (next.length !== items().length) setItems(next);
}

/** A queued task is done once a task with that title, created after it was queued, is on the board. */
export function reconcileTasks(tasks: { title: string; created_at: string }[]): void {
  const next = items().filter(
    (p) => p.kind !== "task" || !tasks.some((t) => t.title === p.title && Date.parse(t.created_at) >= p.at - 60_000),
  );
  if (next.length !== items().length) setItems(next);
}

/**
 * The outbox drained: a project fetch that *started after* an item was queued
 * reports `pending_outbox: 0`, so everything queued before it was applied (or
 * rejected by the steward).
 */
export function reconcileOutbox(pendingOutbox: number | undefined, fetchStartedAt: number | null): void {
  if (pendingOutbox === undefined || pendingOutbox > 0 || fetchStartedAt === null) return;
  const next = items().filter((p) => p.at > fetchStartedAt);
  if (next.length !== items().length) setItems(next);
}

const VERB: Record<ReviewDecision, string> = { accept: "accept", reject: "reject", changes_requested: "changes requested" };
export function pendingLabel(p: PendingReview): string {
  return `${VERB[p.decision]}${p.merge ? " + merge" : ""} pending sync`;
}
