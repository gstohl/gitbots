// App-wide live data shared by the shell and several pages: the project
// (manifest, viewer, can_decide), the board and the inbox. Each refetches when
// the ledger tip changes; pages fetch their own detail data.

import { createContext, createEffect, createMemo, useContext, type JSX } from "solid-js";
import { api } from "./api/client";
import { createLive, type Live } from "./api/live";
import { reconcileAttempts, reconcileOutbox, reconcileTasks } from "./api/pending";
import type { BoardAttempt, BoardResponse, BoardTask, Inbox, ProjectInfo, Session, SessionView } from "./api/types";

export type AppData = {
  project: Live<ProjectInfo>;
  board: Live<BoardResponse>;
  inbox: Live<Inbox>;
  /** Lookups over the board, recomputed when it changes. */
  index: () => BoardIndex;
};

export type BoardIndex = {
  tasks: Map<string, BoardTask>;
  attempts: Map<string, BoardAttempt>;
  sessions: Map<string, SessionView>;
};

const Ctx = createContext<AppData>();

export function buildIndex(b: BoardResponse | undefined): BoardIndex {
  return {
    tasks: new Map((b?.tasks ?? []).map((t) => [t.id, t])),
    attempts: new Map((b?.attempts ?? []).map((a) => [a.id, a])),
    sessions: new Map((b?.sessions ?? []).map((s) => [s.session.id, s])),
  };
}

export function DataProvider(props: { children: JSX.Element }): JSX.Element {
  const project = createLive(() => true, () => api.project());
  const board = createLive(() => true, () => api.board());
  const inbox = createLive(() => true, () => api.inbox());
  const index = createMemo(() => buildIndex(board.data()));
  // Hosted: drop "pending sync" markers once the ledger shows the decision.
  createEffect(() => {
    const b = board.data();
    if (!b) return;
    reconcileAttempts(b.attempts);
    reconcileTasks(b.tasks);
  });
  createEffect(() => {
    const p = project.data();
    if (p?.hosted) reconcileOutbox(p.pending_outbox, project.fetchedAt());
  });
  return <Ctx.Provider value={{ project, board, inbox, index }}>{props.children}</Ctx.Provider>;
}

export function useData(): AppData {
  const c = useContext(Ctx);
  if (!c) throw new Error("useData outside <DataProvider>");
  return c;
}

/** The session record for an id, if the board knows it. */
export function sessionOf(index: BoardIndex, id: string | null | undefined): Session | undefined {
  return id ? index.sessions.get(id)?.session : undefined;
}
