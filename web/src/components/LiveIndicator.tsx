import type { JSX } from "solid-js";
import { lastChange, liveStatus } from "../api/live";
import { now } from "./RelativeTime";
import { relativeTime } from "../lib/format";

const LABEL = { connecting: "Connecting", live: "Live", paused: "Paused", offline: "Offline" } as const;
const HELP = {
  connecting: "Connecting to gitbots ui…",
  live: "Polling the activity ledger every 3 s",
  paused: "Paused while this tab is hidden",
  offline: "Can't reach gitbots ui; retrying every 3 s",
} as const;

export function LiveIndicator(): JSX.Element {
  const title = () => {
    const c = lastChange();
    const base = HELP[liveStatus()];
    return c ? `${base}\nLast change ${relativeTime(new Date(c).toISOString(), now())}` : base;
  };
  return (
    <span class={`live live-${liveStatus()}`} title={title()} role="status" aria-label={`${LABEL[liveStatus()]}: ${HELP[liveStatus()]}`}>
      <span class="live-dot" aria-hidden="true" />
      <span class="live-label">{LABEL[liveStatus()]}</span>
    </span>
  );
}
