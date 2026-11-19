// The two lists the web client shows, and the verbs that change them.
//
//   NEEDS YOU: the daemon's `standing` band (dated obligations and live
//   correspondence, never rotating out until resolved) plus `new` mail that
//   triage did not file as noise.
//   EVERYTHING ELSE: the most recent mail that is not already above.
//
// Verbs follow the Mac client's Model/Actions.swift: the row leaves at once,
// the call follows, a failure puts the row back, and a success offers a
// five-second undo (done -> back to open; archive -> the INBOX label back).
import { useSyncExternalStore } from "react";
import { INBOX_LABEL, type Client } from "../api/client";
import { ApiError } from "../api/transport";
import type { AttentionUpdate, Tier } from "../api/wire";
import { parseInstant } from "../lib/time";
import { toasts, type ToastStore } from "./toasts";

const TIER_RANK: Record<Tier, number> = { past_due: 0, deadline: 1, signal: 2, noise: 3 };

const arrival = (u: AttentionUpdate) => parseInstant(u.received_at) ?? 0;

/** Overdue, then due, then by importance, then newest. Pure, for tests. */
export function needsYouOrder(a: AttentionUpdate, b: AttentionUpdate): number {
  return (
    TIER_RANK[a.tier] - TIER_RANK[b.tier] ||
    b.importance - a.importance ||
    arrival(b) - arrival(a) ||
    b.id - a.id
  );
}

/** Build both lists from the three fetches. Pure, for tests. */
export function composeLists(
  standing: AttentionUpdate[],
  fresh: AttentionUpdate[],
  recent: AttentionUpdate[],
): { needsYou: AttentionUpdate[]; rest: AttentionUpdate[] } {
  const seen = new Set<number>();
  const needsYou: AttentionUpdate[] = [];
  for (const u of [...standing, ...fresh.filter((u) => u.tier !== "noise")]) {
    if (u.status === "done" || seen.has(u.id)) continue;
    seen.add(u.id);
    needsYou.push(u);
  }
  needsYou.sort(needsYouOrder);
  const rest = recent
    .filter((u) => u.status !== "done" && !seen.has(u.id))
    .sort((a, b) => arrival(b) - arrival(a) || b.id - a.id);
  return { needsYou, rest };
}

export interface InboxState {
  needsYou: AttentionUpdate[];
  rest: AttentionUpdate[];
  account?: string;
  /** Nothing has loaded yet. */
  loading: boolean;
  /** The last refresh failed; what is shown (if anything) is stale. */
  stale: boolean;
  signedOut: boolean;
}

const RECENT_LIMIT = 50;

export class Inbox {
  private state: InboxState = { needsYou: [], rest: [], loading: true, stale: false, signedOut: false };
  private listeners = new Set<() => void>();
  private inflight: Promise<void> | null = null;
  private again: Promise<void> | null = null;
  /** Rows removed optimistically, kept out of refreshes until their call settles. */
  private hidden = new Set<number>();

  constructor(
    private readonly client: Client,
    private readonly toastStore: ToastStore = toasts,
  ) {}

  subscribe = (l: () => void) => {
    this.listeners.add(l);
    return () => this.listeners.delete(l);
  };
  snapshot = () => this.state;

  /** One refresh at a time: a manual refresh joins a scheduled one in flight. */
  /**
   * One load at a time. A refresh asked for DURING a load runs once more after
   * it, rather than joining it: that load may have read the mailbox before the
   * change the caller is refreshing for (an undo, a live event) had landed.
   * Any number of such requests coalesce into that one follow-up load.
   */
  refresh(): Promise<void> {
    if (this.inflight) {
      this.again ??= this.inflight.then(() => {
        this.again = null;
        return this.refresh();
      });
      return this.again;
    }
    this.inflight = this.load().finally(() => {
      this.inflight = null;
    });
    return this.inflight;
  }

  private async load(): Promise<void> {
    try {
      const [stats, standing, fresh, recent] = await Promise.all([
        this.client.stats(),
        this.client.updates({ band: "standing", limit: 100 }),
        this.client.updates({ band: "new", limit: 100 }),
        this.client.updates({ limit: RECENT_LIMIT }),
      ]);
      const lists = composeLists(standing.items, fresh.items, recent.items);
      const keep = (u: AttentionUpdate) => !this.hidden.has(u.id);
      this.set({
        needsYou: lists.needsYou.filter(keep),
        rest: lists.rest.filter(keep),
        account: stats.account_email,
        loading: false,
        stale: false,
        signedOut: false,
      });
    } catch (e) {
      const signedOut =
        e instanceof ApiError && (e.kind === "unauthorized" || e.kind === "signed_out");
      this.set({ ...this.state, loading: false, stale: !signedOut, signedOut });
    }
  }

  find(id: number): AttentionUpdate | undefined {
    return this.state.needsYou.find((u) => u.id === id) ?? this.state.rest.find((u) => u.id === id);
  }

  async done(u: AttentionUpdate): Promise<boolean> {
    return this.verb(
      u,
      () => this.client.setStatus(u.id, "done"),
      `done · ${u.from_name ?? u.sender}`,
      () => this.client.setStatus(u.id, "open").then(() => undefined),
      "done failed",
    );
  }

  async archive(u: AttentionUpdate): Promise<boolean> {
    return this.verb(
      u,
      () => this.client.archive(u.id),
      `archived · ${u.from_name ?? u.sender}`,
      () => this.client.label(u.id, [INBOX_LABEL]),
      "archive failed",
    );
  }

  private async verb(
    u: AttentionUpdate,
    call: () => Promise<unknown>,
    label: string,
    revert: () => Promise<void>,
    failure: string,
  ): Promise<boolean> {
    const restore = this.remove(u.id);
    try {
      await call();
    } catch (e) {
      restore();
      const forbidden = e instanceof ApiError && e.kind === "forbidden";
      this.toastStore.push(forbidden ? "this mailbox can't be changed from here yet" : failure, "error");
      return false;
    }
    this.toastStore.push(label, "info", async () => {
      await revert();
      this.hidden.delete(u.id);
      await this.refresh();
    });
    // The row stays hidden until the next refresh no longer lists it.
    void this.refresh().then(() => this.hidden.delete(u.id));
    return true;
  }

  /** Take a row out of both lists; returns a thunk that puts it back where it was. */
  private remove(id: number): () => void {
    const prev = this.state;
    this.hidden.add(id);
    this.set({
      ...prev,
      needsYou: prev.needsYou.filter((u) => u.id !== id),
      rest: prev.rest.filter((u) => u.id !== id),
    });
    return () => {
      this.hidden.delete(id);
      const cur = this.state;
      const was = prev.needsYou.find((u) => u.id === id);
      const wasRest = prev.rest.find((u) => u.id === id);
      this.set({
        ...cur,
        needsYou: was ? [...cur.needsYou, was].sort(needsYouOrder) : cur.needsYou,
        rest: wasRest
          ? [...cur.rest, wasRest].sort((a, b) => arrival(b) - arrival(a) || b.id - a.id)
          : cur.rest,
      });
    };
  }

  private set(s: InboxState) {
    this.state = s;
    this.listeners.forEach((l) => l());
  }
}

export function useInbox(inbox: Inbox): InboxState {
  return useSyncExternalStore(inbox.subscribe, inbox.snapshot);
}
