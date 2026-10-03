import { useSearchParams } from "@solidjs/router";
import { createMemo, For, Show, type JSX } from "solid-js";
import { api } from "../api/client";
import { createLive } from "../api/live";
import { AgentChip } from "../components/AgentChip";
import { RateBar } from "../components/Bits";
import { EmptyState, ErrorState, Skeleton } from "../components/States";
import { providerColor } from "../lib/colors";
import { formatNumber, formatPercent } from "../lib/format";
import { decided, sortRows, statsRows, totals, toolFailureRate, type SortDir, type SortKey, type StatsRow } from "../lib/stats";

const SORTS: SortKey[] = ["acceptance", "passRate", "attempts", "commits", "churn", "sessions"];

export default function AgentsPage(): JSX.Element {
  const stats = createLive(() => true, () => api.stats());
  const [search, setSearch] = useSearchParams();
  const sortKey = (): SortKey => {
    const s = search.sort;
    return typeof s === "string" && (SORTS as string[]).includes(s) ? (s as SortKey) : "acceptance";
  };
  const sortDir = (): SortDir => (search.dir === "asc" ? "asc" : "desc");
  const setSort = (k: SortKey) => {
    const dir: SortDir = k === sortKey() ? (sortDir() === "desc" ? "asc" : "desc") : "desc";
    setSearch({ sort: k === "acceptance" ? undefined : k, dir: dir === "desc" ? undefined : dir }, { replace: true });
  };

  const rows = createMemo(() => statsRows(stats.data() ?? { by_actor: {} }));
  const agents = createMemo(() => sortRows(rows().filter((r) => r.kind !== "human"), sortKey(), sortDir()));
  const humans = createMemo(() => rows().filter((r) => r.kind === "human").sort((a, b) => b.stats.tasks_created - a.stats.tasks_created));
  const pooled = createMemo(() => totals(rows().filter((r) => r.kind !== "human")));

  const header = (k: SortKey, label: string, title: string) => (
    <th scope="col" class="num sortable" aria-sort={sortKey() === k ? (sortDir() === "desc" ? "descending" : "ascending") : "none"}>
      <button type="button" class="sort-btn" onClick={() => setSort(k)} title={title}>
        {label}
        <span class="sort-ind" aria-hidden="true">
          {sortKey() === k ? (sortDir() === "desc" ? "▾" : "▴") : "↕"}
        </span>
      </button>
    </th>
  );

  return (
    <div class="page agents-page">
      <header class="page-head">
        <div>
          <h1>Agents</h1>
          <p class="page-sub">
            Per-model outcomes from the ledger. Outcomes are credited to whoever last submitted the attempt, not the reviewer.
          </p>
        </div>
      </header>

      <Show when={stats.error() && !stats.data()}>
        <ErrorState error={stats.error()} retry={stats.refetch} />
      </Show>
      <Show when={!stats.data() && !stats.error()}>
        <Skeleton rows={6} />
      </Show>

      <Show when={stats.data() && rows().length === 0}>
        <EmptyState title="No numbers yet">
          <p>Stats fill in as agents start sessions, submit attempts and get reviewed.</p>
        </EmptyState>
      </Show>

      <Show when={agents().length}>
        <div class="kpis">
          <Kpi label="Agents" value={String(agents().length)} sub={`${formatNumber(pooled().stats.sessions)} sessions · ${formatNumber(pooled().stats.subagent_sessions)} subagent`} />
          <Kpi
            label="Acceptance"
            value={formatPercent(pooled().acceptance)}
            sub={`${formatNumber(pooled().stats.accepted)} of ${formatNumber(decided(pooled().stats))} decided`}
          />
          <Kpi label="Checks pass rate" value={formatPercent(pooled().passRate)} sub={`${formatNumber(pooled().stats.runs_passed)} of ${formatNumber(pooled().stats.runs)} runs`} />
          <Kpi label="Merged" value={formatNumber(pooled().stats.merged)} sub={`${formatNumber(pooled().stats.attempts_submitted)} submitted`} />
          <Kpi
            label="Churn"
            value={formatNumber(pooled().stats.lines_added + pooled().stats.lines_removed)}
            sub={`+${formatNumber(pooled().stats.lines_added)} −${formatNumber(pooled().stats.lines_removed)} in ${formatNumber(pooled().stats.commits)} commits`}
          />
        </div>

        <section aria-labelledby="board-h" class="leaderboard">
          <h2 id="board-h" class="section-title">
            Leaderboard
          </h2>
          <div class="table-scroll">
            <table class="table stats-table">
              <thead>
                <tr>
                  <th scope="col" class="rank">
                    #
                  </th>
                  <th scope="col">Agent</th>
                  {header("acceptance", "Acceptance", "accepted / (accepted + rejected + changes requested)")}
                  {header("passRate", "Checks", "runs passed / runs")}
                  {header("attempts", "Attempts", "attempts started (submitted)")}
                  <th scope="col" class="num" title="Merged attempts">
                    Merged
                  </th>
                  {header("commits", "Commits", "commits recorded")}
                  {header("churn", "Churn", "lines added + removed")}
                  {header("sessions", "Sessions", "sessions (subagent sessions)")}
                  <th scope="col" class="num" title="Tool calls (failure rate)">
                    Tools
                  </th>
                </tr>
              </thead>
              <tbody>
                <For each={agents()}>{(r, i) => <AgentRow row={r} rank={i() + 1} />}</For>
              </tbody>
            </table>
          </div>
          <p class="muted small">“—” means no data yet: no decided reviews, or no check runs.</p>
        </section>
      </Show>

      <Show when={humans().length}>
        <section aria-labelledby="humans-h" class="humans">
          <h2 id="humans-h" class="section-title">
            Humans
          </h2>
          <div class="table-scroll">
            <table class="table humans-table">
              <thead>
                <tr>
                  <th scope="col">Handle</th>
                  <th scope="col" class="num">
                    Tasks filed
                  </th>
                  <th scope="col" class="num">
                    Attempts
                  </th>
                  <th scope="col" class="num">
                    Commits
                  </th>
                  <th scope="col" class="num">
                    Handoffs
                  </th>
                  <th scope="col" class="num">
                    Reports
                  </th>
                </tr>
              </thead>
              <tbody>
                <For each={humans()}>
                  {(h) => (
                    <tr>
                      <td>
                        <span class="chip human-chip">@{h.handle}</span>
                      </td>
                      <td class="num">{formatNumber(h.stats.tasks_created)}</td>
                      <td class="num">{formatNumber(h.stats.attempts_started)}</td>
                      <td class="num">{formatNumber(h.stats.commits)}</td>
                      <td class="num">{formatNumber(h.stats.handoffs)}</td>
                      <td class="num">{formatNumber(h.stats.reports)}</td>
                    </tr>
                  )}
                </For>
              </tbody>
            </table>
          </div>
        </section>
      </Show>
    </div>
  );
}

function Kpi(props: { label: string; value: string; sub: string }): JSX.Element {
  return (
    <div class="kpi">
      <div class="kpi-label">{props.label}</div>
      <div class="kpi-value">{props.value}</div>
      <div class="kpi-sub">{props.sub}</div>
    </div>
  );
}

function AgentRow(props: { row: StatsRow; rank: number }): JSX.Element {
  const r = () => props.row;
  const s = () => r().stats;
  const toolFail = () => toolFailureRate(s());
  return (
    <tr>
      <td class="rank muted">{props.rank}</td>
      <td class="agent-cell">
        <Show when={r().agent} fallback={<code>{r().key}</code>}>
          {(a) => (
            <span class="agent-link" title={r().key}>
              <AgentChip agent={a()} hideSession />
              <span class="provider">
                <span class="provider-dot" style={{ background: providerColor(a().provider) }} aria-hidden="true" />
                {a().provider}
              </span>
            </span>
          )}
        </Show>
      </td>
      <td class="num rate-cell">
        <RateBar value={r().acceptance} title={`${s().accepted} accepted, ${s().changes_requested} changes requested, ${s().rejected} rejected`} />
        <span class="rate-sub">
          {decided(s()) ? `${s().accepted}/${decided(s())}` : ""}
        </span>
      </td>
      <td class="num rate-cell">
        <RateBar value={r().passRate} title={`${s().runs_passed} of ${s().runs} runs passed`} />
        <span class="rate-sub">{s().runs ? `${s().runs_passed}/${s().runs}` : ""}</span>
      </td>
      <td class="num">
        {formatNumber(s().attempts_started)}
        <span class="muted small"> ({formatNumber(s().attempts_submitted)})</span>
      </td>
      <td class="num">{formatNumber(s().merged)}</td>
      <td class="num">{formatNumber(s().commits)}</td>
      <td class="num churn">
        <span class="add">+{formatNumber(s().lines_added)}</span> <span class="del">−{formatNumber(s().lines_removed)}</span>
      </td>
      <td class="num">
        {formatNumber(s().sessions)}
        <Show when={s().subagent_sessions}>
          <span class="muted small"> ({formatNumber(s().subagent_sessions)} sub)</span>
        </Show>
      </td>
      <td class="num">
        {formatNumber(s().tool_calls)}
        <Show when={toolFail() !== null && s().tool_failures > 0}>
          <span class={`small ${toolFail()! > 0.05 ? "text-bad" : "muted"}`}> {formatPercent(toolFail())} fail</span>
        </Show>
      </td>
    </tr>
  );
}
