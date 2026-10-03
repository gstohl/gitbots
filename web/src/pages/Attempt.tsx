import { A, useParams, useSearchParams } from "@solidjs/router";
import { createEffect, createMemo, createResource, createSignal, For, onCleanup, Show, type JSX } from "solid-js";
import { api } from "../api/client";
import { createLive, invalidateAll, onTipChange } from "../api/live";
import type { ActionRun, AttemptDetail, CommitInfo, HandoffTarget, JobResult, Session } from "../api/types";
import { ActorChip, AgentChip } from "../components/AgentChip";
import { ChecksBadge, DiffStatView, Labels, Sha } from "../components/Bits";
import { DiffView } from "../components/DiffView";
import { EventRow } from "../components/EventRow";
import { LogViewer } from "../components/LogViewer";
import { PendingReviewBadge } from "../components/PendingBadge";
import { reconcileAttempts } from "../api/pending";
import { RelativeTime } from "../components/RelativeTime";
import { ReviewPanel } from "../components/ReviewPanel";
import { StatePill } from "../components/StatePill";
import { EmptyState, ErrorState, Skeleton, Spinner } from "../components/States";
import { hrefs } from "../lib/events";
import { formatDuration, handoffLabel, shortId, shortSha } from "../lib/format";
import { useKey } from "../lib/keyboard";
import { useData } from "../state";

const TABS = ["diff", "commits", "checks", "timeline"] as const;
type Tab = (typeof TABS)[number];
const TAB_LABEL: Record<Tab, string> = { diff: "Diff", commits: "Commits", checks: "Checks", timeline: "Timeline" };
const DECIDED = { accept: "Accepted", reject: "Rejected", changes_requested: "Changes requested" } as const;

export default function AttemptPage(): JSX.Element {
  const params = useParams();
  const [search, setSearch] = useSearchParams();
  const { project, index } = useData();
  const detail = createLive(() => params.id, (id) => api.attempt(id));
  // Hosted: clear "decision pending sync" once the attempt left `submitted`.
  createEffect(() => {
    const d = detail.data();
    if (d) reconcileAttempts([d.attempt]);
  });

  const tab = (): Tab => {
    const t = search.tab;
    return typeof t === "string" && (TABS as readonly string[]).includes(t) ? (t as Tab) : "diff";
  };
  const setTab = (t: Tab) => setSearch({ tab: t === "diff" ? undefined : t }, { replace: true });

  TABS.forEach((t, i) => useKey(String(i + 1), () => setTab(t)));
  useKey("r", (e) => {
    const el = document.querySelector<HTMLInputElement>("[data-review-first]");
    if (el && !el.disabled) {
      e.preventDefault();
      el.focus();
    } else document.getElementById("review-panel")?.scrollIntoView({ block: "nearest" });
  });

  return (
    <div class="page attempt-page">
      <Show when={detail.error() && !detail.data()}>
        <ErrorState error={detail.error()} retry={detail.refetch} />
      </Show>
      <Show when={!detail.data() && !detail.error()}>
        <Skeleton rows={8} />
      </Show>
      <Show when={detail.data()}>
        {(d) => (
          <>
            <AttemptHeader d={d()} />
            <div class="attempt-layout">
              <div class="attempt-main">
                <div class="tabs" role="tablist" aria-label="Attempt">
                  <For each={TABS}>
                    {(t, i) => (
                      <button
                        type="button"
                        role="tab"
                        id={`tab-${t}`}
                        class="tab"
                        aria-selected={tab() === t}
                        aria-controls={`panel-${t}`}
                        tabindex={tab() === t ? 0 : -1}
                        onClick={() => setTab(t)}
                        onKeyDown={(e) => {
                          if (e.key === "ArrowRight" || e.key === "ArrowLeft") {
                            e.preventDefault();
                            const next = TABS[(i() + (e.key === "ArrowRight" ? 1 : TABS.length - 1)) % TABS.length]!;
                            setTab(next);
                            document.getElementById(`tab-${next}`)?.focus();
                          }
                        }}
                        title={`${TAB_LABEL[t]} (${i() + 1})`}
                      >
                        {TAB_LABEL[t]}
                        <span class="tab-count">
                          {t === "commits"
                            ? d().commits.length
                            : t === "checks"
                              ? d().runs.length
                              : t === "timeline"
                                ? d().events.length
                                : d().attempt.diff?.files ?? ""}
                        </span>
                      </button>
                    )}
                  </For>
                </div>
                <div class="tab-panel" role="tabpanel" id={`panel-${tab()}`} aria-labelledby={`tab-${tab()}`}>
                  <Show when={tab() === "diff"}>
                    <DiffTab id={d().attempt.id} head={d().attempt.head} />
                  </Show>
                  <Show when={tab() === "commits"}>
                    <CommitsTab commits={d().commits} />
                  </Show>
                  <Show when={tab() === "checks"}>
                    <ChecksTab runs={d().runs} />
                  </Show>
                  <Show when={tab() === "timeline"}>
                    <Show when={d().events.length} fallback={<EmptyState title="No events yet" />}>
                      <ol class="event-list" role="list">
                        <For each={d().events}>
                          {(e) => <EventRow event={e} ctx={{ taskTitle: (id) => index().tasks.get(id)?.title }} />}
                        </For>
                      </ol>
                    </Show>
                  </Show>
                </div>
              </div>
              <aside class="attempt-side">
                <ReviewPanel
                  attempt={d().attempt}
                  project={project.data()}
                  onDecided={() => {
                    invalidateAll();
                  }}
                />
              </aside>
            </div>
          </>
        )}
      </Show>
    </div>
  );
}

function Lineage(props: { chain: Session[] }): JSX.Element {
  return (
    <span class="lineage" aria-label="Session lineage, subagent first">
      <For each={props.chain}>
        {(s, i) => (
          <>
            <Show when={i() > 0}>
              <span class="lineage-sep" title="spawned by">
                ←
              </span>
            </Show>
            <span class="lineage-item">
              <AgentChip agent={s.agent} session={s.id} parent={s.parent} />
              <Show when={s.role}>
                <span class="muted small">{s.role}</span>
              </Show>
            </span>
          </>
        )}
      </For>
    </span>
  );
}

/** Who holds the attempt after a handoff: a session (as an agent chip when known), a role or a human. */
function Holder(props: { target: HandoffTarget; chain: Session[] }): JSX.Element {
  const { index } = useData();
  const session = () => {
    const t = props.target;
    if (t.type !== "session") return undefined;
    return props.chain.find((s) => s.id === t.session) ?? index().sessions.get(t.session)?.session;
  };
  return (
    <Show when={session()} fallback={<span class="holder">{handoffLabel(props.target)}</span>}>
      {(s) => <AgentChip agent={s().agent} session={s().id} parent={s().parent} />}
    </Show>
  );
}

function AttemptHeader(props: { d: AttemptDetail }): JSX.Element {
  const a = () => props.d.attempt;
  const t = () => props.d.task;
  return (
    <header class="attempt-head">
      <nav class="crumbs-line" aria-label="Breadcrumb">
        <A href="/board">Board</A>
        <span aria-hidden="true">/</span>
        <A href={hrefs.task(t().id)}>{shortId(t().id)}</A>
        <span aria-hidden="true">/</span>
        <span>attempt {shortId(a().id)}</span>
      </nav>
      <h1 class="attempt-title">{t().title}</h1>
      <div class="attempt-line">
        <StatePill kind="attempt" value={a().state} />
        <PendingReviewBadge attempt={a().id} />
        <code class="branch" title={a().branch}>
          {a().branch}
        </code>
        <span class="muted" aria-label="into">
          →
        </span>
        <code class="branch base">{a().base}</code>
        <span class="muted small">head</span>
        <Sha sha={a().head} />
        <DiffStatView diff={a().diff} files />
        <ChecksBadge passed={a().checks_passed} runs={a().runs} />
      </div>
      <Show when={props.d.violations.length}>
        <div class="banner banner-warn" role="alert">
          <strong>Mandate violations.</strong> This attempt touches paths agents may not change:
          <ul>
            <For each={props.d.violations}>
              {(v) => (
                <li>
                  <code>{v.path}</code> <span class="muted">{v.reason}</span>
                </li>
              )}
            </For>
          </ul>
        </div>
      </Show>
      <dl class="facts">
        <div>
          <dt>Agent</dt>
          <dd>
            <Show when={props.d.session_chain.length} fallback={<ActorChip actor={a().started_by} />}>
              <Lineage chain={props.d.session_chain} />
            </Show>
          </dd>
        </div>
        <Show when={a().holder}>
          {(h) => (
            <div>
              <dt>Held by</dt>
              <dd>
                <Holder target={h()} chain={props.d.session_chain} /> <span class="muted small">after a handoff</span>
              </dd>
            </div>
          )}
        </Show>
        <div>
          <dt>Started</dt>
          <dd>
            <RelativeTime iso={a().started_at} /> by <ActorChip actor={a().started_by} />
          </dd>
        </div>
        <Show when={a().submitted_by}>
          {(s) => (
            <div>
              <dt>Submitted</dt>
              <dd>
                <RelativeTime iso={a().updated_at} /> by <ActorChip actor={s()} />
              </dd>
            </div>
          )}
        </Show>
        <Show when={a().review}>
          {(r) => (
            <div>
              <dt>Review</dt>
              <dd>
                <span class={`decision decision-${r().decision}`}>{DECIDED[r().decision]}</span> by{" "}
                <ActorChip actor={r().by} /> <RelativeTime iso={r().at} />
                <Show when={r().reason}>
                  <span class="review-reason">“{r().reason}”</span>
                </Show>
              </dd>
            </div>
          )}
        </Show>
        <Show when={a().merged_commit}>
          {(m) => (
            <div>
              <dt>Merged</dt>
              <dd>
                into <code>{a().base}</code> at <Sha sha={m()} />
              </dd>
            </div>
          )}
        </Show>
        <Show when={props.d.workroom}>
          {(w) => (
            <div>
              <dt>Workroom</dt>
              <dd>
                <code class="path" title={w()}>
                  {shortPath(w())}
                </code>
              </dd>
            </div>
          )}
        </Show>
        <div>
          <dt>Base</dt>
          <dd>
            <Sha sha={a().base_commit} />
          </dd>
        </div>
      </dl>
      <Show when={a().summary}>
        <p class="attempt-summary">{a().summary}</p>
      </Show>
      <Show when={t().body || t().labels.length}>
        <details class="task-details">
          <summary>Task</summary>
          <Labels labels={t().labels} />
          <Show when={t().body}>
            <p class="pre-wrap">{t().body}</p>
          </Show>
        </details>
      </Show>
    </header>
  );
}

function DiffTab(props: { id: string; head: string | null }): JSX.Element {
  // Keyed on id + head: a new push refetches; the tip poll refetches too.
  const [diff, { refetch }] = createResource(() => `${props.id}@${props.head ?? ""}`, () => api.attemptDiff(props.id));
  onCleanup(onTipChange(() => void refetch()));
  return (
    <>
      <Show when={diff.loading && diff.state !== "refreshing"}>
        <Spinner label="Loading diff…" />
      </Show>
      <Show when={diff.error}>
        <ErrorState error={diff.error} retry={() => void refetch()} />
      </Show>
      <Show when={diff.state === "ready" || diff.state === "refreshing"}>
        <DiffView text={diff.latest ?? ""} />
      </Show>
    </>
  );
}

/** `…/workrooms/<prj>/<slug>` -> `…/<slug>` when long; the full path is in the tooltip. */
function shortPath(p: string): string {
  if (p.length <= 48) return p;
  const last = p.replace(/\/+$/, "").split("/").pop() ?? p;
  return `…/${last}`;
}

function trailerClass(k: string): string {
  return /^Gitbots-(Session|Model|Client|Provider|Parent-Session)$/i.test(k) ? "trailer gitbots" : "trailer";
}

function CommitsTab(props: { commits: CommitInfo[] }): JSX.Element {
  return (
    <Show when={props.commits.length} fallback={<EmptyState title="No commits on this attempt yet" />}>
      <ol class="commit-list" role="list">
        <For each={props.commits}>
          {(c) => (
            <li class="commit" data-nav-item tabindex="-1">
              <div class="commit-head">
                <Sha sha={c.sha} />
                <span class="commit-subject">{c.subject}</span>
              </div>
              <Show when={c.trailers.length}>
                <div class="trailers">
                  <For each={c.trailers}>
                    {([k, v]) => (
                      <span class={trailerClass(k)} title={`${k}: ${v}`}>
                        <span class="trailer-k">{k}</span>
                        <span class="trailer-v">{/^Gitbots-(Parent-)?Session$/i.test(k) ? shortId(v) : v}</span>
                      </span>
                    )}
                  </For>
                </div>
              </Show>
            </li>
          )}
        </For>
      </ol>
    </Show>
  );
}

function ChecksTab(props: { runs: ActionRun[] }): JSX.Element {
  const [open, setOpen] = createSignal<{ job: JobResult; run: ActionRun } | null>(null);
  const runs = createMemo(() => [...props.runs].reverse()); // newest first
  return (
    <>
      <Show when={props.runs.length} fallback={<EmptyState title="No checks have run">Workflows triggered by <code>attempt.submitted</code> show up here.</EmptyState>}>
        <ol class="run-list" role="list">
          <For each={runs()}>
            {(r, i) => (
              <li class="run">
                <div class="run-head">
                  <StatePill kind="run" value={r.status} />
                  <strong class="run-wf">{r.workflow}</strong>
                  <span class="muted small">
                    on {r.trigger} · {r.runner} · {formatDuration(r.duration_ms)}
                  </span>
                  <Show when={r.commit}>
                    <span class="muted small">
                      at <code>{shortSha(r.commit)}</code>
                    </span>
                  </Show>
                  <Show when={i() > 0}>
                    <span class="muted small">(earlier run)</span>
                  </Show>
                </div>
                <table class="jobs">
                  <thead class="sr-only">
                    <tr>
                      <th>Job</th>
                      <th>Status</th>
                      <th>Duration</th>
                      <th>Log</th>
                    </tr>
                  </thead>
                  <tbody>
                    <For each={r.jobs}>
                      {(j) => (
                        <tr class={`job job-${j.status}`}>
                          <td class="job-name">
                            <Show when={j.log} fallback={<span>{j.name}</span>}>
                              <button type="button" class="link-btn" onClick={() => setOpen({ job: j, run: r })} data-nav-item>
                                {j.name}
                              </button>
                            </Show>
                            <Show when={j.failed_step}>
                              <span class="muted small"> step {j.failed_step}</span>
                            </Show>
                            <Show when={j.exit_code !== undefined && j.exit_code !== 0}>
                              <span class="muted small"> exit {j.exit_code}</span>
                            </Show>
                          </td>
                          <td>
                            <StatePill kind="run" value={j.status} compact />
                          </td>
                          <td class="num muted">{j.status === "skipped" ? "—" : formatDuration(j.duration_ms)}</td>
                          <td class="num">
                            <Show when={j.log} fallback={<span class="muted small">no log</span>}>
                              <button type="button" class="btn btn-sm btn-ghost" onClick={() => setOpen({ job: j, run: r })}>
                                View log
                              </button>
                            </Show>
                          </td>
                        </tr>
                      )}
                    </For>
                  </tbody>
                </table>
              </li>
            )}
          </For>
        </ol>
      </Show>
      <Show when={open()}>
        {(o) => (
          <LogViewer
            path={o().job.log!.path}
            title={`${o().run.workflow} / ${o().job.name}`}
            onClose={() => setOpen(null)}
          />
        )}
      </Show>
    </>
  );
}
