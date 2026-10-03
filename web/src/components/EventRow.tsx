import { A } from "@solidjs/router";
import { createMemo, For, Show, type JSX } from "solid-js";
import type { Event, Via } from "../api/types";
import { describeEvent, kindGroup, type DescribeContext, type EventPart } from "../lib/events";
import { ActorChip } from "./AgentChip";
import { RelativeTime } from "./RelativeTime";

/** How far to trust the attribution, by how the actor was resolved. */
export const VIA_HINT: Record<Via, { label: string; title: string; trust: "high" | "mid" | "low" }> = {
  mcp: { label: "mcp", title: "Resolved from the MCP connection (client info)", trust: "mid" },
  worktree: { label: "worktree", title: "Session bound to the attempt's workroom", trust: "high" },
  env: { label: "env", title: "GITBOTS_SESSION environment variable", trust: "mid" },
  flag: { label: "flag", title: "Explicit --session flag", trust: "mid" },
  tty: { label: "tty", title: "Interactive terminal confirmed by a human", trust: "high" },
  ui: { label: "ui", title: "This web UI, authenticated by its launch token", trust: "high" },
  git_config: { label: "git config", title: "Fell back to the human in git config (weakest)", trust: "low" },
  system: { label: "system", title: "gitbots itself (hooks, actions engine)", trust: "high" },
  other: { label: "other", title: "A resolution method from a newer gitbots", trust: "low" },
};

export function ViaHint(props: { via: Via | undefined }): JSX.Element {
  return (
    <Show when={props.via}>
      {(v) => {
        const h = VIA_HINT[v()] ?? VIA_HINT.other;
        return (
          <span class={`via trust-${h.trust}`} title={`via ${h.label}: ${h.title}`}>
            via {h.label}
          </span>
        );
      }}
    </Show>
  );
}

export function KindBadge(props: { kind: string }): JSX.Element {
  return <span class={`kind kind-${kindGroup(props.kind)}`}>{props.kind}</span>;
}

export function Parts(props: { parts: EventPart[] }): JSX.Element {
  return (
    <For each={props.parts}>
      {(p) =>
        typeof p === "string" ? (
          p
        ) : p.href ? (
          <A href={p.href} class={p.mono ? "mono" : undefined} title={p.title}>
            {p.text}
          </A>
        ) : (
          <code class="inline-code" title={p.title}>
            {p.text}
          </code>
        )
      }
    </For>
  );
}

export function EventRow(props: { event: Event; ctx?: DescribeContext; hideActor?: boolean }): JSX.Element {
  const d = createMemo(() => describeEvent(props.event, props.ctx));
  return (
    <li class={`event tone-${d().tone}${d().known ? "" : " is-unknown"}`} data-kind={props.event.kind}>
      <span class="event-time">
        <RelativeTime iso={props.event.ts} short />
      </span>
      <span class="event-kind">
        <KindBadge kind={props.event.kind} />
      </span>
      <span class="event-main">
        <Show when={!props.hideActor}>
          <span class="event-actor">
            <ActorChip actor={props.event.actor} />
            <ViaHint via={props.event.via} />
          </span>
        </Show>
        <span class="event-text">
          <Parts parts={d().parts} />
        </span>
        <Show when={d().detail}>
          <span class="event-detail">{d().detail}</span>
        </Show>
      </span>
    </li>
  );
}
