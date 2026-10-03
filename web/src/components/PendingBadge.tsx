import { Show, type JSX } from "solid-js";
import { pendingLabel, pendingReview } from "../api/pending";

const TITLE = "Queued in the hosted outbox. Applied on the next `gitbots sync`.";

/** "⧗ accept pending sync" for an attempt with a queued (hosted) decision. */
export function PendingReviewBadge(props: { attempt: string; compact?: boolean }): JSX.Element {
  return (
    <Show when={pendingReview(props.attempt)}>
      {(p) => (
        <span class={`pending-badge${props.compact ? " compact" : ""}`} title={TITLE} data-testid="pending-badge">
          <span aria-hidden="true">⧗</span> {props.compact ? "pending sync" : pendingLabel(p())}
        </span>
      )}
    </Show>
  );
}

export function PendingBadge(props: { children: JSX.Element }): JSX.Element {
  return (
    <span class="pending-badge" title={TITLE}>
      <span aria-hidden="true">⧗</span> {props.children}
    </span>
  );
}
