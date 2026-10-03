import { createSignal } from "solid-js";

export type ThemePref = "system" | "light" | "dark";
const KEY = "gitbots.theme";

function read(): ThemePref {
  try {
    const v = localStorage.getItem(KEY);
    return v === "light" || v === "dark" ? v : "system";
  } catch {
    return "system";
  }
}

const [theme, setThemeSignal] = createSignal<ThemePref>(typeof window === "undefined" ? "system" : read());
export { theme };

export function setTheme(t: ThemePref): void {
  setThemeSignal(t);
  const root = document.documentElement;
  if (t === "system") root.removeAttribute("data-theme");
  else root.setAttribute("data-theme", t);
  try {
    if (t === "system") localStorage.removeItem(KEY);
    else localStorage.setItem(KEY, t);
  } catch {
    /* storage blocked: the choice lasts for this page */
  }
}

export function cycleTheme(): void {
  const order: ThemePref[] = ["system", "light", "dark"];
  setTheme(order[(order.indexOf(theme()) + 1) % order.length]!);
}
