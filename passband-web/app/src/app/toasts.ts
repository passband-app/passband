// Toasts and the undo hold, as on the Mac: an action happens at once, the row
// leaves at once, and for five seconds an undo puts it back. Toasts without an
// undo leave after six seconds.
import { useSyncExternalStore } from "react";

export const UNDO_MS = 5_000;
export const TOAST_MS = 6_000;

export interface Toast {
  id: number;
  text: string;
  tone: "info" | "error";
  undo?: () => Promise<void>;
}

type Listener = () => void;

export class ToastStore {
  private toasts: Toast[] = [];
  private next = 1;
  private listeners = new Set<Listener>();
  private timers = new Map<number, ReturnType<typeof setTimeout>>();

  subscribe = (l: Listener) => {
    this.listeners.add(l);
    return () => this.listeners.delete(l);
  };
  snapshot = () => this.toasts;

  push(text: string, tone: Toast["tone"] = "info", undo?: () => Promise<void>): number {
    const id = this.next++;
    this.toasts = [...this.toasts, { id, text, tone, undo }];
    this.timers.set(id, setTimeout(() => this.dismiss(id), undo ? UNDO_MS : TOAST_MS));
    this.emit();
    return id;
  }

  dismiss(id: number): void {
    const t = this.timers.get(id);
    if (t) clearTimeout(t);
    this.timers.delete(id);
    const before = this.toasts.length;
    this.toasts = this.toasts.filter((x) => x.id !== id);
    if (this.toasts.length !== before) this.emit();
  }

  /** Run the newest live undo, if any. Returns whether there was one. */
  async undoLatest(): Promise<boolean> {
    const t = [...this.toasts].reverse().find((x) => x.undo);
    if (!t?.undo) return false;
    this.dismiss(t.id);
    try {
      await t.undo();
    } catch {
      this.push("undo failed", "error");
    }
    return true;
  }

  private emit() {
    this.listeners.forEach((l) => l());
  }
}

export const toasts = new ToastStore();

export function useToasts(): Toast[] {
  return useSyncExternalStore(toasts.subscribe, toasts.snapshot);
}
