import { createSignal, type JSX } from "solid-js";
import { absoluteTime, age, relativeTime } from "../lib/format";

// One shared clock for every RelativeTime on the page.
const [now, setNow] = createSignal(Date.now());
if (typeof window !== "undefined") setInterval(() => setNow(Date.now()), 30_000);
export { now };

export function RelativeTime(props: { iso: string; short?: boolean; class?: string }): JSX.Element {
  return (
    <time class={`reltime${props.class ? ` ${props.class}` : ""}`} datetime={props.iso} title={absoluteTime(props.iso)}>
      {props.short ? age(props.iso, now()) : relativeTime(props.iso, now())}
    </time>
  );
}
