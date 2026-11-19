// LIVE UPDATES: the daemon's event stream (`/client/events`) tells the app the
// moment something changes, so the board refreshes then instead of on a timer.
//
// ONE STREAM PER BROWSER, NOT PER TAB. The tab holding the Web Lock runs the
// stream and announces each change on a BroadcastChannel; the other tabs
// listen. When the leading tab closes, the lock passes to another, which picks
// up from the shared cursor. Without Web Locks every tab streams for itself.
//
// Polling stays as the fallback: every 30 seconds while the stream is down,
// every five minutes while it is live (in case an event was missed).
import { useSyncExternalStore } from "react";
import type { Transport } from "../api/transport";
import type { Consumed, WireEvent } from "../live/sse";
import {
  EventStream,
  attachLifecycle,
  localStorageCursor,
  type CursorStore,
  type StreamStatus,
} from "../live/stream";

export const POLL_WHEN_DOWN_MS = 30_000;
export const POLL_WHEN_LIVE_MS = 5 * 60_000;
/** The daemon emits a moment before its lists settle: refresh soon, then once more. */
export const REFRESH_AFTER_MS = [300, 1600] as const;

const CHANNEL = "passband-live";

/** Kinds that mean "something new needs you" (squelch_core EventKind). */
export function worthMentioning(e: WireEvent): boolean {
  return e.kind === "urgent" || e.kind === "deadline" || e.kind === "surfaced";
}

/** Sign-in codes and other sealed mail are never named in a toast. */
export function announce(e: WireEvent): string {
  if (e.is_auth || e.sealed_kind) return "New sign-in mail. It opens in the Passband app.";
  const who = e.sender.replace(/\s*<[^>]*>\s*$/, "").trim() || "Someone";
  return `New for you · ${who}`;
}

type Message =
  | { type: "changed"; event?: WireEvent }
  /** The leading tab's stream state, so the other tabs poll at the right pace. */
  | { type: "status"; status: StreamStatus };

/** Event ids remembered for dedupe; old ones can no longer be replayed. */
const SEEN_CAP = 500;

export interface LiveOptions {
  transport: Transport;
  /** Something changed: refresh what is on screen. */
  onChange: () => void;
  /** New mail that needs the person, once per event across all tabs. */
  onArrival?: (e: WireEvent) => void;
}

/**
 * The cursor for this server. Merely touching `localStorage` throws where site
 * storage is blocked, so fall back to a cursor that lives as long as the tab.
 */
export function cursorFor(server: string): CursorStore {
  try {
    const storage = globalThis.localStorage;
    if (storage) return localStorageCursor(server, storage);
  } catch {
    /* blocked: fall through */
  }
  let id: number | null = null;
  return {
    load: () => id,
    save: (n) => {
      id = n;
    },
  };
}

export class Live {
  private status: StreamStatus = "stopped";
  private listeners = new Set<() => void>();
  private stopFns: (() => void)[] = [];
  private timers: ReturnType<typeof setTimeout>[] = [];
  private seen = new Set<number>();

  constructor(private readonly opts: LiveOptions) {}

  subscribe = (l: () => void) => {
    this.listeners.add(l);
    return () => this.listeners.delete(l);
  };
  snapshot = () => this.status;

  /** Fallback poll interval for the screens, given the stream's state. */
  pollMs(): number {
    return this.status === "open" ? POLL_WHEN_LIVE_MS : POLL_WHEN_DOWN_MS;
  }

  start(): void {
    const server = this.opts.transport.server();
    if (!server || this.stopFns.length) return;

    const channel = typeof BroadcastChannel === "undefined" ? null : new BroadcastChannel(CHANNEL);
    // Posting to a channel already closed by stop() throws; nothing is lost.
    const post = (m: Message) => {
      try {
        channel?.postMessage(m);
      } catch {
        /* closed */
      }
    };
    if (channel) {
      channel.onmessage = (m: MessageEvent<Message>) => {
        if (m.data?.type === "changed") this.changed(m.data.event);
        else if (m.data?.type === "status") this.setStatus(m.data.status);
      };
      this.stopFns.push(() => channel.close());
    }

    const lead = () => {
      const stream = new EventStream({
        open: (after, signal) =>
          this.opts.transport.request("/client/events", {
            query: { after: after ?? undefined },
            signal,
            timeoutMs: 15_000,
          }),
        cursor: cursorFor(server),
        onConsumed: (c: Consumed) => {
          if (c.type !== "event") return;
          post({ type: "changed", event: c.event });
          this.changed(c.event);
        },
        onStatus: (s) => {
          this.setStatus(s);
          post({ type: "status", status: s });
        },
      });
      stream.start();
      // Reconnect at once on coming back (visible, resumed, online).
      const detach = typeof window === "undefined" ? () => {} : attachLifecycle(stream, window);
      return () => {
        detach();
        void stream.stop();
      };
    };

    const locks = typeof navigator !== "undefined" ? navigator.locks : undefined;
    if (locks) {
      const release = new AbortController();
      let stopLeading: (() => void) | null = null;
      // Waits for the lock; holds it (and the stream) until this Live stops.
      void locks
        .request(`${CHANNEL}:${server}`, { signal: release.signal }, () => {
          stopLeading = lead();
          return new Promise<void>((resolve) => release.signal.addEventListener("abort", () => resolve()));
        })
        .catch(() => {
          /* aborted while waiting: this tab never led */
        });
      this.stopFns.push(() => {
        stopLeading?.();
        release.abort();
      });
    } else {
      this.stopFns.push(lead());
    }
  }

  stop(): void {
    // Last started, first stopped: the stream before the channel it reports on.
    for (const f of this.stopFns.splice(0).reverse()) f();
    for (const t of this.timers.splice(0)) clearTimeout(t);
    this.setStatus("stopped");
  }

  private changed(event: WireEvent | undefined) {
    for (const t of this.timers.splice(0)) clearTimeout(t);
    for (const ms of REFRESH_AFTER_MS) this.timers.push(setTimeout(() => this.opts.onChange(), ms));
    // Dedupe by id: a replay across the reconnect seam, or the same event
    // heard locally and over the channel, is announced once.
    if (!event || this.seen.has(event.id)) return;
    this.seen.add(event.id);
    if (this.seen.size > SEEN_CAP) this.seen.delete(this.seen.values().next().value as number);
    if (worthMentioning(event)) this.opts.onArrival?.(event);
  }

  private setStatus(s: StreamStatus) {
    if (s === this.status) return;
    this.status = s;
    this.listeners.forEach((l) => l());
  }
}

export function useLiveStatus(live: Live): StreamStatus {
  return useSyncExternalStore(live.subscribe, live.snapshot);
}
