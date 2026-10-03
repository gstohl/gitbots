// Tiny typed client for the gitbots HTTP API (docs/API.md).
//
// Auth: `gitbots ui` prints `http://127.0.0.1:7777/#token=<hex>`. On first load
// we take the token from `location.hash`, keep it in sessionStorage (so it
// survives reloads in this tab but not a new browser session) and strip it
// from the URL so it doesn't end up in history, bookmarks or screenshots.

import { createSignal } from "solid-js";
import type {
  ApiErrorBody,
  AttemptDetail,
  BoardResponse,
  Event,
  EventQuery,
  Inbox,
  NewTask,
  NewTaskResponse,
  ProjectInfo,
  QueuedResponse,
  RecipesResponse,
  ReviewOutcome,
  ReviewRequest,
  Stats,
  TipResponse,
  WorkflowsResponse,
} from "./types";

export const TOKEN_KEY = "gitbots.token";

/** A non-2xx response (or a network failure, `status === 0`). */
export class ApiError extends Error {
  readonly status: number;
  constructor(status: number, message: string) {
    super(message);
    this.name = "ApiError";
    this.status = status;
  }
}

// ---- token -----------------------------------------------------------------

type TokenEnv = {
  location: Pick<Location, "hash" | "pathname" | "search">;
  history: Pick<History, "replaceState" | "state">;
  storage: Pick<Storage, "getItem" | "setItem" | "removeItem">;
};

function browserEnv(): TokenEnv {
  return { location: window.location, history: window.history, storage: window.sessionStorage };
}

function safeGet(storage: TokenEnv["storage"]): string | null {
  try {
    return storage.getItem(TOKEN_KEY);
  } catch {
    return null;
  }
}

/** In-memory fallback when sessionStorage is unavailable. */
let memoryToken: string | null = null;

/**
 * Moves `#token=...` from the URL into sessionStorage and returns the token
 * to use (the new one, else the stored one). Other hash parameters survive.
 */
export function captureToken(env: TokenEnv = browserEnv()): string | null {
  const raw = env.location.hash.replace(/^#/, "");
  if (raw) {
    const params = new URLSearchParams(raw);
    const token = params.get("token");
    if (token !== null) {
      params.delete("token");
      const rest = params.toString();
      const url = env.location.pathname + env.location.search + (rest ? `#${rest}` : "");
      env.history.replaceState(env.history.state, "", url);
      if (token) {
        memoryToken = token;
        try {
          env.storage.setItem(TOKEN_KEY, token);
        } catch {
          /* private mode: the in-memory copy still works for this page */
        }
        return token;
      }
    }
  }
  const stored = safeGet(env.storage);
  if (stored) memoryToken = stored;
  return stored ?? memoryToken;
}

export function getToken(env?: TokenEnv): string | null {
  if (memoryToken) return memoryToken;
  return safeGet((env ?? browserEnv()).storage);
}

export function clearToken(env: TokenEnv = browserEnv()): void {
  memoryToken = null;
  try {
    env.storage.removeItem(TOKEN_KEY);
  } catch {
    /* ignore */
  }
}

/** Test hook: reset the module's in-memory token. */
export function _resetTokenForTests(): void {
  memoryToken = null;
}

// ---- transport ---------------------------------------------------------------

export type Transport = (path: string, init: RequestInit) => Promise<Response>;

export const isMock = import.meta.env.VITE_GITBOTS_MOCK === "1";

let transport: Transport | null = null;

async function getTransport(): Promise<Transport> {
  if (transport) return transport;
  let t: Transport;
  if (import.meta.env.VITE_GITBOTS_MOCK === "1") {
    // Only bundled when VITE_GITBOTS_MOCK=1 (the condition is constant-folded).
    const m = await import("../mocks/server");
    t = m.mockFetch;
  } else {
    t = (path, init) => fetch(path, init);
  }
  transport = t;
  return t;
}

/** Replace the transport (tests, mock scenarios). `null` restores the default. */
export function setTransport(t: Transport | null): void {
  transport = t;
}

/** Set when any request gets a 401; the app then shows the "open the link" screen. */
const [unauthorized, setUnauthorized] = createSignal(false);
export { unauthorized, setUnauthorized };

/** Extracts the message from an error response: `{error}`, else text, else the status. */
export async function errorMessage(res: Response): Promise<string> {
  let text = "";
  try {
    text = await res.text();
  } catch {
    /* body unreadable */
  }
  if (text) {
    try {
      const body = JSON.parse(text) as Partial<ApiErrorBody>;
      if (body && typeof body.error === "string" && body.error) return body.error;
    } catch {
      /* not JSON */
    }
    const trimmed = text.trim();
    if (trimmed && trimmed.length < 500 && !trimmed.startsWith("<")) return trimmed;
  }
  return `HTTP ${res.status}${res.statusText ? ` ${res.statusText}` : ""}`;
}

async function request(path: string, init: RequestInit = {}): Promise<Response> {
  const headers = new Headers(init.headers);
  const token = getToken();
  if (token) headers.set("Authorization", `Bearer ${token}`);
  if (init.body !== undefined && !headers.has("Content-Type")) {
    headers.set("Content-Type", "application/json");
  }
  const send = await getTransport();
  let res: Response;
  try {
    res = await send(path, { ...init, headers });
  } catch (e) {
    throw new ApiError(0, `Can't reach the gitbots server (${e instanceof Error ? e.message : String(e)}). Is \`gitbots ui\` still running?`);
  }
  if (res.status === 401) setUnauthorized(true);
  if (!res.ok) throw new ApiError(res.status, await errorMessage(res));
  return res;
}

async function getJson<T>(path: string): Promise<T> {
  const res = await request(path, { headers: { Accept: "application/json" } });
  return (await res.json()) as T;
}

async function getText(path: string): Promise<string> {
  const res = await request(path, { headers: { Accept: "text/plain" } });
  return res.text();
}

async function postJson<T>(path: string, body: unknown): Promise<T> {
  const res = await request(path, {
    method: "POST",
    headers: { Accept: "application/json" },
    body: JSON.stringify(body),
  });
  return (await res.json()) as T;
}

function query(q: Record<string, string | number | undefined>): string {
  const p = new URLSearchParams();
  for (const [k, v] of Object.entries(q)) if (v !== undefined && v !== "") p.set(k, String(v));
  const s = p.toString();
  return s ? `?${s}` : "";
}

const enc = encodeURIComponent;

/** True for a hosted 202 `{queued: true, outbox}` answer to a write. */
export function isQueued(x: unknown): x is QueuedResponse {
  return typeof x === "object" && x !== null && (x as { queued?: unknown }).queued === true && typeof (x as { outbox?: unknown }).outbox === "string";
}

export const api = {
  project: () => getJson<ProjectInfo>("/api/project"),
  tip: () => getJson<TipResponse>("/api/tip"),
  board: () => getJson<BoardResponse>("/api/board"),
  inbox: () => getJson<Inbox>("/api/inbox"),
  events: (q: EventQuery = {}) => getJson<Event[]>(`/api/events${query(q)}`),
  stats: () => getJson<Stats>("/api/stats"),
  attempt: (id: string) => getJson<AttemptDetail>(`/api/attempts/${enc(id)}`),
  attemptDiff: (id: string) => getText(`/api/attempts/${enc(id)}/diff`),
  log: (path: string) => getText(`/api/logs${query({ path })}`),
  workflows: () => getJson<WorkflowsResponse>("/api/workflows"),
  recipes: () => getJson<RecipesResponse>("/api/recipes"),
  /** Hosted: may answer 202 `{queued, outbox}` instead (see `isQueued`). */
  createTask: (body: NewTask) => postJson<NewTaskResponse | QueuedResponse>("/api/tasks", body),
  /** Hosted: may answer 202 `{queued, outbox}` instead (see `isQueued`). */
  review: (id: string, body: ReviewRequest) =>
    postJson<ReviewOutcome | QueuedResponse>(`/api/attempts/${enc(id)}/review`, body),
};
