// Global keyboard shortcuts: `g x` sequences navigate, `/` focuses the page
// filter, `?` toggles the help overlay, `j`/`k` move between list items.
// Pages add their own single-key shortcuts with `useKey`.

import { onCleanup } from "solid-js";

export type ShortcutHelp = { keys: string[]; description: string };
export type ShortcutGroup = { title: string; items: ShortcutHelp[] };

export const NAV_SEQUENCES: Record<string, string> = {
  i: "/",
  b: "/board",
  a: "/activity",
  g: "/agents",
  s: "/sessions",
  m: "/mandate",
};

export const SHORTCUT_HELP: ShortcutGroup[] = [
  {
    title: "Navigate",
    items: [
      { keys: ["g", "i"], description: "Inbox" },
      { keys: ["g", "b"], description: "Board" },
      { keys: ["g", "a"], description: "Activity" },
      { keys: ["g", "g"], description: "Agents" },
      { keys: ["g", "s"], description: "Sessions" },
      { keys: ["g", "m"], description: "Mandate" },
    ],
  },
  {
    title: "Anywhere",
    items: [
      { keys: ["/"], description: "Focus the filter" },
      { keys: ["j"], description: "Next item" },
      { keys: ["k"], description: "Previous item" },
      { keys: ["?"], description: "Show this help" },
      { keys: ["Esc"], description: "Close / leave a field" },
    ],
  },
  {
    title: "Attempt review",
    items: [
      { keys: ["1"], description: "Diff" },
      { keys: ["2"], description: "Commits" },
      { keys: ["3"], description: "Checks" },
      { keys: ["4"], description: "Timeline" },
      { keys: ["r"], description: "Focus the review panel" },
    ],
  },
];

type Handler = (e: KeyboardEvent) => void;
const pageKeys = new Map<string, Handler>();

/** Registers a page-level single-key shortcut for the lifetime of the calling component. */
export function useKey(key: string, fn: Handler): void {
  pageKeys.set(key, fn);
  onCleanup(() => {
    if (pageKeys.get(key) === fn) pageKeys.delete(key);
  });
}

export function isEditable(t: EventTarget | null): boolean {
  if (!(t instanceof HTMLElement)) return false;
  if (t.isContentEditable) return true;
  const tag = t.tagName;
  if (tag === "TEXTAREA" || tag === "SELECT") return true;
  if (tag === "INPUT") {
    const type = (t as HTMLInputElement).type;
    return !["checkbox", "radio", "button", "submit", "reset"].includes(type);
  }
  return false;
}

/** Moves focus to the next/previous `[data-nav-item]` on the page. */
export function moveFocus(delta: 1 | -1): void {
  const items = Array.from(document.querySelectorAll<HTMLElement>("[data-nav-item]")).filter(
    (el) => el.offsetParent !== null || el.getClientRects().length > 0,
  );
  if (!items.length) return;
  const active = document.activeElement;
  let i = items.findIndex((el) => el === active || el.contains(active));
  i = i < 0 ? (delta > 0 ? 0 : items.length - 1) : Math.min(items.length - 1, Math.max(0, i + delta));
  const el = items[i]!;
  el.focus();
  el.scrollIntoView({ block: "nearest" });
}

export type ShortcutOptions = {
  navigate: (path: string) => void;
  toggleHelp: () => void;
  closeOverlay: () => boolean;
};

export function installShortcuts(opts: ShortcutOptions): () => void {
  let pendingG = false;
  let timer: ReturnType<typeof setTimeout> | undefined;

  const onKey = (e: KeyboardEvent) => {
    if (e.defaultPrevented || e.metaKey || e.ctrlKey || e.altKey) return;
    if (e.key === "Escape") {
      if (opts.closeOverlay()) {
        e.preventDefault();
        return;
      }
      if (isEditable(e.target)) (e.target as HTMLElement).blur();
      return;
    }
    if (isEditable(e.target)) return;

    if (pendingG) {
      pendingG = false;
      clearTimeout(timer);
      const to = NAV_SEQUENCES[e.key];
      if (to) {
        e.preventDefault();
        opts.navigate(to);
      }
      return;
    }

    switch (e.key) {
      case "g":
        pendingG = true;
        timer = setTimeout(() => (pendingG = false), 1200);
        return;
      case "?":
        e.preventDefault();
        opts.toggleHelp();
        return;
      case "/": {
        const el = document.querySelector<HTMLElement>("[data-shortcut-filter]");
        if (el) {
          e.preventDefault();
          el.focus();
          if (el instanceof HTMLInputElement) el.select();
        }
        return;
      }
      case "j":
        e.preventDefault();
        moveFocus(1);
        return;
      case "k":
        e.preventDefault();
        moveFocus(-1);
        return;
    }
    const page = pageKeys.get(e.key);
    if (page) page(e);
  };

  window.addEventListener("keydown", onKey);
  return () => {
    window.removeEventListener("keydown", onKey);
    clearTimeout(timer);
  };
}
