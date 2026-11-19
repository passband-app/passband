// Single-letter shortcuts, with the Mac client's input guard: a bare letter
// never fires while the person is typing, and never with ⌘/Ctrl/Alt held.
import { useEffect, useRef } from "react";

export type KeyMap = Record<string, () => void>;

export function isTyping(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  return (
    target.isContentEditable ||
    target instanceof HTMLInputElement ||
    target instanceof HTMLTextAreaElement ||
    target instanceof HTMLSelectElement
  );
}

/** Exact match first (so `E` and `e` can differ), then the lowercase sibling. */
export function lookup(map: KeyMap, key: string): (() => void) | undefined {
  return map[key] ?? (key.length === 1 ? map[key.toLowerCase()] : undefined);
}

export function useKeys(map: KeyMap): void {
  const ref = useRef(map);
  ref.current = map;
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.metaKey || e.ctrlKey || e.altKey || e.defaultPrevented) return;
      if (e.key !== "Escape" && isTyping(e.target)) return;
      const f = lookup(ref.current, e.key);
      if (!f) return;
      e.preventDefault();
      f();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);
}
