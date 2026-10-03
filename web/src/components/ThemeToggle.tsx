import type { JSX } from "solid-js";
import { cycleTheme, theme } from "../lib/theme";

const ICON = { system: "◑", light: "☀", dark: "☾" } as const;
const NEXT = { system: "light", light: "dark", dark: "system" } as const;

export function ThemeToggle(): JSX.Element {
  return (
    <button
      type="button"
      class="icon-btn theme-toggle"
      onClick={cycleTheme}
      aria-label={`Theme: ${theme()}. Switch to ${NEXT[theme()]}`}
      title={`Theme: ${theme()} (click for ${NEXT[theme()]})`}
    >
      <span aria-hidden="true">{ICON[theme()]}</span>
    </button>
  );
}
