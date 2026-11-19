// The sitrep's state: one load at a time, verbs that act at once and undo, and
// the billing "since you last looked" window the Mac keeps per device.
import { useSyncExternalStore } from "react";
import { INBOX_LABEL, type Client } from "../api/client";
import { ApiError } from "../api/transport";
import { loadSitrep, type Clock } from "../sitrep/derive";
import { EMPTY_SITREP, type EyesItem, type Sitrep } from "../sitrep/model";
import { toasts, type ToastStore } from "./toasts";

export interface SitrepState {
  sitrep: Sitrep;
  loading: boolean;
  stale: boolean;
  signedOut: boolean;
}

const BILLING_SEEN_KEY = "pb_billing_seen_v1";

function readSeen(storage: Pick<Storage, "getItem"> | undefined): number | null {
  try {
    const n = Number(storage?.getItem(BILLING_SEEN_KEY));
    return Number.isFinite(n) && n > 0 ? n : null;
  } catch {
    return null;
  }
}

function writeSeen(storage: Pick<Storage, "setItem"> | undefined, at: number): void {
  try {
    storage?.setItem(BILLING_SEEN_KEY, String(at));
  } catch {
    // Blocked storage: the billing window just stays at its default.
  }
}

export function browserZone(): string {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
  } catch {
    return "UTC";
  }
}

export class SitrepStore {
  private state: SitrepState = { sitrep: EMPTY_SITREP, loading: true, stale: false, signedOut: false };
  private listeners = new Set<() => void>();
  private inflight: Promise<void> | null = null;
  private again: Promise<void> | null = null;
  private hidden = new Set<number>();
  /** The previous visit, fixed for this page load so a refresh does not empty billing. */
  private billingLastSeen: number | null;

  constructor(
    private readonly client: Client,
    private readonly toastStore: ToastStore = toasts,
    private readonly clock: () => Clock = () => ({ now: Date.now(), zone: browserZone() }),
    private readonly storage: Storage | undefined = safeLocalStorage(),
  ) {
    this.billingLastSeen = readSeen(storage);
    writeSeen(storage, Date.now());
  }

  subscribe = (l: () => void) => {
    this.listeners.add(l);
    return () => this.listeners.delete(l);
  };
  snapshot = () => this.state;

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
    const loaded = await loadSitrep(this.client, this.clock(), {
      billingLastSeen: this.billingLastSeen,
    });
    if (loaded.signedOut) {
      this.set({ ...this.state, loading: false, signedOut: true });
      return;
    }
    const failedAll = Object.values(loaded.sources).every((s) => s !== "ok");
    if (failedAll && !this.state.loading) {
      // Nothing came back: keep what is on screen and say it is stale.
      this.set({ ...this.state, stale: true });
      return;
    }
    const sitrep = { ...loaded.sitrep, eyes: loaded.sitrep.eyes.filter((e) => !this.hidden.has(e.id)) };
    this.set({ sitrep, loading: false, stale: false, signedOut: false });
  }

  /** `afterUndo` runs once the undo has landed (flow mode rewinds its walk). */
  done(item: EyesItem, afterUndo?: () => void): Promise<boolean> {
    return this.verb(
      item,
      () => this.client.setStatus(item.id, "done"),
      `done · ${item.sender}`,
      () => this.client.setStatus(item.id, "open").then(() => undefined),
      "done failed",
      afterUndo,
    );
  }

  archive(item: EyesItem, afterUndo?: () => void): Promise<boolean> {
    return this.verb(
      item,
      () => this.client.archive(item.id),
      `archived · ${item.sender}`,
      () => this.client.label(item.id, [INBOX_LABEL]),
      "archive failed",
      afterUndo,
    );
  }

  /** The current row for an id, for surfaces (the reader) that only know the id. */
  find(id: number): EyesItem | undefined {
    return this.state.sitrep.eyes.find((e) => e.id === id);
  }

  private async verb(
    item: EyesItem,
    call: () => Promise<unknown>,
    label: string,
    revert: () => Promise<void>,
    failure: string,
    afterUndo?: () => void,
  ): Promise<boolean> {
    const before = this.state.sitrep.eyes;
    this.hidden.add(item.id);
    this.set({ ...this.state, sitrep: { ...this.state.sitrep, eyes: before.filter((e) => e.id !== item.id) } });
    try {
      await call();
    } catch (e) {
      this.hidden.delete(item.id);
      this.set({ ...this.state, sitrep: { ...this.state.sitrep, eyes: before } });
      const forbidden = e instanceof ApiError && e.kind === "forbidden";
      this.toastStore.push(forbidden ? "this mailbox can't be changed from here yet" : failure, "error");
      return false;
    }
    this.toastStore.push(label, "info", async () => {
      await revert();
      this.hidden.delete(item.id);
      afterUndo?.();
      await this.refresh();
    });
    void this.refresh().then(() => this.hidden.delete(item.id));
    return true;
  }

  private set(s: SitrepState) {
    this.state = s;
    this.listeners.forEach((l) => l());
  }
}

function safeLocalStorage(): Storage | undefined {
  try {
    return typeof localStorage === "undefined" ? undefined : localStorage;
  } catch {
    return undefined;
  }
}

export function useSitrep(store: SitrepStore): SitrepState {
  return useSyncExternalStore(store.subscribe, store.snapshot);
}
