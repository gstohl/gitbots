import { A, useLocation } from "@solidjs/router";
import { createEffect, createMemo, createSignal, For, on, Show, type JSX } from "solid-js";
import { api, isQueued } from "../api/client";
import { invalidateAll } from "../api/live";
import { addPending, pendingTasks } from "../api/pending";
import type { BoardAttempt, BoardTask, TaskStatus } from "../api/types";
import { ActorChip, AgentChip } from "../components/AgentChip";
import { ChecksBadge, Labels } from "../components/Bits";
import { PendingBadge, PendingReviewBadge } from "../components/PendingBadge";
import { RelativeTime } from "../components/RelativeTime";
import { StatePill } from "../components/StatePill";
import { EmptyState, ErrorState, errorText, Skeleton } from "../components/States";
import { hrefs } from "../lib/events";
import { shortId } from "../lib/format";
import { sessionOf, useData } from "../state";

const COLUMNS: { status: TaskStatus; title: string; hint: string }[] = [
  { status: "open", title: "Open", hint: "No live attempt yet" },
  { status: "in_progress", title: "In progress", hint: "Active, submitted or changes requested" },
  { status: "accepted", title: "Accepted", hint: "Accepted, not merged yet" },
  { status: "done", title: "Done", hint: "Merged" },
];

export default function BoardPage(): JSX.Element {
  const { board, project, index } = useData();
  const [filter, setFilter] = createSignal("");
  const [showForm, setShowForm] = createSignal(false);
  const location = useLocation();

  const tasks = createMemo(() => {
    const q = filter().trim().toLowerCase();
    const all = board.data()?.tasks ?? [];
    if (!q) return all;
    return all.filter(
      (t) =>
        t.title.toLowerCase().includes(q) ||
        t.id.toLowerCase().includes(q) ||
        t.labels.some((l) => l.toLowerCase().includes(q)),
    );
  });
  const byStatus = (s: TaskStatus) =>
    tasks()
      .filter((t) => t.status === s)
      .sort((a, b) => b.created_at.localeCompare(a.created_at));

  // `/board#tsk_...` scrolls to and highlights a task once the board is loaded.
  createEffect(
    on([() => location.hash, () => !!board.data()], ([hash, ready]) => {
      if (!ready || !hash) return;
      const id = decodeURIComponent(hash.replace(/^#/, ""));
      queueMicrotask(() => {
        const el = document.getElementById(id);
        if (!el) return;
        el.scrollIntoView({ block: "center" });
        el.classList.add("flash");
        setTimeout(() => el.classList.remove("flash"), 1600);
      });
    }),
  );

  return (
    <div class="page board-page">
      <header class="page-head">
        <div>
          <h1>Board</h1>
          <p class="page-sub">
            <Show when={board.data()} fallback="Loading tasks…">
              {(b) => (
                <>
                  {b().tasks.length} tasks · {b().attempts.length} attempts · {b().awaiting_review.length} awaiting review
                  <Show when={b().orphans > 0}>
                    <span title="Events that reference tasks or attempts this ledger doesn't know (e.g. not yet synced)">
                      {" "}
                      · {b().orphans} orphaned events
                    </span>
                  </Show>
                </>
              )}
            </Show>
          </p>
        </div>
        <div class="page-actions">
          <input
            class="input filter-input"
            type="search"
            placeholder="Filter tasks  /"
            aria-label="Filter tasks"
            data-shortcut-filter
            value={filter()}
            onInput={(e) => setFilter(e.currentTarget.value)}
          />
          <button type="button" class="btn btn-primary" onClick={() => setShowForm(!showForm())} aria-expanded={showForm()}>
            {showForm() ? "Close" : "New task"}
          </button>
        </div>
      </header>

      <Show when={showForm()}>
        <NewTaskForm canDecide={project.data()?.can_decide} onDone={() => setShowForm(false)} />
      </Show>

      <Show when={board.error() && !board.data()}>
        <ErrorState error={board.error()} retry={board.refetch} />
      </Show>
      <Show when={!board.data() && !board.error()}>
        <Skeleton rows={6} />
      </Show>
      <Show when={board.data() && board.data()!.tasks.length === 0}>
        <EmptyState title="No tasks yet">
          <p>
            File one with <strong>New task</strong>, or from a terminal: <code>gitbots task create "…"</code>.
          </p>
        </EmptyState>
      </Show>

      <Show when={board.data() && board.data()!.tasks.length > 0}>
        <div class="board">
          <For each={COLUMNS}>
            {(col) => (
              <section class={`board-col col-${col.status}`} aria-labelledby={`col-${col.status}`}>
                <h2 id={`col-${col.status}`} class="col-head" title={col.hint}>
                  <StatePill kind="task" value={col.status} compact />
                  <span class="count">{byStatus(col.status).length}</span>
                </h2>
                <ul class="col-list" role="list">
                  <Show when={col.status === "open"}>
                    <For each={pendingTasks()}>
                      {(p) => (
                        <li class="task-card is-pending">
                          <div class="task-head">
                            <span class="task-title">{p.title}</span>
                          </div>
                          <PendingBadge>pending sync</PendingBadge>
                        </li>
                      )}
                    </For>
                  </Show>
                  <For each={byStatus(col.status)} fallback={<Show when={col.status !== "open" || !pendingTasks().length}><li class="col-empty muted">Nothing here</li></Show>}>
                    {(t) => <TaskCard task={t} attempts={t.attempts.map((id) => index().attempts.get(id)).filter((a): a is BoardAttempt => !!a)} />}
                  </For>
                </ul>
              </section>
            )}
          </For>
        </div>
      </Show>
    </div>
  );
}

function TaskCard(props: { task: BoardTask; attempts: BoardAttempt[] }): JSX.Element {
  const { index } = useData();
  const t = () => props.task;
  // Newest attempt first; finished ones are de-emphasized.
  const attempts = () => [...props.attempts].sort((a, b) => b.started_at.localeCompare(a.started_at));
  return (
    <li id={t().id} class="task-card" data-nav-item tabindex="-1">
      <div class="task-head">
        <span class="task-title">{t().title}</span>
        <code class="id-badge" title={t().id}>
          {shortId(t().id)}
        </code>
      </div>
      <div class="task-meta">
        <Labels labels={t().labels} />
        <Show when={t().recipe}>
          <span class="recipe" title="Rendered from a recipe">
            ⧉ {t().recipe}
          </span>
        </Show>
      </div>
      <Show when={t().body}>
        <p class="task-body">{t().body}</p>
      </Show>
      <div class="task-by">
        <ActorChip actor={t().created_by} />
        <RelativeTime iso={t().created_at} />
      </div>
      <Show when={attempts().length}>
        <ul class="attempt-rows" role="list" aria-label="Attempts">
          <For each={attempts()}>
            {(a) => {
              const s = () => sessionOf(index(), a.session);
              return (
                <li class={`attempt-row${["rejected", "abandoned"].includes(a.state) ? " is-dim" : ""}`}>
                  <A href={hrefs.attempt(a.id)} class="attempt-link" title={a.branch}>
                    <StatePill kind="attempt" value={a.state} compact />
                    <code class="id-badge">{shortId(a.id)}</code>
                  </A>
                  <Show when={s()} fallback={<ActorChip actor={a.started_by} />}>
                    {(ss) => <AgentChip agent={ss().agent} session={ss().id} parent={ss().parent} />}
                  </Show>
                  <Show when={a.runs.length || a.state !== "active"}>
                    <ChecksBadge passed={a.checks_passed} runs={a.runs} />
                  </Show>
                  <PendingReviewBadge attempt={a.id} compact />
                </li>
              );
            }}
          </For>
        </ul>
      </Show>
    </li>
  );
}

export function NewTaskForm(props: { canDecide: boolean | undefined; onDone?: () => void }): JSX.Element {
  const [title, setTitle] = createSignal("");
  const [body, setBody] = createSignal("");
  const [labels, setLabels] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [created, setCreated] = createSignal<string | null>(null);
  const [queuedTitle, setQueuedTitle] = createSignal<string | null>(null);
  const disabled = () => props.canDecide === false;

  const submit = async (e: SubmitEvent) => {
    e.preventDefault();
    if (disabled() || busy() || !title().trim()) return;
    setBusy(true);
    setError(null);
    setCreated(null);
    setQueuedTitle(null);
    try {
      const ls = labels()
        .split(",")
        .map((l) => l.trim())
        .filter(Boolean);
      const t = title().trim();
      const res = await api.createTask({
        title: t,
        ...(body().trim() ? { body: body().trim() } : {}),
        ...(ls.length ? { labels: ls } : {}),
      });
      if (isQueued(res)) {
        // Hosted: stored in the outbox; it shows up after the next `gitbots sync`.
        addPending({ kind: "task", outbox: res.outbox, title: t, at: Date.now() });
        setCreated(null);
        setQueuedTitle(t);
      } else {
        setQueuedTitle(null);
        setCreated(res.task);
      }
      setTitle("");
      setBody("");
      setLabels("");
      invalidateAll();
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <form class="card new-task" onSubmit={submit} aria-labelledby="new-task-h">
      <h2 id="new-task-h" class="section-title">
        New task
      </h2>
      <Show when={disabled()}>
        <p class="notice notice-warn" role="note">
          Read-only: <code>gitbots ui</code> was started from an agent session, so it can't act as a human. Start it from your own
          terminal to file tasks and make decisions.
        </p>
      </Show>
      <fieldset disabled={disabled() || busy()}>
        <label class="field">
          <span class="field-label">Title</span>
          <input class="input" required value={title()} onInput={(e) => setTitle(e.currentTarget.value)} placeholder="Add a health endpoint" />
        </label>
        <label class="field">
          <span class="field-label">
            Body <span class="muted">(optional, what done looks like)</span>
          </span>
          <textarea class="input textarea" rows="3" value={body()} onInput={(e) => setBody(e.currentTarget.value)} />
        </label>
        <label class="field">
          <span class="field-label">
            Labels <span class="muted">(comma-separated)</span>
          </span>
          <input class="input" value={labels()} onInput={(e) => setLabels(e.currentTarget.value)} placeholder="api, reliability" />
        </label>
        <div class="form-actions">
          <button class="btn btn-primary" type="submit" disabled={disabled() || busy() || !title().trim()}>
            {busy() ? "Creating…" : "Create task"}
          </button>
          <Show when={props.onDone}>
            <button class="btn" type="button" onClick={() => props.onDone?.()}>
              Cancel
            </button>
          </Show>
        </div>
      </fieldset>
      <div aria-live="polite" class="form-status">
        <Show when={error()}>
          <p class="notice notice-bad">{error()}</p>
        </Show>
        <Show when={queuedTitle()}>
          {(t) => (
            <p class="notice notice-pending">
              <PendingBadge>pending sync</PendingBadge> Queued “{t()}”: applied on the next <code>gitbots sync</code>.
            </p>
          )}
        </Show>
        <Show when={created()}>
          {(id) => (
            <p class="notice notice-good">
              Created <A href={hrefs.task(id())}>{shortId(id())}</A>. Agents pick it up from <code>gitbots status</code>.
            </p>
          )}
        </Show>
      </div>
    </form>
  );
}
