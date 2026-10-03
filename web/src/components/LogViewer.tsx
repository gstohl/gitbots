import { createEffect, createMemo, createResource, createSignal, For, on, onCleanup, onMount, Show, type JSX } from "solid-js";
import { api } from "../api/client";
import { firstErrorLine, isErrorLine, logLines, searchLines } from "../lib/log";
import { ErrorState, Spinner } from "./States";

const MAX_LINES = 20_000;

function highlight(line: string, q: string): JSX.Element {
  if (!q) return line;
  const lower = line.toLowerCase();
  const needle = q.toLowerCase();
  const out: JSX.Element[] = [];
  let from = 0;
  let i = lower.indexOf(needle);
  while (i >= 0) {
    if (i > from) out.push(line.slice(from, i));
    out.push(<mark>{line.slice(i, i + needle.length)}</mark>);
    from = i + needle.length;
    i = lower.indexOf(needle, from);
  }
  if (from < line.length) out.push(line.slice(from));
  return out;
}

/** Side sheet showing one job log from the logs branch, with search. */
export function LogViewer(props: { path: string; title: string; onClose: () => void }): JSX.Element {
  const [text] = createResource(() => props.path, (p) => api.log(p));
  const [query, setQuery] = createSignal("");
  const [cursor, setCursor] = createSignal(0);
  const [showAll, setShowAll] = createSignal(false);
  let scroller: HTMLDivElement | undefined;
  let closeBtn: HTMLButtonElement | undefined;

  const lines = createMemo(() => (text.state === "ready" ? logLines(text()) : []));
  const visible = createMemo(() => (showAll() ? lines() : lines().slice(0, MAX_LINES)));
  const errorAt = createMemo(() => firstErrorLine(lines()));
  const matches = createMemo(() => searchLines(visible(), query()));
  const matchSet = createMemo(() => new Set(matches()));

  const scrollTo = (i: number) => {
    const el = scroller?.querySelector<HTMLElement>(`[data-line="${i}"]`);
    el?.scrollIntoView({ block: "center" });
  };

  onMount(() => {
    const prev = document.activeElement as HTMLElement | null;
    closeBtn?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        props.onClose();
      }
    };
    window.addEventListener("keydown", onKey, true);
    onCleanup(() => {
      window.removeEventListener("keydown", onKey, true);
      prev?.focus?.();
    });
  });

  // Jump to the first failure once loaded.
  createEffect(
    on(errorAt, (i) => {
      if (i >= 0 && i < visible().length) queueMicrotask(() => scrollTo(i));
    }),
  );
  createEffect(
    on(matches, (m) => {
      setCursor(0);
      if (m.length) queueMicrotask(() => scrollTo(m[0]!));
    }),
  );

  const step = (d: 1 | -1) => {
    const m = matches();
    if (!m.length) return;
    const next = (cursor() + d + m.length) % m.length;
    setCursor(next);
    scrollTo(m[next]!);
  };

  return (
    <div class="overlay sheet-overlay" onClick={(e) => e.target === e.currentTarget && props.onClose()}>
      <div class="sheet log-sheet" role="dialog" aria-modal="true" aria-labelledby="log-title">
        <div class="sheet-head">
          <div class="sheet-title">
            <h2 id="log-title">{props.title}</h2>
            <code class="muted small" title={props.path}>
              {props.path}
            </code>
          </div>
          <button ref={closeBtn} type="button" class="icon-btn" onClick={() => props.onClose()} aria-label="Close log">
            ✕
          </button>
        </div>
        <div class="log-tools">
          <input
            class="input input-sm log-search"
            type="search"
            placeholder="Search log"
            aria-label="Search log"
            value={query()}
            onInput={(e) => setQuery(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                step(e.shiftKey ? -1 : 1);
              }
            }}
          />
          <span class="muted small" aria-live="polite">
            <Show when={query()}>{matches().length ? `${cursor() + 1} / ${matches().length}` : "no matches"}</Show>
          </span>
          <button type="button" class="btn btn-sm btn-ghost" onClick={() => step(-1)} disabled={!matches().length} aria-label="Previous match">
            ↑
          </button>
          <button type="button" class="btn btn-sm btn-ghost" onClick={() => step(1)} disabled={!matches().length} aria-label="Next match">
            ↓
          </button>
          <span class="spacer" />
          <Show when={errorAt() >= 0}>
            <button type="button" class="btn btn-sm btn-ghost text-bad" onClick={() => scrollTo(errorAt())}>
              First error (line {errorAt() + 1})
            </button>
          </Show>
        </div>
        <div class="log-scroll" ref={scroller} tabindex="0" aria-label="Log output">
          <Show when={text.loading}>
            <div class="log-pad">
              <Spinner label="Loading log…" />
            </div>
          </Show>
          <Show when={text.error}>
            <div class="log-pad">
              <ErrorState error={text.error} />
            </div>
          </Show>
          <Show when={text.state === "ready"}>
            <Show when={lines().length} fallback={<div class="log-pad muted">Empty log.</div>}>
              <pre class="log">
                <For each={visible()}>
                  {(l, i) => (
                    <div
                      class={`log-line${i() === errorAt() ? " is-first-error" : isErrorLine(l) ? " is-error" : ""}${
                        matchSet().has(i()) ? " is-match" : ""
                      }${matches()[cursor()] === i() ? " is-current" : ""}`}
                      data-line={i()}
                    >
                      <span class="log-ln" data-ln={i() + 1} />
                      <span class="log-text">{matchSet().has(i()) ? highlight(l, query()) : l}</span>
                    </div>
                  )}
                </For>
              </pre>
              <Show when={!showAll() && lines().length > MAX_LINES}>
                <button type="button" class="btn btn-sm log-more" onClick={() => setShowAll(true)}>
                  Show all {lines().length.toLocaleString()} lines
                </button>
              </Show>
            </Show>
          </Show>
        </div>
      </div>
    </div>
  );
}
