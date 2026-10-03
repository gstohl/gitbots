import { A, useLocation } from "@solidjs/router";
import { createEffect, createMemo, createSignal, For, on, Show, type JSX } from "solid-js";
import type { BoardAttempt, SessionView } from "../api/types";
import { AgentChip } from "../components/AgentChip";
import { RelativeTime } from "../components/RelativeTime";
import { StatePill } from "../components/StatePill";
import { EmptyState, ErrorState, Skeleton } from "../components/States";
import { hrefs } from "../lib/events";
import { shortId } from "../lib/format";
import { useData } from "../state";

type Node = { view: SessionView; children: Node[] };

/** Builds the parent -> subagent forest; sessions whose parent is unknown become roots. */
export function sessionTree(sessions: SessionView[]): Node[] {
  const nodes = new Map<string, Node>(sessions.map((v) => [v.session.id, { view: v, children: [] }]));
  const roots: Node[] = [];
  for (const n of nodes.values()) {
    const p = n.view.session.parent;
    const parent = p ? nodes.get(p) : undefined;
    if (parent && parent !== n) parent.children.push(n);
    else roots.push(n);
  }
  const byStart = (a: Node, b: Node) => b.view.session.started_at.localeCompare(a.view.session.started_at);
  const sortRec = (list: Node[]) => {
    list.sort(byStart);
    for (const n of list) sortRec(n.children);
  };
  sortRec(roots);
  return roots;
}

export default function SessionsPage(): JSX.Element {
  const { board } = useData();
  const [openOnly, setOpenOnly] = createSignal(false);
  const [filter, setFilter] = createSignal("");
  const location = useLocation();

  const attemptsBySession = createMemo(() => {
    const m = new Map<string, BoardAttempt[]>();
    for (const a of board.data()?.attempts ?? []) {
      if (!a.session) continue;
      const list = m.get(a.session) ?? [];
      list.push(a);
      m.set(a.session, list);
    }
    return m;
  });

  const tree = createMemo(() => {
    const q = filter().trim().toLowerCase();
    const all = board.data()?.sessions ?? [];
    const keep = (v: SessionView) => {
      const s = v.session;
      if (openOnly() && v.ended) return false;
      if (!q) return true;
      return [s.id, s.agent.model, s.agent.client, s.agent.provider, s.role ?? "", s.label ?? "", s.operator ?? ""].some((x) =>
        x.toLowerCase().includes(q),
      );
    };
    // Keep ancestors of matching sessions so the tree stays connected.
    const byId = new Map(all.map((v) => [v.session.id, v]));
    const ids = new Set<string>();
    for (const v of all) {
      if (!keep(v)) continue;
      let cur: SessionView | undefined = v;
      while (cur && !ids.has(cur.session.id)) {
        ids.add(cur.session.id);
        cur = cur.session.parent ? byId.get(cur.session.parent) : undefined;
      }
    }
    return sessionTree(all.filter((v) => ids.has(v.session.id)));
  });

  const counts = () => {
    const all = board.data()?.sessions ?? [];
    return { total: all.length, open: all.filter((s) => !s.ended).length, sub: all.filter((s) => s.session.parent).length };
  };

  createEffect(
    on([() => location.hash, () => !!board.data()], ([hash, ready]) => {
      if (!ready || !hash) return;
      const el = document.getElementById(decodeURIComponent(hash.slice(1)));
      if (!el) return;
      el.scrollIntoView({ block: "center" });
      el.classList.add("flash");
      setTimeout(() => el.classList.remove("flash"), 1600);
    }),
  );

  return (
    <div class="page sessions-page">
      <header class="page-head">
        <div>
          <h1>Sessions</h1>
          <p class="page-sub">
            <Show when={board.data()} fallback="Loading sessions…">
              {counts().total} sessions · {counts().open} open · {counts().sub} subagents
            </Show>
          </p>
        </div>
        <div class="page-actions">
          <label class="check">
            <input type="checkbox" checked={openOnly()} onChange={(e) => setOpenOnly(e.currentTarget.checked)} /> Open only
          </label>
          <input
            class="input filter-input"
            type="search"
            placeholder="Filter sessions  /"
            aria-label="Filter sessions"
            data-shortcut-filter
            value={filter()}
            onInput={(e) => setFilter(e.currentTarget.value)}
          />
        </div>
      </header>

      <Show when={board.error() && !board.data()}>
        <ErrorState error={board.error()} retry={board.refetch} />
      </Show>
      <Show when={!board.data() && !board.error()}>
        <Skeleton rows={6} />
      </Show>
      <Show when={board.data() && tree().length === 0}>
        <EmptyState title={filter() || openOnly() ? "No sessions match" : "No sessions yet"}>
          <p>
            An agent starts one with <code>gitbots session start</code> or the MCP <code>session_start</code> tool.
          </p>
        </EmptyState>
      </Show>
      <Show when={tree().length}>
        <ul class="session-tree" role="tree" aria-label="Sessions">
          <For each={tree()}>{(n) => <SessionNode node={n} depth={0} attempts={attemptsBySession()} />}</For>
        </ul>
      </Show>
    </div>
  );
}

function SessionNode(props: { node: Node; depth: number; attempts: Map<string, BoardAttempt[]> }): JSX.Element {
  const s = () => props.node.view.session;
  const attempts = () => props.attempts.get(s().id) ?? [];
  return (
    <li role="treeitem" aria-expanded={props.node.children.length ? true : undefined} aria-level={props.depth + 1} class="session-item">
      <div id={s().id} class={`session-row${props.node.view.ended ? " is-ended" : ""}`} data-nav-item tabindex="-1">
        <div class="session-main">
          <AgentChip agent={s().agent} session={s().id} parent={s().parent} />
          <Show when={s().role}>
            <span class="role">{s().role}</span>
          </Show>
          <Show when={s().label}>
            <span class="session-label">{s().label}</span>
          </Show>
          <span class={`status-dot ${props.node.view.ended ? "ended" : "open"}`}>{props.node.view.ended ? "ended" : "open"}</span>
        </div>
        <div class="session-meta">
          <span>
            started <RelativeTime iso={s().started_at} />
          </span>
          <Show when={s().operator}>
            <span>for @{s().operator}</span>
          </Show>
          <Show when={s().external_id}>
            <code class="small muted" title="The client's own id for this chat">
              {s().external_id}
            </code>
          </Show>
          <A href={`/activity?session=${encodeURIComponent(s().id)}`} class="small">
            activity
          </A>
        </div>
        <Show when={attempts().length}>
          <div class="session-attempts">
            <For each={attempts()}>
              {(a) => (
                <A href={hrefs.attempt(a.id)} class="session-attempt" title={a.branch}>
                  <StatePill kind="attempt" value={a.state} compact />
                  <code>{shortId(a.id)}</code>
                </A>
              )}
            </For>
          </div>
        </Show>
      </div>
      <Show when={props.node.children.length}>
        <ul role="group" class="session-children">
          <For each={props.node.children}>{(c) => <SessionNode node={c} depth={props.depth + 1} attempts={props.attempts} />}</For>
        </ul>
      </Show>
    </li>
  );
}
