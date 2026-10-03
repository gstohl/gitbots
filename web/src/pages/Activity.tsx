import { useSearchParams } from "@solidjs/router";
import { createMemo, For, Show, type JSX } from "solid-js";
import { api } from "../api/client";
import { createLive } from "../api/live";
import type { Event, EventQuery } from "../api/types";
import { EventRow } from "../components/EventRow";
import { EmptyState, ErrorState, Skeleton } from "../components/States";
import { describeEvent, KIND_GROUPS } from "../lib/events";
import { actorLabel, shortId } from "../lib/format";
import { useData } from "../state";

const PAGE = 200;

function one(v: string | string[] | undefined): string {
  return (Array.isArray(v) ? v[0] : v) ?? "";
}

function dayLabel(iso: string, now = new Date()): string {
  const d = new Date(iso);
  const start = (x: Date) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  const diff = Math.round((start(now) - start(d)) / 86_400_000);
  if (diff === 0) return "Today";
  if (diff === 1) return "Yesterday";
  return d.toLocaleDateString(undefined, { weekday: "short", month: "short", day: "numeric", year: d.getFullYear() === now.getFullYear() ? undefined : "numeric" });
}

export default function ActivityPage(): JSX.Element {
  const [search, setSearch] = useSearchParams();
  const { board, index } = useData();

  const kind = () => one(search.kind);
  const session = () => one(search.session);
  const task = () => one(search.task);
  const text = () => one(search.q);
  const limit = () => Number(one(search.limit)) || PAGE;

  // A string key so unrelated param changes (e.g. the text filter) don't refetch.
  const key = () => JSON.stringify({ kind: kind(), session: session(), task: task(), limit: limit() });
  const events = createLive(key, (k) => {
    const q = JSON.parse(k) as { kind: string; session: string; task: string; limit: number };
    const query: EventQuery = { limit: q.limit };
    if (q.kind) query.kind = q.kind;
    if (q.session) query.session = q.session;
    if (q.task) query.task = q.task;
    return api.events(query);
  });

  // Newest first, then the client-side text filter.
  const rows = createMemo(() => {
    const all = [...(events.data() ?? [])].reverse();
    const k = kind();
    const byKind = k ? all.filter((e) => e.kind === k || e.kind.startsWith(`${k}.`)) : all;
    const q = text().trim().toLowerCase();
    if (!q) return byKind;
    return byKind.filter((e) => {
      const d = describeEvent(e);
      return d.text.toLowerCase().includes(q) || e.kind.includes(q) || actorLabel(e.actor).toLowerCase().includes(q) || e.id.toLowerCase().includes(q);
    });
  });
  const grouped = createMemo(() => {
    const out: { day: string; events: Event[] }[] = [];
    for (const e of rows()) {
      const day = dayLabel(e.ts);
      const last = out[out.length - 1];
      if (last && last.day === day) last.events.push(e);
      else out.push({ day, events: [e] });
    }
    return out;
  });
  const ctx = { taskTitle: (id: string) => index().tasks.get(id)?.title };
  const update = (patch: Record<string, string | undefined>) => setSearch(patch, { replace: true });
  const anyFilter = () => !!(kind() || session() || task() || text());
  const sessions = () =>
    [...(board.data()?.sessions ?? [])].sort((a, b) => b.session.started_at.localeCompare(a.session.started_at));
  const tasks = () => [...(board.data()?.tasks ?? [])].sort((a, b) => b.created_at.localeCompare(a.created_at));

  return (
    <div class="page activity-page">
      <header class="page-head">
        <div>
          <h1>Activity</h1>
          <p class="page-sub">The ledger on gitbots/activity, newest first. Updates live.</p>
        </div>
      </header>

      <div class="filters" role="search">
        <div class="kind-filter" role="group" aria-label="Kind">
          <button type="button" class={`chip-btn${kind() ? "" : " on"}`} aria-pressed={!kind()} onClick={() => update({ kind: undefined })}>
            all
          </button>
          <For each={KIND_GROUPS}>
            {(g) => (
              <button
                type="button"
                class={`chip-btn kind-${g}${kind() === g ? " on" : ""}`}
                aria-pressed={kind() === g}
                onClick={() => update({ kind: kind() === g ? undefined : g })}
              >
                {g}
              </button>
            )}
          </For>
        </div>
        <div class="filter-row">
          <select class="select" aria-label="Session" value={session()} onChange={(e) => update({ session: e.currentTarget.value || undefined })}>
            <option value="">All sessions</option>
            <For each={sessions()}>
              {(s) => (
                <option value={s.session.id}>
                  {s.session.parent ? "↳ " : ""}
                  {s.session.agent.model}@{s.session.agent.client} · {shortId(s.session.id)}
                  {s.session.label ? ` · ${s.session.label}` : ""}
                </option>
              )}
            </For>
            <Show when={session() && !index().sessions.has(session())}>
              <option value={session()}>{shortId(session())}</option>
            </Show>
          </select>
          <select class="select" aria-label="Task" value={task()} onChange={(e) => update({ task: e.currentTarget.value || undefined })}>
            <option value="">All tasks</option>
            <For each={tasks()}>{(t) => <option value={t.id}>{t.title}</option>}</For>
            <Show when={task() && !index().tasks.has(task())}>
              <option value={task()}>{shortId(task())}</option>
            </Show>
          </select>
          <input
            class="input filter-input"
            type="search"
            placeholder="Filter text  /"
            aria-label="Filter events by text"
            data-shortcut-filter
            value={text()}
            onInput={(e) => update({ q: e.currentTarget.value || undefined })}
          />
          <Show when={anyFilter()}>
            <button type="button" class="btn btn-sm btn-ghost" onClick={() => setSearch({ kind: undefined, session: undefined, task: undefined, q: undefined, limit: undefined }, { replace: true })}>
              Clear
            </button>
          </Show>
        </div>
      </div>

      <Show when={events.error() && !events.data()}>
        <ErrorState error={events.error()} retry={events.refetch} />
      </Show>
      <Show when={!events.data() && !events.error()}>
        <Skeleton rows={10} />
      </Show>
      <Show when={events.data() && rows().length === 0}>
        <EmptyState title={anyFilter() ? "No events match these filters" : "No activity yet"}>
          <Show when={!anyFilter()}>
            <p>
              Events appear once agents start sessions: <code>gitbots session start</code>.
            </p>
          </Show>
        </EmptyState>
      </Show>
      <Show when={rows().length}>
        <div class="feed" aria-live="off">
          <For each={grouped()}>
            {(g) => (
              <section class="feed-day" aria-label={g.day}>
                <h2 class="feed-day-label">{g.day}</h2>
                <ol class="event-list" role="list">
                  <For each={g.events}>{(e) => <EventRow event={e} ctx={ctx} />}</For>
                </ol>
              </section>
            )}
          </For>
          <Show when={(events.data()?.length ?? 0) >= limit()}>
            <button type="button" class="btn load-more" onClick={() => update({ limit: String(limit() + PAGE) })} disabled={events.loading()}>
              {events.loading() ? "Loading…" : "Load older events"}
            </button>
          </Show>
        </div>
      </Show>
    </div>
  );
}
