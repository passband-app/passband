// What the search field is asking and what came back (the Mac's SearchSession
// and SearchFetch, for one field). The rules that matter:
//
//   - one request per typing run: the fetch waits out a short debounce;
//   - a newer keystroke aborts the request in flight, and an answer for any
//     query but the live one is dropped, so an older response can never
//     overwrite a newer one;
//   - a field that is only operators with no value (`from:`) is not a search
//     yet: the daemon would refuse it with a 400, so nothing is asked;
//   - senders are offered above the hits for plain words or a `from:` being
//     typed, from their own (shorter) debounce, and a held row the newest
//     keystroke ruled out is dropped rather than shown armed.
import type { Client } from "../api/client";
import { ApiError } from "../api/transport";
import type { SearchHit, SenderHit } from "../api/wire";
import { awaitingValue, fragment, matches, searchOrder, senderFragment } from "./fromOperator";
import { cleanPreview } from "./preview";

export const DEBOUNCE_MS = 220;
export const SENDER_DEBOUNCE_MS = 120;
export const PAGE_SIZE = 50;
export const SENDER_ROWS = 3;

export interface ShownHit extends SearchHit {
  /** The excerpt, cleaned once per page rather than per render. */
  preview: string;
}

export type SearchStatus = "idle" | "waiting" | "loading" | "done" | "error";

export interface SearchState {
  query: string;
  status: SearchStatus;
  /** The query the hits on screen answer (null: none answered yet). */
  answered: string | null;
  hits: ShownHit[];
  nextCursor?: string;
  indexedSince?: string;
  senders: SenderHit[];
  /** True when the senders shown are the `from:` menu, not an offer above hits. */
  senderMenu: boolean;
  signedOut: boolean;
}

const EMPTY: SearchState = {
  query: "",
  status: "idle",
  answered: null,
  hits: [],
  senders: [],
  senderMenu: false,
  signedOut: false,
};

function shown(h: SearchHit): ShownHit {
  return { ...h, preview: cleanPreview(h.snippet) };
}

/** The senders to look up for this query, and whether they are the `from:` menu. */
export function senderAsk(query: string): { q: string; menu: boolean } | null {
  const f = fragment(query);
  if (f !== null) return { q: f, menu: true };
  const s = senderFragment(query);
  return s === null ? null : { q: s, menu: false };
}

export class SearchSession {
  private state: SearchState = EMPTY;
  private listeners = new Set<() => void>();
  private timer: ReturnType<typeof setTimeout> | undefined;
  private senderTimer: ReturnType<typeof setTimeout> | undefined;
  private ctl: AbortController | undefined;
  private senderCtl: AbortController | undefined;

  constructor(
    private readonly client: Pick<Client, "search" | "senders">,
    /** Plain ranked order (`search_plain_order`); every current daemon has it. */
    private readonly plainOrder = true,
  ) {}

  subscribe = (l: () => void) => {
    this.listeners.add(l);
    return () => this.listeners.delete(l);
  };
  snapshot = () => this.state;

  setQuery(query: string): void {
    clearTimeout(this.timer);
    clearTimeout(this.senderTimer);
    this.ctl?.abort();
    this.senderCtl?.abort();

    const trimmed = query.trim();
    const ask = senderAsk(query);
    // Held sender rows stay only while they still answer the live field.
    const held = ask
      ? this.state.senders.filter((s) => ask.menu || matches(s.addr, s.display_name, ask.q))
      : [];

    if (trimmed === "") {
      this.set({ ...EMPTY });
      return;
    }
    const waiting = awaitingValue(query);
    this.set({
      ...this.state,
      query,
      status: waiting ? "waiting" : "loading",
      senders: held,
      senderMenu: ask?.menu ?? false,
      // Hits for another query stay on screen (no flash) until the answer lands.
      signedOut: false,
    });

    if (ask) {
      this.senderTimer = setTimeout(() => void this.fetchSenders(query, ask), SENDER_DEBOUNCE_MS);
    }
    if (!waiting) {
      this.timer = setTimeout(() => void this.fetchPage(query), DEBOUNCE_MS);
    }
  }

  /** The next page of the hits on screen. */
  async more(): Promise<void> {
    const { answered, nextCursor, query } = this.state;
    if (answered === null || answered !== query || !nextCursor) return;
    await this.fetchPage(query, nextCursor);
  }

  dispose(): void {
    clearTimeout(this.timer);
    clearTimeout(this.senderTimer);
    this.ctl?.abort();
    this.senderCtl?.abort();
  }

  private async fetchPage(query: string, cursor?: string): Promise<void> {
    const ctl = new AbortController();
    this.ctl = ctl;
    const order = searchOrder(query, this.plainOrder);
    try {
      const page = await this.client.search(query, {
        limit: PAGE_SIZE,
        cursor,
        mode: "hybrid",
        partial: true,
        unfinishedFirst: order.unfinishedFirst,
        includeSent: order.includeSent,
        signal: ctl.signal,
      });
      if (ctl.signal.aborted || this.state.query !== query) return;
      const hits = page.items.map(shown);
      this.set({
        ...this.state,
        status: "done",
        answered: query,
        hits: cursor ? [...this.state.hits, ...hits] : hits,
        nextCursor: page.next_cursor,
        indexedSince: page.indexed_since ?? this.state.indexedSince,
      });
    } catch (e) {
      if (ctl.signal.aborted || this.state.query !== query) return;
      const signedOut = e instanceof ApiError && (e.kind === "unauthorized" || e.kind === "signed_out");
      // A 400 is the daemon refusing a query with nothing searchable in it:
      // an empty answer, not an outage.
      const refused = e instanceof ApiError && e.status === 400;
      this.set({
        ...this.state,
        status: refused ? "done" : "error",
        answered: refused ? query : null,
        hits: refused ? [] : this.state.hits,
        nextCursor: undefined,
        signedOut,
      });
    }
  }

  private async fetchSenders(query: string, ask: { q: string; menu: boolean }): Promise<void> {
    const ctl = new AbortController();
    this.senderCtl = ctl;
    try {
      const found = await this.client.senders(ask.q, ask.menu ? 6 : SENDER_ROWS, ctl.signal);
      if (ctl.signal.aborted || this.state.query !== query) return;
      // The menu lists who the daemon matched; the offer re-checks the live field.
      const keep = ask.menu ? found : found.filter((s) => matches(s.addr, s.display_name, ask.q));
      this.set({ ...this.state, senders: keep, senderMenu: ask.menu });
    } catch {
      // The offer is a convenience; the search under it reports its own errors.
      if (ctl.signal.aborted || this.state.query !== query) return;
      this.set({ ...this.state, senders: [] });
    }
  }

  private set(s: SearchState) {
    this.state = s;
    this.listeners.forEach((l) => l());
  }
}
