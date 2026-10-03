import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  _resetTokenForTests,
  api,
  ApiError,
  captureToken,
  clearToken,
  errorMessage,
  getToken,
  isQueued,
  setTransport,
  setUnauthorized,
  TOKEN_KEY,
  unauthorized,
} from "./client";

function memStorage() {
  const m = new Map<string, string>();
  return {
    getItem: (k: string) => m.get(k) ?? null,
    setItem: (k: string, v: string) => void m.set(k, v),
    removeItem: (k: string) => void m.delete(k),
    map: m,
  };
}

function env(hash: string, stored?: string) {
  const storage = memStorage();
  if (stored) storage.setItem(TOKEN_KEY, stored);
  const replaceState = vi.fn();
  return {
    location: { hash, pathname: "/attempts/att_x", search: "?tab=checks" },
    history: { replaceState, state: { n: 1 } },
    storage,
    replaceState,
  };
}

beforeEach(() => {
  _resetTokenForTests();
  setUnauthorized(false);
});
afterEach(() => setTransport(null));

describe("token handling", () => {
  it("moves #token= into sessionStorage and strips it from the URL", () => {
    const e = env("#token=abc123");
    expect(captureToken(e)).toBe("abc123");
    expect(e.storage.map.get(TOKEN_KEY)).toBe("abc123");
    expect(e.replaceState).toHaveBeenCalledWith({ n: 1 }, "", "/attempts/att_x?tab=checks");
  });

  it("keeps other hash parameters", () => {
    const e = env("#token=abc&view=compact");
    captureToken(e);
    expect(e.replaceState).toHaveBeenCalledWith({ n: 1 }, "", "/attempts/att_x?tab=checks#view=compact");
  });

  it("a new link replaces the stored token", () => {
    const e = env("#token=new", "old");
    expect(captureToken(e)).toBe("new");
    expect(e.storage.map.get(TOKEN_KEY)).toBe("new");
  });

  it("falls back to the stored token and leaves the URL alone", () => {
    const e = env("", "stored");
    expect(captureToken(e)).toBe("stored");
    expect(e.replaceState).not.toHaveBeenCalled();
    const e2 = env("#section", "stored");
    expect(captureToken(e2)).toBe("stored");
    expect(e2.replaceState).not.toHaveBeenCalled();
  });

  it("strips an empty token without storing it", () => {
    const e = env("#token=");
    expect(captureToken(e)).toBeNull();
    expect(e.replaceState).toHaveBeenCalled();
    expect(e.storage.map.has(TOKEN_KEY)).toBe(false);
  });

  it("works when storage throws (private mode)", () => {
    const e = env("#token=t1");
    e.storage.setItem = () => {
      throw new Error("blocked");
    };
    e.storage.getItem = () => {
      throw new Error("blocked");
    };
    expect(captureToken(e)).toBe("t1");
    expect(getToken(e)).toBe("t1");
  });

  it("clearToken forgets it", () => {
    const e = env("#token=t2");
    captureToken(e);
    clearToken(e);
    expect(getToken(e)).toBeNull();
  });
});

describe("requests", () => {
  it("sends the bearer token and parses JSON", async () => {
    captureToken(env("#token=sekrit"));
    const calls: [string, RequestInit][] = [];
    setTransport(async (path, init) => {
      calls.push([path, init]);
      return new Response(JSON.stringify({ activity: "evt_1" }), { status: 200 });
    });
    expect(await api.tip()).toEqual({ activity: "evt_1" });
    const [path, init] = calls[0]!;
    expect(path).toBe("/api/tip");
    expect(new Headers(init.headers).get("Authorization")).toBe("Bearer sekrit");
  });

  it("encodes ids and query parameters", async () => {
    const paths: string[] = [];
    setTransport(async (p) => {
      paths.push(p);
      return new Response("[]", { status: 200 });
    });
    await api.events({ kind: "attempt", session: "ses_1", limit: 50 });
    await api.attempt("gitbots/attempt/x-1");
    await api.log("runs/2026/10/03/run_1/test.log").catch(() => {});
    expect(paths).toEqual([
      "/api/events?kind=attempt&session=ses_1&limit=50",
      "/api/attempts/gitbots%2Fattempt%2Fx-1",
      "/api/logs?path=runs%2F2026%2F10%2F03%2Frun_1%2Ftest.log",
    ]);
  });

  it("posts JSON bodies", async () => {
    let seen: RequestInit | undefined;
    setTransport(async (_p, init) => {
      seen = init;
      return new Response(JSON.stringify({ attempt: "att_1", decision: "accept", merged: null }), { status: 200 });
    });
    await api.review("att_1", { decision: "accept", merge: true });
    expect(seen?.method).toBe("POST");
    expect(new Headers(seen?.headers).get("Content-Type")).toBe("application/json");
    expect(JSON.parse(String(seen?.body))).toEqual({ decision: "accept", merge: true });
  });

  it("surfaces {error} verbatim with the status", async () => {
    setTransport(async () => new Response(JSON.stringify({ error: "needs a human with role maintainer" }), { status: 403 }));
    const err = await api.review("att_1", { decision: "accept" }).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect((err as ApiError).status).toBe(403);
    expect((err as ApiError).message).toBe("needs a human with role maintainer");
    expect(unauthorized()).toBe(false);
  });

  it("flags 401 globally", async () => {
    setTransport(async () => new Response(JSON.stringify({ error: "bad token" }), { status: 401 }));
    await expect(api.project()).rejects.toThrow("bad token");
    expect(unauthorized()).toBe(true);
  });

  it("falls back to text or the status for non-JSON errors", async () => {
    expect(await errorMessage(new Response("upstream timeout", { status: 502 }))).toBe("upstream timeout");
    expect(await errorMessage(new Response("<html>oops</html>", { status: 500, statusText: "Internal" }))).toBe("HTTP 500 Internal");
    expect(await errorMessage(new Response("", { status: 404 }))).toBe("HTTP 404");
  });

  it("turns network failures into status 0", async () => {
    setTransport(async () => {
      throw new TypeError("Failed to fetch");
    });
    const err = (await api.board().catch((e: unknown) => e)) as ApiError;
    expect(err.status).toBe(0);
    expect(err.message).toContain("gitbots ui");
  });

  it("recognizes hosted 202 queued answers", async () => {
    setTransport(async () => new Response(JSON.stringify({ queued: true, outbox: "obx_1" }), { status: 202 }));
    const res = await api.createTask({ title: "x" });
    expect(isQueued(res)).toBe(true);
    expect(isQueued({ task: "tsk_1" })).toBe(false);
    expect(isQueued(null)).toBe(false);
  });
});
