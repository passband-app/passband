// FLOW MODE: Space on the sitrep walks "For your eyes" one email at a time,
// as on the Mac (AppStore.startWalk / finishWalk, Views/SitrepView.swift's
// eyesWalk). E is done-and-next, S is not-sure-yet-and-next, the arrows move
// without deciding, and at the end you are back on the board with a tally.
//
// The walk is a pure state machine here; WalkStore wraps it for React.
import { useSyncExternalStore } from "react";
import type { EyesItem } from "../sitrep/model";

export interface Walk {
  queue: EyesItem[];
  index: number;
  done: number;
  skipped: number;
  startedAt: number;
}

export interface WalkSummary {
  done: number;
  skipped: number;
  durationMs: number;
}

/**
 * Sealed mail (a sign-in code) is never on the walk: it opens in a one-time
 * reveal, never the reader, so a walk that stepped onto it would leave the walk.
 */
export function walkQueue(eyes: EyesItem[]): EyesItem[] {
  return eyes.filter((e) => !e.sealed);
}

/** Start at the highlighted row when there is one on the walk, else the first. */
export function startWalk(eyes: EyesItem[], highlighted: EyesItem | undefined, now: number): Walk | null {
  const queue = walkQueue(eyes);
  if (queue.length === 0) return null;
  const at = highlighted ? queue.findIndex((e) => e.id === highlighted.id) : -1;
  return { queue, index: Math.max(at, 0), done: 0, skipped: 0, startedAt: now };
}

export function current(w: Walk): EyesItem | undefined {
  return w.queue[w.index];
}

export type Step = "done" | "skip";

/** Count the step and move on. `finished` when that was the last email. */
export function step(w: Walk, kind: Step): { walk: Walk; finished: boolean } {
  const counted = {
    ...w,
    done: w.done + (kind === "done" ? 1 : 0),
    skipped: w.skipped + (kind === "skip" ? 1 : 0),
  };
  if (w.index + 1 >= w.queue.length) return { walk: counted, finished: true };
  return { walk: { ...counted, index: w.index + 1 }, finished: false };
}

/** Move without deciding. Stops at either end. */
export function move(w: Walk, by: 1 | -1): Walk {
  const index = Math.min(Math.max(w.index + by, 0), w.queue.length - 1);
  return index === w.index ? w : { ...w, index };
}

/**
 * Undo of a step: take it back off the tally, and step back onto the email it
 * acted on when that email is the one just behind. Anywhere else the undo is
 * just an undo (the mail comes back; the walk stays where it is).
 */
export function rewind(w: Walk, kind: Step, itemId: number): Walk {
  const tally = {
    ...w,
    done: Math.max(0, w.done - (kind === "done" ? 1 : 0)),
    skipped: Math.max(0, w.skipped - (kind === "skip" ? 1 : 0)),
  };
  const back = w.queue[w.index - 1];
  return back?.id === itemId ? { ...tally, index: w.index - 1 } : tally;
}

/** The summary the board shows, or null when nothing was decided. */
export function summarize(w: Walk, now: number): WalkSummary | null {
  if (w.done + w.skipped === 0) return null;
  return { done: w.done, skipped: w.skipped, durationMs: Math.max(0, now - w.startedAt) };
}

/** "4 done, 1 saved for later, in 2 minutes." */
export function summaryLine(s: WalkSummary): string {
  const parts: string[] = [];
  if (s.done) parts.push(`${s.done} done`);
  if (s.skipped) parts.push(`${s.skipped} saved for later`);
  const min = Math.round(s.durationMs / 60_000);
  const took = min < 1 ? "in under a minute" : min === 1 ? "in a minute" : `in ${min} minutes`;
  return `${parts.join(", ")}, ${took}.`;
}

export interface WalkState {
  walk: Walk | null;
  finished: WalkSummary | null;
}

export class WalkStore {
  private state: WalkState = { walk: null, finished: null };
  private listeners = new Set<() => void>();
  constructor(private readonly now: () => number = () => Date.now()) {}

  subscribe = (l: () => void) => {
    this.listeners.add(l);
    return () => this.listeners.delete(l);
  };
  snapshot = () => this.state;

  start(eyes: EyesItem[], highlighted?: EyesItem): EyesItem | undefined {
    const w = startWalk(eyes, highlighted, this.now());
    this.set({ walk: w, finished: null });
    return w ? current(w) : undefined;
  }

  /** Returns the next email, or undefined when the walk just finished. */
  step(kind: Step): EyesItem | undefined {
    const w = this.state.walk;
    if (!w) return undefined;
    const r = step(w, kind);
    if (r.finished) {
      this.set({ walk: null, finished: summarize(r.walk, this.now()) });
      return undefined;
    }
    this.set({ ...this.state, walk: r.walk });
    return current(r.walk);
  }

  move(by: 1 | -1): EyesItem | undefined {
    const w = this.state.walk;
    if (!w) return undefined;
    const next = move(w, by);
    if (next !== w) this.set({ ...this.state, walk: next });
    return current(next);
  }

  /** Returns the email to show after the rewind, when the walk is still on. */
  rewind(kind: Step, itemId: number): EyesItem | undefined {
    const w = this.state.walk;
    if (!w) return undefined;
    const next = rewind(w, kind, itemId);
    this.set({ ...this.state, walk: next });
    return current(next);
  }

  /** Leave the walk early (Esc); the tally so far still counts. */
  end(): void {
    const w = this.state.walk;
    this.set({ walk: null, finished: w ? summarize(w, this.now()) : null });
  }

  dismissSummary(): void {
    if (this.state.finished) this.set({ ...this.state, finished: null });
  }

  private set(s: WalkState) {
    this.state = s;
    this.listeners.forEach((l) => l());
  }
}

export function useWalk(store: WalkStore): WalkState {
  return useSyncExternalStore(store.subscribe, store.snapshot);
}
