// Light, dark, or auto (follow the system), as on the Mac. Auto is the default.
//
// Applied as `data-theme` on <html>. The stylesheet carries the dark tokens
// twice (see app.css): under `[data-theme="dark"]` for an explicit choice, and
// under `prefers-color-scheme: dark` for auto, which therefore needs no script
// and is right from the first frame. Only an explicit choice that differs from
// the system depends on this module, which the entry imports before anything
// else so it runs as early as a module can (the CSP allows no inline script).
import { useSyncExternalStore } from "react";

export type ThemeChoice = "auto" | "light" | "dark";

export const THEME_KEY = "pb_theme_v1";
const ORDER: ThemeChoice[] = ["auto", "light", "dark"];

export function parseChoice(raw: string | null | undefined): ThemeChoice {
  return raw === "light" || raw === "dark" ? raw : "auto";
}

/** The next choice for the `\` key: auto → light → dark → auto. */
export function nextChoice(c: ThemeChoice): ThemeChoice {
  return ORDER[(ORDER.indexOf(c) + 1) % ORDER.length]!;
}

export function label(c: ThemeChoice): string {
  return c === "auto" ? "Theme: auto (follows your system)" : `Theme: ${c}`;
}

function storage(): Storage | undefined {
  try {
    return typeof localStorage === "undefined" ? undefined : localStorage;
  } catch {
    return undefined;
  }
}

export function loadChoice(s: Pick<Storage, "getItem"> | undefined = storage()): ThemeChoice {
  try {
    return parseChoice(s?.getItem(THEME_KEY));
  } catch {
    return "auto";
  }
}

/** Write the choice onto the document. Auto removes the attribute. */
export function applyChoice(c: ThemeChoice, root: HTMLElement = document.documentElement): void {
  if (c === "auto") root.removeAttribute("data-theme");
  else root.setAttribute("data-theme", c);
}

let current: ThemeChoice = "auto";
const listeners = new Set<() => void>();

export function bootTheme(): void {
  current = loadChoice();
  applyChoice(current);
}

export function setChoice(c: ThemeChoice): void {
  current = c;
  applyChoice(c);
  try {
    const s = storage();
    if (c === "auto") s?.removeItem(THEME_KEY);
    else s?.setItem(THEME_KEY, c);
  } catch {
    // Blocked storage: the choice lasts for this visit.
  }
  listeners.forEach((l) => l());
}

export function cycleTheme(): void {
  setChoice(nextChoice(current));
}

export function useThemeChoice(): ThemeChoice {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => current,
  );
}
