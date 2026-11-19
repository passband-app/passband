import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { ApiError } from "../src/api/transport";
import type { SearchPage, SenderHit } from "../src/api/wire";
import { DEBOUNCE_MS, SENDER_DEBOUNCE_MS, SearchSession, senderAsk } from "../src/search/session";

const hit = (id: number, subject = `s${id}`) => ({
  id,
  thread_id: `t${id}`,
  from_addr: "a@example.com",
  subject,
  received_at: "2026-11-18T10:00:00Z",
  snippet: "Read https://www.example.com/news?utm=x",
});

type Call = { q: string; signal?: AbortSignal; resolve: (p: SearchPage) => void; reject: (e: unknown) => void };

function fake() {
  const calls: Call[] = [];
  const senderCalls: string[] = [];
  const client = {
    search: vi.fn(
      (q: string, opts: { signal?: AbortSignal } = {}) =>
        new Promise<SearchPage>((resolve, reject) => calls.push({ q, signal: opts.signal, resolve, reject })),
    ),
    senders: vi.fn(async (q: string): Promise<SenderHit[]> => {
      senderCalls.push(q);
      return [{ addr: "dan@example.com", display_name: "Dan", msg_count: 3 }];
    }),
  };
  return { client, calls, senderCalls };
}

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

describe("SearchSession", () => {
  test("one request per typing run", async () => {
    const { client, calls } = fake();
    const s = new SearchSession(client);
    s.setQuery("o");
    s.setQuery("of");
    s.setQuery("offs");
    await vi.advanceTimersByTimeAsync(DEBOUNCE_MS);
    expect(calls.map((c) => c.q)).toEqual(["offs"]);
  });

  test("an older answer never overwrites a newer one", async () => {
    const { client, calls } = fake();
    const s = new SearchSession(client);
    s.setQuery("off");
    await vi.advanceTimersByTimeAsync(DEBOUNCE_MS);
    s.setQuery("offsite");
    expect(calls[0]!.signal?.aborted).toBe(true);
    await vi.advanceTimersByTimeAsync(DEBOUNCE_MS);
    calls[1]!.resolve({ items: [hit(2, "Offsite moved")] });
    await vi.runAllTimersAsync();
    // The aborted request answers late: ignored.
    calls[0]!.resolve({ items: [hit(1, "Offer")] });
    await vi.runAllTimersAsync();
    expect(s.snapshot().hits.map((h) => h.id)).toEqual([2]);
    expect(s.snapshot().answered).toBe("offsite");
  });

  test("excerpts are cleaned once, on arrival", async () => {
    const { client, calls } = fake();
    const s = new SearchSession(client);
    s.setQuery("news");
    await vi.advanceTimersByTimeAsync(DEBOUNCE_MS);
    calls[0]!.resolve({ items: [hit(1)] });
    await vi.runAllTimersAsync();
    expect(s.snapshot().hits[0]?.preview).toBe("Read example.com/news");
  });

  test("operators with no value are not a search yet", async () => {
    const { client, calls } = fake();
    const s = new SearchSession(client);
    s.setQuery("from:");
    await vi.advanceTimersByTimeAsync(DEBOUNCE_MS);
    expect(calls).toHaveLength(0);
    expect(s.snapshot().status).toBe("waiting");
  });

  test("an emptied field clears everything", async () => {
    const { client, calls } = fake();
    const s = new SearchSession(client);
    s.setQuery("wifi");
    await vi.advanceTimersByTimeAsync(DEBOUNCE_MS);
    calls[0]!.resolve({ items: [hit(1)] });
    await vi.runAllTimersAsync();
    s.setQuery("  ");
    expect(s.snapshot()).toMatchObject({ query: "", hits: [], status: "idle", answered: null });
  });

  test("a 400 is an empty answer; anything else is an error that keeps the hits", async () => {
    const { client, calls } = fake();
    const s = new SearchSession(client);
    s.setQuery("wifi");
    await vi.advanceTimersByTimeAsync(DEBOUNCE_MS);
    calls[0]!.resolve({ items: [hit(1)] });
    await vi.runAllTimersAsync();
    s.setQuery("wifi password");
    await vi.advanceTimersByTimeAsync(DEBOUNCE_MS);
    calls[1]!.reject(new ApiError("network"));
    await vi.runAllTimersAsync();
    expect(s.snapshot()).toMatchObject({ status: "error", answered: null });
    expect(s.snapshot().hits).toHaveLength(1);

    s.setQuery("after:x");
    await vi.advanceTimersByTimeAsync(DEBOUNCE_MS);
    calls[2]!.reject(new ApiError("http", 400));
    await vi.runAllTimersAsync();
    expect(s.snapshot()).toMatchObject({ status: "done", answered: "after:x", hits: [] });
  });

  test("a 401 reports signed out", async () => {
    const { client, calls } = fake();
    const s = new SearchSession(client);
    s.setQuery("wifi");
    await vi.advanceTimersByTimeAsync(DEBOUNCE_MS);
    calls[0]!.reject(new ApiError("unauthorized", 401));
    await vi.runAllTimersAsync();
    expect(s.snapshot().signedOut).toBe(true);
  });

  test("senders are offered for plain words and for a from: being typed", async () => {
    const { client, senderCalls } = fake();
    const s = new SearchSession(client);
    s.setQuery("dan");
    await vi.advanceTimersByTimeAsync(SENDER_DEBOUNCE_MS);
    expect(senderCalls).toEqual(["dan"]);
    expect(s.snapshot()).toMatchObject({ senderMenu: false });
    expect(s.snapshot().senders).toHaveLength(1);
    // A keystroke the held row no longer answers drops it at once.
    s.setQuery("dana");
    expect(s.snapshot().senders).toHaveLength(0);
    s.setQuery("from:");
    await vi.advanceTimersByTimeAsync(SENDER_DEBOUNCE_MS);
    expect(senderCalls.at(-1)).toBe("");
    expect(s.snapshot().senderMenu).toBe(true);
  });

  test("paging appends to the hits on screen", async () => {
    const { client, calls } = fake();
    const s = new SearchSession(client);
    s.setQuery("sale");
    await vi.advanceTimersByTimeAsync(DEBOUNCE_MS);
    calls[0]!.resolve({ items: [hit(1)], next_cursor: "c1" });
    await vi.runAllTimersAsync();
    const more = s.more();
    calls[1]!.resolve({ items: [hit(2)] });
    await more;
    expect(s.snapshot().hits.map((h) => h.id)).toEqual([1, 2]);
    expect(s.snapshot().nextCursor).toBeUndefined();
  });
});

describe("senderAsk", () => {
  test("the from: menu, the plain-word offer, or nothing", () => {
    expect(senderAsk("from:da")).toEqual({ q: "da", menu: true });
    expect(senderAsk("dan smith")).toEqual({ q: "dan smith", menu: false });
    expect(senderAsk("invoice from:dan ")).toBeNull();
    expect(senderAsk("abstract conference wifi password")).toBeNull();
  });
});
