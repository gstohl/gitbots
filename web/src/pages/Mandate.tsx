import { createResource, For, Show, type JSX } from "solid-js";
import { api } from "../api/client";
import type { Approver, Manifest } from "../api/types";
import { Sha } from "../components/Bits";
import { ErrorState, Skeleton } from "../components/States";
import { shortSha } from "../lib/format";
import { useData } from "../state";

const DECISIONS: { key: keyof Manifest["mandate"]["approvals"]; label: string }[] = [
  { key: "accept_attempt", label: "Accept an attempt" },
  { key: "merge_protected", label: "Merge into a protected branch" },
  { key: "change_mandate", label: "Change the mandate" },
  { key: "run_hosted_action", label: "Run a hosted action" },
];
const WHO: { key: "agent" | "reviewer" | "maintainer" | "owner"; label: string }[] = [
  { key: "agent", label: "Agents" },
  { key: "reviewer", label: "Reviewer" },
  { key: "maintainer", label: "Maintainer" },
  { key: "owner", label: "Owner" },
];
const RANK = { agent: 0, reviewer: 1, maintainer: 2, owner: 3 } as const;
const NEED: Record<Approver, number> = { any: 0, reviewer: 1, maintainer: 2, owner: 3 };

const AUTONOMY: Record<Manifest["mandate"]["autonomy"], string> = {
  supervised: "Humans decide everything that matters; agents propose.",
  assisted: "Agents work and may decide routine things; humans gate merges and the mandate.",
  autonomous: "Agents decide and merge within the mandate; humans set direction.",
};

export default function MandatePage(): JSX.Element {
  const { project } = useData();
  const [workflows] = createResource(() => api.workflows());
  const [recipes] = createResource(() => api.recipes());

  return (
    <div class="page mandate-page">
      <Show when={project.error() && !project.data()}>
        <ErrorState error={project.error()} retry={project.refetch} />
      </Show>
      <Show when={!project.data() && !project.error()}>
        <Skeleton rows={8} />
      </Show>
      <Show when={project.data()}>
        {(p) => {
          const m = () => p().manifest;
          const md = () => m().mandate;
          return (
            <>
              <header class="page-head">
                <div>
                  <h1>Mandate</h1>
                  <p class="page-sub">
                    What agents may do in <strong>{m().project.name}</strong>, and which decisions stay with humans.
                  </p>
                </div>
                <div class="page-actions">
                  <Show
                    when={p().manifest_source.type === "trusted" && p().manifest_source}
                    fallback={
                      <span class="source-badge warn" role="status">
                        ⚠ Not committed to the trusted branch ({p().trusted_branch}) yet: read from the working tree
                      </span>
                    }
                  >
                    {(src) => {
                      const s = src() as { type: "trusted"; branch: string; oid: string };
                      return (
                        <span class="source-badge ok" title={`.gitbots/manifest.json at ${s.branch} (blob ${s.oid})`}>
                          ✓ trusted · {s.branch} @ <code>{shortSha(s.oid)}</code>
                        </span>
                      );
                    }}
                  </Show>
                </div>
              </header>

              <p class="notice notice-muted">
                Read-only. The mandate lives in <code>.gitbots/manifest.json</code> and is read from <code>{p().trusted_branch}</code>,
                never from a workroom, so an agent can't grant itself permissions. Change it with a commit to{" "}
                <code>{p().trusted_branch}</code> (approval: {md().approvals.change_mandate}).
              </p>

              <div class="mandate-grid">
                <section class="card">
                  <h2 class="section-title">Goal</h2>
                  <Show when={md().goal} fallback={<p class="muted">No goal set.</p>}>
                    <blockquote class="goal">{md().goal}</blockquote>
                  </Show>
                  <div class="autonomy">
                    <span class={`autonomy-badge a-${md().autonomy}`}>{md().autonomy}</span>
                    <span class="muted">{AUTONOMY[md().autonomy]}</span>
                  </div>
                  <p class="muted small">Autonomy only picks the default approvals at init; the matrix below is what's enforced.</p>
                </section>

                <section class="card">
                  <h2 class="section-title">Principals</h2>
                  <table class="table compact">
                    <thead>
                      <tr>
                        <th scope="col">Handle</th>
                        <th scope="col">Email</th>
                        <th scope="col">Role</th>
                      </tr>
                    </thead>
                    <tbody>
                      <For each={md().principals}>
                        {(pr) => (
                          <tr>
                            <td>
                              <span class="chip human-chip">@{pr.handle}</span>
                            </td>
                            <td class="muted">{pr.email ?? "—"}</td>
                            <td>
                              <span class={`role-badge r-${pr.role}`}>{pr.role}</span>
                            </td>
                          </tr>
                        )}
                      </For>
                    </tbody>
                  </table>
                  <p class="muted small">reviewer &lt; maintainer &lt; owner. Agents in an attempt's session family never approve it.</p>
                </section>

                <section class="card span-2">
                  <h2 class="section-title">Approvals</h2>
                  <div class="table-scroll">
                    <table class="table matrix">
                      <thead>
                        <tr>
                          <th scope="col">Decision</th>
                          <For each={WHO}>{(w) => <th scope="col" class="center">{w.label}</th>}</For>
                          <th scope="col">Rule</th>
                        </tr>
                      </thead>
                      <tbody>
                        <For each={DECISIONS}>
                          {(d) => {
                            const need = () => md().approvals[d.key];
                            return (
                              <tr>
                                <th scope="row">{d.label}</th>
                                <For each={WHO}>
                                  {(w) => {
                                    const ok = () => RANK[w.key] >= NEED[need()];
                                    return (
                                      <td class={`center cell-${ok() ? "yes" : "no"}`} aria-label={ok() ? "allowed" : "not allowed"}>
                                        {ok() ? "✓" : "—"}
                                      </td>
                                    );
                                  }}
                                </For>
                                <td>
                                  <code>{need()}</code>
                                </td>
                              </tr>
                            );
                          }}
                        </For>
                      </tbody>
                    </table>
                  </div>
                </section>

                <section class="card">
                  <h2 class="section-title">Paths agents may touch</h2>
                  <h3 class="sub-title">Allowed</h3>
                  <div class="globs">
                    <For each={md().agents.allowed_paths} fallback={<span class="muted">none</span>}>
                      {(g) => <code class="glob allow">{g}</code>}
                    </For>
                  </div>
                  <h3 class="sub-title">Denied</h3>
                  <div class="globs">
                    <For each={md().agents.denied_paths} fallback={<span class="muted">none</span>}>
                      {(g) => <code class="glob deny">{g}</code>}
                    </For>
                  </div>
                  <p class="muted small">Matching is case-insensitive. A submit that touches a denied path is blocked.</p>
                </section>

                <section class="card">
                  <h2 class="section-title">Protected branches</h2>
                  <div class="globs">
                    <For each={md().agents.protected_branches} fallback={<span class="muted">none</span>}>
                      {(g) => <code class="glob protect">⛨ {g}</code>}
                    </For>
                  </div>
                  <p class="muted small">
                    Merging into these needs <code>{md().approvals.merge_protected}</code>. Mirror them in your git host's branch
                    protection; local enforcement is advisory.
                  </p>
                  <h3 class="sub-title">Ledger</h3>
                  <dl class="kv">
                    <dt>activity</dt>
                    <dd>
                      <code>{m().ledger.activity_branch}</code>
                    </dd>
                    <dt>logs</dt>
                    <dd>
                      <code>{m().ledger.logs_branch}</code>
                    </dd>
                    <dt>workrooms</dt>
                    <dd>
                      <code>{m().workrooms.branch_prefix}/…</code>
                    </dd>
                    <dt>project</dt>
                    <dd>
                      <code class="small">{m().project.id}</code>
                    </dd>
                    <dt>tenancy</dt>
                    <dd>
                      {m().tenancy.owner.kind} <strong>{m().tenancy.owner.handle}</strong>
                      {m().tenancy.team ? ` / ${m().tenancy.team}` : ""}
                      {m().tenancy.workspace ? ` / ${m().tenancy.workspace}` : ""}
                    </dd>
                    <dt>server</dt>
                    <dd>
                      <code>{p().producer}</code>
                    </dd>
                  </dl>
                </section>

                <section class="card span-2">
                  <h2 class="section-title">Trusted workflows and recipes</h2>
                  <p class="muted small">Also read from {p().trusted_branch}, so an attempt can't change the checks that judge it.</p>
                  <Show when={workflows.state === "ready" && workflows()}>
                    {(w) => (
                      <ul class="plain-list">
                        <For each={w().workflows}>
                          {(x) => (
                            <li>
                              <code>{x.path}</code>{" "}
                              <span class="muted small">{workflowSummary(x.workflow)}</span>
                            </li>
                          )}
                        </For>
                        <For each={w().invalid}>
                          {(x) => (
                            <li class="text-bad">
                              <code>{x.path}</code> <span class="small">invalid: {x.error}</span>
                            </li>
                          )}
                        </For>
                        <Show when={!w().workflows.length && !w().invalid.length}>
                          <li class="muted">No workflows in .gitbots/actions/.</li>
                        </Show>
                      </ul>
                    )}
                  </Show>
                  <Show when={workflows.error}>
                    <p class="muted small">Couldn't load workflows: {String((workflows.error as Error)?.message ?? workflows.error)}</p>
                  </Show>
                  <Show when={recipes.state === "ready" && recipes()}>
                    {(r) => (
                      <ul class="plain-list">
                        <For each={r()}>
                          {(x) => (
                            <li class={"error" in x ? "text-bad" : ""}>
                              <code>{x.path}</code>{" "}
                              <span class="small">{"error" in x ? `invalid: ${x.error}` : "recipe"}</span>
                            </li>
                          )}
                        </For>
                      </ul>
                    )}
                  </Show>
                  <Show when={p().manifest_source.type === "trusted"}>
                    <p class="muted small">
                      Manifest blob <Sha sha={(p().manifest_source as { oid: string }).oid} />
                    </p>
                  </Show>
                </section>
              </div>
            </>
          );
        }}
      </Show>
    </div>
  );
}

function workflowSummary(w: unknown): string {
  if (typeof w !== "object" || w === null) return "";
  const o = w as { name?: unknown; on?: unknown; jobs?: unknown };
  const parts: string[] = [];
  if (typeof o.name === "string") parts.push(o.name);
  if (Array.isArray(o.on)) parts.push(`on ${o.on.join(", ")}`);
  if (o.jobs && typeof o.jobs === "object") parts.push(`${Object.keys(o.jobs).length} jobs`);
  return parts.join(" · ");
}
