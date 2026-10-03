import { Show, type JSX } from "solid-js";
import type { Actor, AgentDescriptor } from "../api/types";
import { providerColor } from "../lib/colors";
import { shortId } from "../lib/format";

type AgentChipProps = {
  agent: AgentDescriptor;
  session?: string | null | undefined;
  parent?: string | null | undefined;
  /** Hide the session short id (e.g. in leaderboards keyed by agent). */
  hideSession?: boolean;
  class?: string;
};

/** `model@client`, colored by provider, with session short id and a subagent marker. */
export function AgentChip(props: AgentChipProps): JSX.Element {
  const title = () => {
    const a = props.agent;
    const lines = [`${a.provider}/${a.model}@${a.client}${a.client_version ? ` ${a.client_version}` : ""}`];
    if (props.session) lines.push(`session ${props.session}`);
    if (props.parent) lines.push(`subagent of ${props.parent}`);
    return lines.join("\n");
  };
  return (
    <span
      class={`chip agent-chip${props.parent ? " is-sub" : ""}${props.class ? ` ${props.class}` : ""}`}
      style={{ "--chip-color": providerColor(props.agent.provider) }}
      title={title()}
      data-provider={props.agent.provider}
    >
      <span class="chip-dot" aria-hidden="true" />
      <Show when={props.parent}>
        <span class="chip-sub" aria-label="subagent">
          ↳
        </span>
      </Show>
      <span class="chip-model">{props.agent.model}</span>
      <span class="chip-client">@{props.agent.client}</span>
      <Show when={props.session && !props.hideSession}>
        <span class="chip-session">{shortId(props.session)}</span>
      </Show>
    </span>
  );
}

export function HumanChip(props: { handle: string; email?: string | undefined }): JSX.Element {
  return (
    <span class="chip human-chip" title={props.email ? `${props.handle} <${props.email}>` : props.handle}>
      <span class="chip-avatar" aria-hidden="true">
        {props.handle.slice(0, 1).toUpperCase()}
      </span>
      @{props.handle}
    </span>
  );
}

/** Any actor: human, agent, gitbots itself, or unknown. */
export function ActorChip(props: { actor: Actor | null | undefined; hideSession?: boolean }): JSX.Element {
  return (
    <>
      {(() => {
        const a = props.actor;
        if (!a) return <span class="chip muted-chip">nobody</span>;
        switch (a.type) {
          case "human":
            return <HumanChip handle={a.handle} email={a.email} />;
          case "agent":
            return <AgentChip agent={a.agent} session={a.session} parent={a.parent} hideSession={props.hideSession ?? false} />;
          case "system":
            return (
              <span class="chip system-chip" title={`gitbots system component: ${a.component}`}>
                <span class="chip-gear" aria-hidden="true">
                  ⚙
                </span>
                {a.component}
              </span>
            );
          default:
            return (
              <span class="chip muted-chip" title="An actor type from a newer gitbots">
                unknown
              </span>
            );
        }
      })()}
    </>
  );
}
