import { createSignal, For, Show, type JSX } from "solid-js";
import type { DiffStat, RunSummary } from "../api/types";
import { formatNumber, shortSha } from "../lib/format";

/** `+412 −37` with a five-block bar, like a git diffstat. */
export function DiffStatView(props: { diff: DiffStat | null | undefined; files?: boolean }): JSX.Element {
  const blocks = () => {
    const d = props.diff;
    if (!d) return [] as string[];
    const total = d.insertions + d.deletions;
    if (total === 0) return ["n", "n", "n", "n", "n"];
    const add = Math.round((d.insertions / total) * 5);
    return Array.from({ length: 5 }, (_, i) => (i < add ? "a" : "d"));
  };
  return (
    <Show when={props.diff} fallback={<span class="diffstat muted">no diff</span>}>
      {(d) => (
        <span
          class="diffstat"
          title={`${d().files} files changed, ${d().insertions} insertions(+), ${d().deletions} deletions(-)`}
        >
          <Show when={props.files}>
            <span class="diffstat-files">{d().files === 1 ? "1 file" : `${d().files} files`}</span>
          </Show>
          <span class="add">+{formatNumber(d().insertions)}</span>
          <span class="del">−{formatNumber(d().deletions)}</span>
          <span class="diffstat-bar" aria-hidden="true">
            <For each={blocks()}>{(b) => <i class={b} />}</For>
          </span>
        </span>
      )}
    </Show>
  );
}

/** Checks summary: passed / failing / none yet. */
export function ChecksBadge(props: { passed: boolean | null | undefined; runs?: RunSummary[] }): JSX.Element {
  const label = () => {
    const runs = props.runs ?? [];
    if (props.passed === true) return { tone: "good", icon: "✓", text: runs.length > 1 ? `${runs.length} checks passed` : "Checks passed" };
    if (props.passed === false) {
      const failed = runs.filter((r) => r.status !== "success").map((r) => r.workflow);
      return { tone: "bad", icon: "✕", text: failed.length ? `${failed.join(", ")} failing` : "Checks failing" };
    }
    return { tone: "neutral", icon: "○", text: "No checks yet" };
  };
  return (
    <span class={`checks tone-${label().tone}`}>
      <span class="checks-icon" aria-hidden="true">
        {label().icon}
      </span>
      {label().text}
    </span>
  );
}

export function Labels(props: { labels: string[] }): JSX.Element {
  return (
    <Show when={props.labels.length}>
      <span class="labels">
        <For each={props.labels}>{(l) => <span class="label">{l}</span>}</For>
      </span>
    </Show>
  );
}

/** A sha (or id) in mono with a copy button. */
export function Sha(props: { sha: string | null | undefined; short?: (s: string) => string; title?: string }): JSX.Element {
  const [copied, setCopied] = createSignal(false);
  const copy = async () => {
    if (!props.sha) return;
    try {
      await navigator.clipboard.writeText(props.sha);
      setCopied(true);
      setTimeout(() => setCopied(false), 1200);
    } catch {
      /* clipboard blocked */
    }
  };
  return (
    <Show when={props.sha} fallback={<span class="muted">—</span>}>
      {(s) => (
        <span class="sha">
          <code title={props.title ?? s()}>{(props.short ?? shortSha)(s())}</code>
          <button type="button" class="icon-btn copy-btn" onClick={copy} aria-label={`Copy ${s()}`} title="Copy">
            {copied() ? "✓" : "⧉"}
          </button>
        </span>
      )}
    </Show>
  );
}

/** A horizontal rate bar (0..1) with the value as text; "—" when null. */
export function RateBar(props: { value: number | null; title?: string }): JSX.Element {
  return (
    <span class="ratebar" title={props.title}>
      <Show when={props.value !== null} fallback={<span class="ratebar-none">—</span>}>
        <span class="ratebar-num">{Math.round((props.value ?? 0) * 100)}%</span>
        <span class="ratebar-track" aria-hidden="true">
          <span class="ratebar-fill" style={{ width: `${Math.max(2, (props.value ?? 0) * 100)}%` }} />
        </span>
      </Show>
    </span>
  );
}

export function Kbd(props: { children: JSX.Element }): JSX.Element {
  return <kbd class="kbd">{props.children}</kbd>;
}
