import { createMemo, createSignal, For, Show, type JSX } from "solid-js";
import { api, isQueued } from "../api/client";
import { addPending, pendingLabel, pendingReview } from "../api/pending";
import type { AttemptState, BoardAttempt, ProjectInfo, QueuedResponse, ReviewDecision, ReviewOutcome } from "../api/types";
import { PendingBadge } from "./PendingBadge";
import { approverText, globMatch, satisfies, viewerRole } from "../lib/mandate";
import { shortSha } from "../lib/format";
import { errorText } from "./States";

const DECISIONS: { value: ReviewDecision; label: string; tone: string }[] = [
  { value: "accept", label: "Accept", tone: "good" },
  { value: "changes_requested", label: "Request changes", tone: "warn" },
  { value: "reject", label: "Reject", tone: "bad" },
];

const STATE_TEXT: Record<AttemptState, string> = {
  active: "still active (not submitted yet)",
  submitted: "submitted",
  changes_requested: "waiting for the agent to address requested changes",
  accepted: "already accepted",
  rejected: "rejected",
  merged: "merged",
  abandoned: "abandoned",
};

export type ReviewPanelProps = {
  attempt: BoardAttempt;
  project: ProjectInfo | undefined;
  /** Called after a successful (or hosted: queued) review; the page refetches. */
  onDecided?: (o: ReviewOutcome | QueuedResponse) => void;
  /** Submit implementation (tests); defaults to the API. */
  submit?: typeof api.review;
};

/** Accept / request changes / reject, with the mandate's rules spelled out. */
export function ReviewPanel(props: ReviewPanelProps): JSX.Element {
  const [decision, setDecision] = createSignal<ReviewDecision>("accept");
  const [reason, setReason] = createSignal("");
  const [merge, setMerge] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [outcome, setOutcome] = createSignal<ReviewOutcome | null>(null);
  const [queued, setQueued] = createSignal<QueuedResponse | null>(null);
  const pending = () => pendingReview(props.attempt.id);

  const mandate = () => props.project?.manifest.mandate;
  const isProtected = createMemo(() => (mandate()?.agents.protected_branches ?? []).some((g) => globMatch(g, props.attempt.base)));
  const role = createMemo(() => (props.project ? viewerRole(props.project) : null));

  const disabledReason = createMemo((): string | null => {
    const p = props.project;
    if (!p) return "Loading the mandate…";
    if (!p.can_decide) {
      return "Read-only: gitbots ui was started from an agent session (CLAUDECODE, CODEX_* or GITBOTS_SESSION in its environment), so it can't act as a human. Start gitbots ui from your own terminal to decide.";
    }
    if (props.attempt.state !== "submitted") return `This attempt is ${STATE_TEXT[props.attempt.state]}; only submitted attempts can be reviewed.`;
    if (pending()) return "Your decision is queued in the hosted outbox and applied on the next gitbots sync.";
    return null;
  });
  const disabled = () => disabledReason() !== null || busy();

  const canAccept = () => {
    const m = mandate();
    return m && props.project ? satisfies(m.approvals.accept_attempt, props.project.viewer, role()) : false;
  };
  const canMerge = () => {
    const m = mandate();
    if (!m || !props.project) return false;
    return !isProtected() || satisfies(m.approvals.merge_protected, props.project.viewer, role());
  };

  const submitLabel = () => {
    if (busy()) return "Submitting…";
    switch (decision()) {
      case "accept":
        return merge() ? `Accept and merge into ${props.attempt.base}` : "Accept";
      case "changes_requested":
        return "Request changes";
      case "reject":
        return "Reject";
    }
  };

  const onSubmit = async (e: SubmitEvent) => {
    e.preventDefault();
    if (disabled()) return;
    setBusy(true);
    setError(null);
    setOutcome(null);
    setQueued(null);
    try {
      const body = {
        decision: decision(),
        ...(reason().trim() ? { reason: reason().trim() } : {}),
        ...(decision() === "accept" && merge() ? { merge: true } : {}),
      };
      const res = await (props.submit ?? api.review)(props.attempt.id, body);
      if (isQueued(res)) {
        // Hosted: stored in the outbox (202); the steward applies it on `gitbots sync`.
        addPending({
          kind: "review",
          outbox: res.outbox,
          attempt: props.attempt.id,
          decision: body.decision,
          merge: body.merge === true,
          at: Date.now(),
        });
        setQueued(res);
      } else {
        setOutcome(res);
      }
      setReason("");
      props.onDecided?.(res);
    } catch (err) {
      // Shown verbatim: the server's message names the rule that blocked it.
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section class="card review-panel" aria-labelledby="review-h" id="review-panel">
      <h2 id="review-h" class="section-title">
        Review
      </h2>

      <Show when={mandate()}>
        {(m) => (
          <dl class="who">
            <div>
              <dt>Accept</dt>
              <dd>
                {approverText(m().approvals.accept_attempt)}
                <Mark ok={canAccept()} />
              </dd>
            </div>
            <div>
              <dt>Merge into {props.attempt.base}</dt>
              <dd>
                <Show when={isProtected()} fallback={<>whoever accepts (not a protected branch)</>}>
                  {approverText(m().approvals.merge_protected)} (protected)
                </Show>
                <Mark ok={canMerge()} />
              </dd>
            </div>
            <div>
              <dt>You</dt>
              <dd>
                <Show when={props.project?.viewer?.type === "human"} fallback={<span class="muted">no human viewer</span>}>
                  @{(props.project!.viewer as { handle: string }).handle}
                  {role() ? ` (${role()})` : " (not a principal)"}
                </Show>
              </dd>
            </div>
          </dl>
        )}
      </Show>
      <p class="muted small">Agents in this attempt's session family can't approve it.</p>

      <form onSubmit={onSubmit} class="review-form">
        <fieldset disabled={disabled()} class="review-fieldset">
          <legend class="sr-only">Decision</legend>
          <div class="segmented" role="radiogroup" aria-label="Decision">
            <For each={DECISIONS}>
              {(d) => (
                <label class={`seg tone-${d.tone}${decision() === d.value ? " checked" : ""}`}>
                  <input
                    type="radio"
                    name="decision"
                    value={d.value}
                    checked={decision() === d.value}
                    onChange={() => setDecision(d.value)}
                    data-review-first={d.value === "accept" ? "" : undefined}
                  />
                  {d.label}
                </label>
              )}
            </For>
          </div>
          <label class="field">
            <span class="field-label">
              {decision() === "changes_requested" ? "What should change?" : "Reason"}{" "}
              <span class="muted">(optional, the agent sees it)</span>
            </span>
            <textarea class="input textarea" rows="3" value={reason()} onInput={(e) => setReason(e.currentTarget.value)} />
          </label>
          <Show when={decision() === "accept"}>
            <label class="check">
              <input type="checkbox" checked={merge()} onChange={(e) => setMerge(e.currentTarget.checked)} />
              Merge into <code>{props.attempt.base}</code>
            </label>
          </Show>
          <button type="submit" class={`btn btn-block btn-${decision() === "accept" ? "primary" : decision() === "reject" ? "danger" : "warn"}`} disabled={disabled()}>
            {submitLabel()}
          </button>
        </fieldset>
      </form>

      <Show when={disabledReason() && props.project && !pending()}>
        <p class="notice notice-muted" data-testid="review-disabled">
          {disabledReason()}
        </p>
      </Show>

      <div aria-live="polite" aria-atomic="true" class="review-result">
        <Show when={error()}>
          <p class="notice notice-bad" role="alert" data-testid="review-error">
            {error()}
          </p>
        </Show>
        <Show when={queued() || pending()}>
          <p class="notice notice-pending" data-testid="review-queued">
            <PendingBadge>{pending() ? pendingLabel(pending()!) : "pending sync"}</PendingBadge> Queued: applied on the next{" "}
            <code>gitbots sync</code>.
            <Show when={queued()}>
              {(q) => <span class="muted small"> Outbox item {q().outbox}.</span>}
            </Show>
          </p>
        </Show>
        <Show when={outcome()}>
          {(o) => (
            <p class="notice notice-good">
              {o().decision === "accept" ? "Accepted" : o().decision === "reject" ? "Rejected" : "Changes requested"}
              <Show when={o().merged}>
                {(m) => (
                  <>
                    {" "}
                    and merged as <code>{shortSha(m())}</code>
                  </>
                )}
              </Show>
              .
            </p>
          )}
        </Show>
      </div>
    </section>
  );
}

function Mark(props: { ok: boolean }): JSX.Element {
  return (
    <span class={`who-mark ${props.ok ? "ok" : "no"}`} title={props.ok ? "You qualify" : "You don't qualify; the server will refuse"}>
      {props.ok ? " ✓ you" : " ✕ not you"}
    </span>
  );
}
