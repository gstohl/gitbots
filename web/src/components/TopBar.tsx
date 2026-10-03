import { A } from "@solidjs/router";
import { For, lazy, Show, type JSX } from "solid-js";
import { useData } from "../state";
import { HumanChip } from "./AgentChip";
import { LiveIndicator } from "./LiveIndicator";
import { ThemeToggle } from "./ThemeToggle";

// The mock scenario picker (and the fixtures it pulls in) only exist in mock
// builds: `import.meta.env.VITE_GITBOTS_MOCK` is replaced at build time, so the
// dynamic import is dropped from production bundles.
const MockSwitcher =
  import.meta.env.VITE_GITBOTS_MOCK === "1"
    ? lazy(() => import("./MockSwitcher").then((m) => ({ default: m.MockSwitcher })))
    : null;

const NAV = [
  { href: "/", label: "Inbox", key: "i", end: true },
  { href: "/board", label: "Board", key: "b" },
  { href: "/activity", label: "Activity", key: "a" },
  { href: "/agents", label: "Agents", key: "g" },
  { href: "/sessions", label: "Sessions", key: "s" },
  { href: "/mandate", label: "Mandate", key: "m" },
];

export function TopBar(props: { onHelp: () => void }): JSX.Element {
  const { project, inbox } = useData();
  const m = () => project.data()?.manifest;
  const awaiting = () => inbox.data()?.awaiting_review.length ?? 0;
  const blockers = () => inbox.data()?.reports.filter((r) => r.report.level === "blocker").length ?? 0;
  const crumbs = () => {
    const t = m()?.tenancy;
    if (!t) return [] as string[];
    return [t.owner.handle, t.team, t.workspace].filter((x): x is string => !!x);
  };
  return (
    <header class="topbar">
      <div class="topbar-inner">
        <div class="topbar-left">
          <A href="/" class="brand" aria-label="gitbots inbox">
            <svg viewBox="0 0 32 32" width="22" height="22" aria-hidden="true">
              <rect width="32" height="32" rx="7" fill="var(--accent)" />
              <path d="M10 22V10m0 6h7a5 5 0 0 0 5-5m-12 5a5 5 0 0 1 5 5h2" stroke="var(--surface)" stroke-width="2.6" fill="none" stroke-linecap="round" />
              <circle cx="22" cy="10" r="2.6" fill="var(--surface)" />
              <circle cx="19" cy="22" r="2.6" fill="var(--surface)" />
            </svg>
          </A>
          <nav class="crumbs" aria-label="Project">
            <For each={crumbs()}>
              {(c) => (
                <>
                  <span class="crumb muted">{c}</span>
                  <span class="crumb-sep" aria-hidden="true">
                    /
                  </span>
                </>
              )}
            </For>
            <span class="crumb project-name" title={m()?.project.description ?? m()?.project.id}>
              {m()?.project.name ?? "gitbots"}
            </span>
            <Show when={project.data()?.hosted}>
              <span class="hosted-chip" title="Served by the hosted gitbots Worker. Decisions queue in an outbox until the next `gitbots sync`.">
                hosted
              </span>
            </Show>
          </nav>
        </div>
        <nav class="mainnav" aria-label="Main">
          <For each={NAV}>
            {(n) => (
              <A href={n.href} end={n.end ?? false} class="navlink" activeClass="active" title={`g ${n.key}`}>
                {n.label}
                <Show when={n.href === "/" && awaiting() + blockers() > 0}>
                  <span class={`navcount${blockers() ? " has-blocker" : ""}`} aria-label={`${awaiting()} awaiting, ${blockers()} blockers`}>
                    {awaiting() + blockers()}
                  </span>
                </Show>
              </A>
            )}
          </For>
        </nav>
        <div class="topbar-right">
          {MockSwitcher && <MockSwitcher />}
          <Show when={(project.data()?.pending_outbox ?? 0) > 0}>
            <span
              class="pending-badge outbox-badge"
              title="Decisions queued in the hosted outbox. They're applied to the ledger on the next `gitbots sync`."
              role="status"
            >
              <span aria-hidden="true">⧗</span> {project.data()?.pending_outbox} pending sync
            </span>
          </Show>
          <LiveIndicator />
          <button type="button" class="icon-btn" onClick={() => props.onHelp()} aria-label="Keyboard shortcuts" title="Keyboard shortcuts (?)">
            ?
          </button>
          <ThemeToggle />
          <Show when={project.data()}>
            {(p) => (
              <span class="viewer" title={p().can_decide ? "You act as this human in the UI" : "Read-only: gitbots ui was started by an agent"}>
                <Show when={p().viewer?.type === "human" && p().viewer} fallback={<span class="chip muted-chip">no viewer</span>}>
                  {(v) => {
                    const h = v() as { type: "human"; handle: string; email?: string };
                    return <HumanChip handle={h.handle} email={h.email} />;
                  }}
                </Show>
                <Show when={!p().can_decide}>
                  <span class="readonly-badge">read-only</span>
                </Show>
              </span>
            )}
          </Show>
        </div>
      </div>
    </header>
  );
}
