import { afterEach, describe, expect, test, vi } from "vitest";
import type { RequestOptions, Transport } from "../src/api/transport";
import {
  Live,
  POLL_WHEN_DOWN_MS,
  POLL_WHEN_LIVE_MS,
  REFRESH_AFTER_MS,
  announce,
  worthMentioning,
} from "../src/app/live";
import type { WireEvent } from "../src/live/sse";

const ev = (id: number, over: Partial<WireEvent> = {}): WireEvent => ({
  id,
  kind: "surfaced",
  message_id: id,
  thread_id: `t${id}`,
  tier: "signal",
  importance: 3,
  sender: "Lakeside Dental <frontdesk@lakeside.example>",
  one_line: "x",
  deadline: null,
  created_at: "2026-11-19T10:00:00Z",
  ...over,
});

describe("what gets announced", () => {
  test("new mail that needs you, not opens", () => {
    expect(worthMentioning(ev(1))).toBe(true);
    expect(worthMentioning(ev(1, { kind: "urgent" }))).toBe(true);
    expect(worthMentioning(ev(1, { kind: "opened" }))).toBe(false);
  });
  test("names the sender, never an address", () => {
    expect(announce(ev(1))).toBe("New for you · Lakeside Dental");
  });
  test("sign-in mail is never named", () => {
    expect(announce(ev(1, { is_auth: true }))).not.toContain("Lakeside");
    expect(announce(ev(1, { sealed_kind: "otp" }))).toMatch(/opens in the Passband app/);
  });
});

function sse(events: WireEvent[]): Response {
  const body = events.map((e) => `id: ${e.id}\ndata: ${JSON.stringify(e)}\n\n`).join("");
  return new Response(new Blob([body]).stream(), { status: 200 });
}

function fakeTransport(respond: (opts: RequestOptions) => Promise<Response>) {
  const calls: RequestOptions[] = [];
  const t: Transport = {
    server: () => "https://tenant.passband.example",
    request: async (path, opts = {}) => {
      expect(path).toBe("/client/events");
      calls.push(opts);
      return respond(opts);
    },
    pair: async () => ({ deviceName: "x" }),
    forget: async () => {},
  };
  return { t, calls };
}

describe("Live", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  test("an event refreshes twice, announces once, and a replay is quiet", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    let n = 0;
    const { t } = fakeTransport(async (opts) => {
      n++;
      if (n <= 2) return sse([ev(7)]); // the same event twice: a replay across the seam
      return new Promise<Response>((_r, reject) =>
        opts.signal?.addEventListener("abort", () => reject(new Error("aborted"))),
      );
    });
    const onChange = vi.fn();
    const onArrival = vi.fn();
    const live = new Live({ transport: t, onChange, onArrival });
    live.start();
    await vi.advanceTimersByTimeAsync(5_000);
    expect(onArrival).toHaveBeenCalledTimes(1);
    expect(onChange.mock.calls.length).toBeGreaterThanOrEqual(REFRESH_AFTER_MS.length);
    live.stop();
  });

  test("resumes from the cursor it saved", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    let n = 0;
    const { t, calls } = fakeTransport(async (opts) => {
      n++;
      if (n === 1) return sse([ev(41), ev(42)]);
      return new Promise<Response>((_r, reject) =>
        opts.signal?.addEventListener("abort", () => reject(new Error("aborted"))),
      );
    });
    const live = new Live({ transport: t, onChange: () => {} });
    live.start();
    await vi.advanceTimersByTimeAsync(5_000);
    live.stop();
    expect(calls[0]?.query?.after).toBeUndefined();
    expect(calls[1]?.query?.after).toBe(42);
  });

  test("polls slowly while the stream is open, quickly while it is down", async () => {
    const { t } = fakeTransport(
      (opts) =>
        new Promise<Response>((resolve) => {
          // A stream that stays open with nothing to say.
          resolve(new Response(new ReadableStream({ start() {} }), { status: 200 }));
          void opts;
        }),
    );
    const live = new Live({ transport: t, onChange: () => {} });
    expect(live.pollMs()).toBe(POLL_WHEN_DOWN_MS);
    live.start();
    await new Promise((r) => setTimeout(r, 20));
    expect(live.snapshot()).toBe("open");
    expect(live.pollMs()).toBe(POLL_WHEN_LIVE_MS);
    live.stop();
    expect(live.pollMs()).toBe(POLL_WHEN_DOWN_MS);
  });
});
