import { For, Show, type JSX } from "solid-js";
import { ApiError } from "../api/client";

export function Spinner(props: { label?: string; inline?: boolean }): JSX.Element {
  return (
    <span class={`spinner-wrap${props.inline ? " inline" : ""}`} role="status">
      <span class="spinner" aria-hidden="true" />
      <span class={props.inline ? "sr-only" : "spinner-label"}>{props.label ?? "Loading…"}</span>
    </span>
  );
}

/** Placeholder rows while a page loads. */
export function Skeleton(props: { rows?: number; class?: string }): JSX.Element {
  const rows = () => Array.from({ length: props.rows ?? 4 }, (_, i) => i);
  return (
    <div class={`skeleton${props.class ? ` ${props.class}` : ""}`} aria-busy="true" aria-label="Loading">
      <For each={rows()}>{(i) => <div class="skeleton-row" style={{ width: `${92 - ((i * 17) % 35)}%` }} />}</For>
    </div>
  );
}

export function EmptyState(props: { title: string; icon?: JSX.Element; children?: JSX.Element; class?: string }): JSX.Element {
  return (
    <div class={`empty-state${props.class ? ` ${props.class}` : ""}`}>
      <Show when={props.icon}>
        <div class="empty-icon" aria-hidden="true">
          {props.icon}
        </div>
      </Show>
      <h2 class="empty-title">{props.title}</h2>
      <Show when={props.children}>
        <div class="empty-body">{props.children}</div>
      </Show>
    </div>
  );
}

export function errorText(e: unknown): string {
  if (e instanceof ApiError) return e.message;
  if (e instanceof Error) return e.message;
  return String(e);
}

export function ErrorState(props: { error: unknown; retry?: () => void; title?: string }): JSX.Element {
  const status = () => (props.error instanceof ApiError ? props.error.status : null);
  return (
    <div class="error-state" role="alert">
      <div class="error-head">
        <span class="error-icon" aria-hidden="true">
          !
        </span>
        <strong>{props.title ?? (status() === 404 ? "Not found" : "Something went wrong")}</strong>
        <Show when={status()}>
          <span class="muted mono">HTTP {status()}</span>
        </Show>
      </div>
      <p class="error-msg">{errorText(props.error)}</p>
      <Show when={props.retry}>
        <button type="button" class="btn btn-sm" onClick={() => props.retry?.()}>
          Retry
        </button>
      </Show>
    </div>
  );
}
