import { A } from "@solidjs/router";
import { createMemo, createSignal, For, Show, type JSX } from "solid-js";
import type { AttemptView, ReportView } from "../api/types";
import { ActorChip, AgentChip } from "../components/AgentChip";
import { ChecksBadge, DiffStatView } from "../components/Bits";
import { PendingReviewBadge } from "../components/PendingBadge";
import { RelativeTime } from "../components/RelativeTime";
import { StatePill } from "../components/StatePill";
import { EmptyState, ErrorState, Skeleton } from "../components/States";
import { hrefs } from "../lib/events";
import { plural, shortId } from "../lib/format";
import { sessionOf, useData, type BoardIndex } from "../state";

export default function InboxPage(): JSX.Element {
  const { inbox, index } = useData();
  const awaiting = () => inbox.data()?.awaiting_review ?? [];
  const reports = () => inbox.data()?.reports ?? [];
  const blockers = () => reports().filter((r) => r.report.level === "blocker").length;
  const zero = () => !!inbox.data() && awaiting().length === 0 && reports().length === 0;

  return (
    <div class="page inbox-page">
      <header class="page-head">
        <div>
          <h1>Inbox</h1>
          <p class="page-sub">
            <Show when={inbox.data()} fallback="Loading what needs you…">
              {plural(awaiting().length, "attempt")} awaiting your decision
              <Show when={blockers()}>
                {" · "}
                <span class="text-bad">{plural(blockers(), "blocker")}</span>
              </Show>
              <Show when={reports().length}>{` · ${plural(reports().length, "report")}`}</Show>
            </Show>
          </p>
        </div>
      </header>

      <Show when={inbox.error() && !inbox.data()}>
        <ErrorState error={inbox.error()} retry={inbox.refetch} />
      </Show>
      <Show when={!inbox.data() && !inbox.error()}>
        <Skeleton rows={5} class="cards-skeleton" />
      </Show>

      <Show when={zero()}>
        <EmptyState title="Inbox zero" icon={<InboxZeroArt />} class="inbox-zero">
          <p>Nothing is waiting for you. Agents are working; new submissions and reports land here live.</p>
          <p>
            <A href="/board">Open the board</A> or <A href="/activity">watch the activity feed</A>.
          </p>
        </EmptyState>
      </Show>

      <Show when={awaiting().length}>
        <section class="inbox-section" aria-labelledby="awaiting-h">
          <h2 id="awaiting-h" class="section-title">
            Awaiting your decision <span class="count">{awaiting().length}</span>
          </h2>
          <ul class="card-list" role="list">
            <For each={awaiting()}>{(a) => <AwaitingCard attempt={a} index={index()} />}</For>
          </ul>
        </section>
      </Show>

      <Show when={reports().length}>
        <section class="inbox-section" aria-labelledby="reports-h">
          <h2 id="reports-h" class="section-title">
            Reports <span class="count">{reports().length}</span>
          </h2>
          <ul class="report-list" role="list">
            <For each={reports()}>{(r) => <ReportCard report={r} index={index()} />}</For>
          </ul>
        </section>
      </Show>
    </div>
  );
}

export function AwaitingCard(props: { attempt: AttemptView; index: BoardIndex }): JSX.Element {
  const a = () => props.attempt;
  const task = () => props.index.tasks.get(a().task);
  const boardAttempt = () => props.index.attempts.get(a().id);
  const session = () => sessionOf(props.index, a().session);
  const agentActor = () => {
    const s = session();
    if (s) return { agent: s.agent, session: s.id, parent: s.parent };
    const by = a().submitted_by ?? a().started_by;
    return by.type === "agent" ? { agent: by.agent, session: by.session, parent: by.parent } : null;
  };
  const checks = () => {
    const b = boardAttempt();
    if (b) return b.checks_passed;
    const runs = a().runs;
    return runs.length ? runs.every((r) => r.status === "success") : null;
  };
  return (
    <li>
      <A href={hrefs.attempt(a().id)} class="card awaiting-card" data-nav-item data-testid="awaiting-card">
        <div class="awaiting-top">
          <span class="awaiting-title">{task()?.title ?? `Task ${shortId(a().task)}`}</span>
          <span class="awaiting-age" title="Time since the last update (submission)">
            <RelativeTime iso={a().submitted_at ?? a().updated_at} />
          </span>
        </div>
        <div class="awaiting-meta">
          <code class="id-badge">{shortId(a().id)}</code>
          <code class="branch" title={`${a().branch} → ${a().base}`}>
            {a().branch}
          </code>
          <span class="muted arrow" aria-label="into">
            →
          </span>
          <code class="branch base">{a().base}</code>
        </div>
        <Show when={a().summary}>
          <p class="awaiting-summary">{a().summary}</p>
        </Show>
        <div class="awaiting-foot">
          <Show when={agentActor()} fallback={<ActorChip actor={a().submitted_by ?? a().started_by} />}>
            {(x) => <AgentChip agent={x().agent} session={x().session} parent={x().parent} />}
          </Show>
          <ChecksBadge passed={checks()} runs={a().runs} />
          <DiffStatView diff={a().diff} files />
          <PendingReviewBadge attempt={a().id} />
        </div>
      </A>
    </li>
  );
}

export function ReportCard(props: { report: ReportView; index: BoardIndex }): JSX.Element {
  const r = () => props.report.report;
  const [open, setOpen] = createSignal(r().level === "blocker");
  const long = createMemo(() => (r().body?.length ?? 0) > 220);
  const target = () => {
    if (r().attempt) return { href: hrefs.attempt(r().attempt!), label: `attempt ${shortId(r().attempt)}` };
    if (r().task) {
      const t = props.index.tasks.get(r().task!);
      return { href: hrefs.task(r().task!), label: t ? t.title : `task ${shortId(r().task)}` };
    }
    return null;
  };
  return (
    <li class={`report level-${r().level}`} data-nav-item tabindex="-1">
      <div class="report-head">
        <StatePill kind="level" value={r().level} compact />
        <span class="report-title">{r().title}</span>
        <span class="report-when">
          <RelativeTime iso={props.report.at} />
        </span>
      </div>
      <Show when={r().body}>
        <p class={`report-body${long() && !open() ? " clamped" : ""}`}>{r().body}</p>
        <Show when={long()}>
          <button type="button" class="link-btn" onClick={() => setOpen(!open())} aria-expanded={open()}>
            {open() ? "Show less" : "Show more"}
          </button>
        </Show>
      </Show>
      <div class="report-foot">
        <ActorChip actor={props.report.by} />
        <Show when={target()}>
          {(t) => (
            <A href={t().href} class="report-link">
              {t().label} →
            </A>
          )}
        </Show>
      </div>
    </li>
  );
}

function InboxZeroArt(): JSX.Element {
  return (
    <svg viewBox="0 0 64 64" width="56" height="56">
      <circle cx="32" cy="32" r="28" fill="var(--good-bg)" />
      <path d="M20 33l8 8 16-17" fill="none" stroke="var(--good-solid)" stroke-width="4.5" stroke-linecap="round" stroke-linejoin="round" />
    </svg>
  );
}
