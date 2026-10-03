// Live updates: poll `GET /api/tip` and tell subscribers when the activity
// ledger moved. Polling pauses while the tab is hidden.

import { createResource, createSignal, onCleanup, type ResourceSource } from "solid-js";
import { api, unauthorized } from "./client";

export type LiveStatus = "connecting" | "live" | "paused" | "offline";

const [status, setStatus] = createSignal<LiveStatus>("connecting");
const [lastTip, setLastTip] = createSignal<string | null | undefined>(undefined);
const [lastChange, setLastChange] = createSignal<number | null>(null);

export { status as liveStatus, lastTip, lastChange };

const listeners = new Set<() => void>();

/** Calls `fn` whenever the ledger tip changes. Returns an unsubscribe function. */
export function onTipChange(fn: () => void): () => void {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

/** Tell every live resource to refetch now (e.g. right after our own write). */
export function invalidateAll(): void {
  setLastChange(Date.now());
  for (const fn of [...listeners]) fn();
}

/** Feed one tip observation into the change detector (exported for tests). */
export function observeTip(tip: string | null): boolean {
  const prev = lastTip();
  setLastTip(tip);
  if (prev !== undefined && prev !== tip) {
    invalidateAll();
    return true;
  }
  return false;
}

/**
 * Starts polling every `intervalMs`. Returns a stop function. Safe to call
 * once at app start.
 */
export function startLive(intervalMs = 3000): () => void {
  let timer: ReturnType<typeof setTimeout> | undefined;
  let stopped = false;
  let inFlight = false;

  const schedule = (ms: number) => {
    clearTimeout(timer);
    if (!stopped) timer = setTimeout(tick, ms);
  };

  async function tick() {
    if (stopped) return;
    if (unauthorized()) {
      // The app now shows the "open the link" screen; a new link reloads the page.
      setStatus("offline");
      return;
    }
    if (document.hidden) {
      setStatus("paused");
      return; // resumed by visibilitychange
    }
    if (inFlight) return schedule(intervalMs);
    inFlight = true;
    try {
      const { activity } = await api.tip();
      setStatus("live");
      observeTip(activity);
    } catch {
      // A 401 is surfaced globally by the client; either way we're not live.
      setStatus("offline");
    } finally {
      inFlight = false;
    }
    schedule(intervalMs);
  }

  const onVisibility = () => {
    if (document.hidden) {
      clearTimeout(timer);
      setStatus("paused");
    } else {
      schedule(0);
    }
  };

  document.addEventListener("visibilitychange", onVisibility);
  schedule(0);
  return () => {
    stopped = true;
    clearTimeout(timer);
    document.removeEventListener("visibilitychange", onVisibility);
  };
}

export type Live<T> = {
  /** Latest successful value; kept during live refetches and after a failed refetch. */
  data: () => T | undefined;
  /** The last error, cleared by the next success. */
  error: () => unknown;
  /** True while any fetch is in flight. */
  loading: () => boolean;
  /** When the fetch that produced `data()` started (ms), or null. */
  fetchedAt: () => number | null;
  refetch: () => void;
  mutate: (v: T | undefined) => void;
};

/**
 * `createResource` that refetches when the tip changes. A change of `source`
 * (e.g. a new route param) clears the old value so the page shows a skeleton,
 * while a live refetch keeps showing the previous value until the new one lands.
 */
export function createLive<S, T>(
  source: ResourceSource<S>,
  fetcher: (s: S) => Promise<T>,
  opts: { live?: boolean } = {},
): Live<T> {
  const [data, setData] = createSignal<T | undefined>(undefined);
  const [fetchedAt, setFetchedAt] = createSignal<number | null>(null);
  const [res, { refetch }] = createResource<T, S>(source, async (s, info) => {
    if (!info.refetching) setData(undefined);
    const started = Date.now();
    const v = await fetcher(s);
    setData(() => v);
    setFetchedAt(started);
    return v;
  });
  if (opts.live !== false) {
    const off = onTipChange(() => {
      void refetch();
    });
    onCleanup(off);
  }
  return {
    data,
    error: () => res.error as unknown,
    loading: () => res.loading,
    fetchedAt,
    refetch: () => void refetch(),
    mutate: (v) => setData(() => v),
  };
}
