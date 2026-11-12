// The resident feed: one long-lived fetch() stream to GET /client/events,
// ported from EventStream.swift's connection loop. Differences forced by the
// browser are marked WEB.

import { LineOverflow, LineSplitter, SSEParser, consume, type Consumed } from "./sse.js";

/** Exponential backoff with full jitter over the lower half of the window (Lib/Concurrency.swift Backoff). */
export class Backoff {
  private window: number;
  constructor(
    private readonly base: number,
    private readonly cap: number,
    private readonly random: () => number = Math.random,
  ) {
    this.window = base;
  }
  reset() {
    this.window = this.base;
  }
  /** The current window's jittered delay (seconds), widening the window toward the cap. */
  next(): number {
    const lo = this.window / 2;
    const delay = lo + this.random() * (this.window - lo);
    this.window = Math.min(this.window * 2, this.cap);
    return delay;
  }
}

/** Where the cursor lives. Per account: event ids are one daemon's SQLite ints. */
export interface CursorStore {
  /** null = never connected: connect cursorless (live only), never after=0. */
  load(): number | null;
  save(id: number): void;
}

export function localStorageCursor(accountId: string, storage: Storage = localStorage): CursorStore {
  const key = `passband.events.lastSeen.${accountId}`;
  return {
    load() {
      try {
        const raw = storage.getItem(key);
        if (raw === null || !/^\d+$/.test(raw)) return null;
        return Number(raw);
      } catch {
        return null;
      }
    },
    save(id) {
      try {
        storage.setItem(key, String(id));
      } catch {
        /* private mode / quota: the in-memory cursor still holds for this tab */
      }
    },
  };
}

export const timing = {
  backoffBase: 1,
  backoffCap: 60,
  /** A connection that lived this long was real: its failure restarts the backoff. */
  healthyAfter: 30,
  /** Inactivity watchdog (WEB: fetch has none of its own). Server pings every 15s; 4 missed = dead. */
  inactivityTimeout: 60,
};

export type StreamStatus = "connecting" | "open" | "waiting" | "stopped";

export interface EventStreamOptions {
  serverUrl: string;
  token: string;
  cursor: CursorStore;
  onConsumed: (c: Consumed) => void;
  onStatus?: (s: StreamStatus, detail?: string) => void;
  fetchImpl?: typeof fetch;
  /** Injected for tests. */
  now?: () => number;
  sleep?: (ms: number, signal: AbortSignal) => Promise<void>;
  random?: () => number;
}

const defaultSleep = (ms: number, signal: AbortSignal) =>
  new Promise<void>((resolve) => {
    if (signal.aborted) return resolve();
    const t = setTimeout(resolve, ms);
    signal.addEventListener("abort", () => {
      clearTimeout(t);
      resolve();
    }, { once: true });
  });

export class EventStream {
  private stopped = true;
  private loop: Promise<void> | null = null;
  /** Aborts the live connection (or the backoff nap) without stopping the loop. */
  private kick = new AbortController();
  private stopCtl = new AbortController();
  private cursor: number | null | undefined = undefined;
  private readonly fetchImpl: typeof fetch;
  private readonly now: () => number;
  private readonly sleep: (ms: number, signal: AbortSignal) => Promise<void>;
  private readonly backoff: Backoff;
  /** Bumped when a reconnect should skip the nap (tab visible again, back online). */
  private skipNap = false;
  connections = 0;

  constructor(private readonly opts: EventStreamOptions) {
    this.fetchImpl = opts.fetchImpl ?? fetch.bind(globalThis);
    this.now = opts.now ?? (() => Date.now());
    this.sleep = opts.sleep ?? defaultSleep;
    this.backoff = new Backoff(timing.backoffBase, timing.backoffCap, opts.random);
  }

  get lastSeen(): number | null {
    return this.loadCursor();
  }

  start() {
    if (!this.stopped) return;
    this.stopped = false;
    this.stopCtl = new AbortController();
    this.loop = this.run();
  }

  async stop() {
    this.stopped = true;
    this.stopCtl.abort();
    this.kick.abort();
    await this.loop;
    this.opts.onStatus?.("stopped");
  }

  /**
   * WEB: reconnect now, skipping any backoff nap. Called when the tab becomes
   * visible again, when it is resumed from a frozen state, and on `online`. A
   * live connection is dropped and rebuilt only when `force` (it may be a
   * zombie the inactivity watchdog has not caught yet); otherwise only a nap
   * is cut short. Either way the cursor makes the catch-up exact.
   */
  poke(force = false) {
    if (this.stopped) return;
    if (!force && this.state === "open") return;
    this.skipNap = true;
    this.kick.abort();
  }

  private state: StreamStatus = "stopped";
  private status(s: StreamStatus, detail?: string) {
    this.state = s;
    this.opts.onStatus?.(s, detail);
  }

  private async run() {
    while (!this.stopped) {
      this.kick = new AbortController();
      const opened = this.now();
      await this.connect(this.kick);
      if (this.stopped) return;
      if ((this.now() - opened) / 1000 >= timing.healthyAfter) this.backoff.reset();
      const delay = this.backoff.next();
      if (this.skipNap) {
        this.skipNap = false;
        this.backoff.reset();
        continue;
      }
      this.status("waiting", `${delay.toFixed(1)}s`);
      this.kick = new AbortController();
      await this.sleep(delay * 1000, AbortSignal.any([this.kick.signal, this.stopCtl.signal]));
      this.skipNap = false;
    }
  }

  private buildUrl(): string {
    let base = this.opts.serverUrl;
    while (base.endsWith("/")) base = base.slice(0, -1);
    const after = this.loadCursor();
    // No `opens=1`: that opt-in tells the daemon "a Mac is listening" and asks
    // for draft-open / renotify frames the web client does not act on.
    return after === null ? `${base}/client/events` : `${base}/client/events?after=${after}`;
  }

  /** Hold ONE connection until it ends. Never throws. */
  private async connect(kick: AbortController) {
    this.status("connecting");
    const ctl = new AbortController();
    const onKick = () => ctl.abort();
    kick.signal.addEventListener("abort", onKick, { once: true });
    this.stopCtl.signal.addEventListener("abort", onKick, { once: true });
    let watchdog: ReturnType<typeof setTimeout> | undefined;
    const arm = () => {
      clearTimeout(watchdog);
      watchdog = setTimeout(() => ctl.abort(), timing.inactivityTimeout * 1000);
    };
    try {
      arm();
      const res = await this.fetchImpl(this.buildUrl(), {
        method: "GET",
        headers: {
          Authorization: `Bearer ${this.opts.token}`,
          Accept: "text/event-stream",
          "Cache-Control": "no-store",
        },
        // A 3xx from the feed is a misconfiguration, not a hop to follow with the bearer on it.
        redirect: "error",
        credentials: "omit",
        cache: "no-store",
        referrerPolicy: "no-referrer",
        signal: ctl.signal,
      });
      // 401 / 404 / 5xx are the same from here: back off and retry. The sitrep
      // poller is what tells the human their token is wrong.
      if (res.status !== 200 || !res.body) return;
      this.connections++;
      this.status("open");
      const reader = res.body.getReader();
      const split = new LineSplitter();
      const parser = new SSEParser();
      for (;;) {
        const { value, done } = await reader.read();
        if (done) break;
        arm();
        for (const line of split.push(value)) {
          const frame = parser.feed(line);
          if (!frame) continue;
          const c = consume(frame);
          if (c.type === "event" || c.type === "undecodable") {
            if (c.advanceTo !== null) this.note(c.advanceTo);
          }
          this.opts.onConsumed(c);
        }
      }
    } catch (e) {
      // Refused / DNS / CORS / watchdog / abort / LineOverflow: silent. The
      // error text can embed the server URL; this is a background reconnect.
      void (e instanceof LineOverflow);
    } finally {
      clearTimeout(watchdog);
      kick.signal.removeEventListener("abort", onKick);
      this.stopCtl.signal.removeEventListener("abort", onKick);
      ctl.abort();
    }
  }

  private loadCursor(): number | null {
    if (this.cursor === undefined) this.cursor = this.opts.cursor.load();
    return this.cursor;
  }

  /** MAX, never last-written: a replay overlapping the live seam must not rewind us. */
  private note(id: number) {
    const cur = this.loadCursor();
    if (cur !== null && id <= cur) return;
    this.cursor = id;
    this.opts.cursor.save(id);
  }
}

/**
 * WEB: wire page-lifecycle signals to the stream. A hidden tab keeps its
 * fetch open in Chromium, but a FROZEN or discarded one does not, and a laptop
 * lid closes the socket without a FIN. Coming back, reconnect at once with the
 * cursor rather than wait out the watchdog or a backoff nap.
 */
export function attachLifecycle(stream: EventStream, win: Window & typeof globalThis = window): () => void {
  const onVisible = () => {
    if (win.document.visibilityState === "visible") stream.poke(false);
  };
  const onResume = () => stream.poke(true);
  const onOnline = () => stream.poke(false);
  win.document.addEventListener("visibilitychange", onVisible);
  win.document.addEventListener("resume", onResume);
  win.addEventListener("online", onOnline);
  win.addEventListener("pageshow", onResume);
  return () => {
    win.document.removeEventListener("visibilitychange", onVisible);
    win.document.removeEventListener("resume", onResume);
    win.removeEventListener("online", onOnline);
    win.removeEventListener("pageshow", onResume);
  };
}
