import type { JSX } from "solid-js";
import type { AttemptState, ReportLevel, RunStatus, TaskStatus } from "../api/types";

type Tone = "good" | "bad" | "warn" | "info" | "merged" | "neutral";
type Def = { label: string; tone: Tone; icon: string };

const ATTEMPT: Record<AttemptState, Def> = {
  active: { label: "Active", tone: "info", icon: "◐" },
  submitted: { label: "Awaiting review", tone: "warn", icon: "●" },
  changes_requested: { label: "Changes requested", tone: "warn", icon: "↺" },
  accepted: { label: "Accepted", tone: "good", icon: "✓" },
  rejected: { label: "Rejected", tone: "bad", icon: "✕" },
  merged: { label: "Merged", tone: "merged", icon: "⑂" },
  abandoned: { label: "Abandoned", tone: "neutral", icon: "–" },
};
const TASK: Record<TaskStatus, Def> = {
  open: { label: "Open", tone: "neutral", icon: "○" },
  in_progress: { label: "In progress", tone: "info", icon: "◐" },
  accepted: { label: "Accepted", tone: "good", icon: "✓" },
  done: { label: "Done", tone: "merged", icon: "⑂" },
};
const RUN: Record<RunStatus, Def> = {
  success: { label: "Passed", tone: "good", icon: "✓" },
  failure: { label: "Failed", tone: "bad", icon: "✕" },
  timed_out: { label: "Timed out", tone: "bad", icon: "⏱" },
  cancelled: { label: "Cancelled", tone: "neutral", icon: "⊘" },
  skipped: { label: "Skipped", tone: "neutral", icon: "–" },
};
const LEVEL: Record<ReportLevel, Def> = {
  blocker: { label: "Blocker", tone: "bad", icon: "■" },
  warning: { label: "Warning", tone: "warn", icon: "▲" },
  info: { label: "Info", tone: "info", icon: "i" },
};

function lookup(kind: string, value: string): Def {
  const table: Record<string, Record<string, Def>> = { attempt: ATTEMPT, task: TASK, run: RUN, level: LEVEL };
  return table[kind]?.[value] ?? { label: value.replace(/_/g, " "), tone: "neutral", icon: "•" };
}

export type PillProps =
  | { kind: "attempt"; value: AttemptState }
  | { kind: "task"; value: TaskStatus }
  | { kind: "run"; value: RunStatus }
  | { kind: "level"; value: ReportLevel };

export function StatePill(props: PillProps & { compact?: boolean }): JSX.Element {
  const def = () => lookup(props.kind, props.value);
  return (
    <span class={`pill tone-${def().tone}${props.compact ? " compact" : ""}`} data-state={props.value}>
      <span class="pill-icon" aria-hidden="true">
        {def().icon}
      </span>
      {def().label}
    </span>
  );
}

export function attemptTone(s: AttemptState): Tone {
  return ATTEMPT[s].tone;
}
