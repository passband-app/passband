// Cases derived from the rules written into EventStream.swift (there is no
// EventStreamTests.swift to copy): each test names the Swift rule it pins.
import { describe, expect, it } from "vitest";
import { LineOverflow, LineSplitter, MAX_FRAME_BYTES, SSEParser, consume, decodeEvent } from "./sse.js";
import { Backoff, EventStream, type CursorStore } from "./stream.js";

const feedAll = (p: SSEParser, lines: string[]) => lines.map((l) => p.feed(l)).filter(Boolean);

const event = (id: number) =>
  JSON.stringify({
    id, kind: "urgent", message_id: id + 100, thread_id: `t${id}`, tier: "signal",
    importance: 80, sender: "alice@example.com", one_line: "Hi", deadline: null,
    created_at: "2026-01-01T00:00:00Z",
  });

describe("SSEParser", () => {
  it("dispatches a frame on the blank line, with its id", () => {
    expect(feedAll(new SSEParser(), ["id: 7", "data: {}", ""])).toEqual([{ id: "7", data: "{}" }]);
  });

  it("keep-alive comments are ignored and are not a frame boundary", () => {
    const p = new SSEParser();
    expect(feedAll(p, ["data: a", ":", ": ping", "data: b", ""])).toEqual([{ id: null, data: "a\nb" }]);
  });

  it("a ping's own blank line dispatches nothing", () => {
    expect(feedAll(new SSEParser(), [":", "", ":", ""])).toEqual([]);
  });

  it("strips the CR a CRLF stream leaves behind", () => {
    expect(feedAll(new SSEParser(), ["id: 1\r", "data: x\r", "\r"])).toEqual([{ id: "1", data: "x" }]);
  });

  it("joins several data lines with LF", () => {
    expect(feedAll(new SSEParser(), ["data: one", "data: two", "data: three", ""])[0]?.data).toBe("one\ntwo\nthree");
  });

  it("strips exactly ONE leading space", () => {
    expect(feedAll(new SSEParser(), ["data:  two spaces", ""])[0]?.data).toBe(" two spaces");
    expect(feedAll(new SSEParser(), ["data:none", ""])[0]?.data).toBe("none");
  });

  it("a bare field name is the field with an empty value", () => {
    expect(feedAll(new SSEParser(), ["data", ""])).toEqual([{ id: null, data: "" }]);
  });

  it("id persists across frames that carry none", () => {
    expect(feedAll(new SSEParser(), ["id: 3", "data: a", "", "data: b", ""])).toEqual([
      { id: "3", data: "a" },
      { id: "3", data: "b" },
    ]);
  });

  it("an id containing NUL is ignored", () => {
    expect(feedAll(new SSEParser(), ["id: 4", "data: a", "", "id: 5\0", "data: b", ""])[1]?.id).toBe("4");
  });

  it("unknown fields (event:, retry:) are skipped", () => {
    expect(feedAll(new SSEParser(), ["event: open_draft", "retry: 10", "data: x", ""])).toEqual([{ id: null, data: "x" }]);
  });

  it("a frame over the byte cap is dropped whole, and the parser recovers", () => {
    const p = new SSEParser();
    const big = "x".repeat(MAX_FRAME_BYTES);
    expect(feedAll(p, ["data: a", `data: ${big}`, "data: b", ""])).toEqual([]);
    expect(feedAll(p, ["data: ok", ""])).toEqual([{ id: null, data: "ok" }]);
  });

  it("the cap counts UTF-8 bytes, not UTF-16 units", () => {
    // 3 bytes per char: just past the cap in bytes, well under it in .length.
    const s = "€".repeat(Math.floor(MAX_FRAME_BYTES / 3) + 1);
    expect(s.length).toBeLessThan(MAX_FRAME_BYTES);
    expect(feedAll(new SSEParser(), [`data: ${s}`, ""])).toEqual([]);
  });
});

describe("LineSplitter", () => {
  const enc = (s: string) => new TextEncoder().encode(s);

  it("keeps empty lines (the frame terminator AsyncLineSequence would drop)", () => {
    expect(new LineSplitter().push(enc("data: a\n\n"))).toEqual(["data: a", ""]);
  });

  it("joins a line split across chunks, including mid-codepoint", () => {
    const s = new LineSplitter();
    const bytes = enc("data: café\n");
    const cut = bytes.indexOf(0xc3) + 1; // inside the two-byte é
    expect(s.push(bytes.subarray(0, cut))).toEqual([]);
    expect(s.push(bytes.subarray(cut))).toEqual(["data: café"]);
  });

  it("a stream that never sends a newline overflows instead of growing forever", () => {
    const s = new LineSplitter();
    const chunk = new Uint8Array(64 * 1024).fill(0x61);
    expect(() => {
      for (let i = 0; i < 5; i++) s.push(chunk);
    }).toThrow(LineOverflow);
  });
});

describe("consume", () => {
  it("an Event advances the cursor to its id", () => {
    const c = consume({ id: "9", data: event(9) });
    expect(c).toMatchObject({ type: "event", advanceTo: 9 });
  });

  it("an undecodable frame with a numeric id STILL advances the cursor", () => {
    expect(consume({ id: "12", data: '{"id":"not-an-int"}' })).toEqual({ type: "undecodable", advanceTo: 12 });
  });

  it("an open_draft signal never moves the cursor", () => {
    expect(consume({ id: "12", data: '{"open_draft":5,"at":1}' })).toMatchObject({ type: "signal" });
  });

  it("an unknown enum value decodes (lenient), a missing required key does not", () => {
    expect(decodeEvent(event(1).replace('"urgent"', '"from-the-future"'))).not.toBeNull();
    const o = JSON.parse(event(1));
    delete o.sender;
    expect(decodeEvent(JSON.stringify(o))).toBeNull();
  });

  it("an absent sealed_kind still decodes (older daemon)", () => {
    expect(decodeEvent(event(1))?.sealed_kind).toBeUndefined();
  });
});

describe("Backoff", () => {
  it("jitters over the lower half of the window and doubles to the cap", () => {
    const lo = new Backoff(1, 60, () => 0);
    expect([1, 2, 3, 4, 5, 6, 7, 8].map(() => lo.next())).toEqual([0.5, 1, 2, 4, 8, 16, 30, 30]);
    const hi = new Backoff(1, 60, () => 1);
    expect([1, 2, 3].map(() => hi.next())).toEqual([1, 2, 4]);
  });

  it("reset returns to the base window", () => {
    const b = new Backoff(1, 60, () => 1);
    b.next(); b.next(); b.reset();
    expect(b.next()).toBe(1);
  });
});

// ---------------------------------------------------------------------------
// The connection loop against a fake fetch.

function memCursor(initial: number | null): CursorStore & { saved: number[] } {
  let v = initial;
  const saved: number[] = [];
  return { saved, load: () => v, save: (id) => { v = id; saved.push(id); } };
}

function sseResponse(text: string, status = 200): Response {
  return new Response(new Blob([text]).stream(), { status, headers: { "content-type": "text/event-stream" } });
}

describe("EventStream", () => {
  it("first run connects CURSORLESS (live only), never after=0", async () => {
    const urls: string[] = [];
    let s!: EventStream;
    const fetchImpl = (async (u: string) => {
      urls.push(u);
      void s.stop();
      return sseResponse("");
    }) as unknown as typeof fetch;
    s = new EventStream({ serverUrl: "https://d.example/", token: "t", cursor: memCursor(null), onConsumed: () => {}, fetchImpl });
    s.start();
    await new Promise((r) => setTimeout(r, 10));
    await s.stop();
    expect(urls).toEqual(["https://d.example/client/events"]);
  });

  it("resumes with after=<cursor>; cursor only moves forward; bearer only in the header", async () => {
    const cursor = memCursor(5);
    const calls: { url: string; init: RequestInit }[] = [];
    let s!: EventStream;
    const fetchImpl = (async (url: string, init: RequestInit) => {
      calls.push({ url, init });
      if (calls.length === 2) void s.stop();
      // a replay overlapping the seam: 4 (old), 6, 7, then 6 again
      return sseResponse([4, 6, 7, 6].map((i) => `id: ${i}\ndata: ${event(i)}\n\n`).join(""));
    }) as unknown as typeof fetch;
    const seen: number[] = [];
    s = new EventStream({
      serverUrl: "https://d.example", token: "secret", cursor,
      onConsumed: (c) => { if (c.type === "event") seen.push(c.event.id); },
      fetchImpl, sleep: async () => {},
    });
    s.start();
    await new Promise((r) => setTimeout(r, 20));
    await s.stop();
    expect(calls[0]!.url).toBe("https://d.example/client/events?after=5");
    expect(calls[1]!.url).toBe("https://d.example/client/events?after=7");
    expect(cursor.saved).toEqual([6, 7]);
    expect((calls[0]!.init.headers as Record<string, string>).Authorization).toBe("Bearer secret");
    expect(calls[0]!.url).not.toContain("secret");
    expect(calls[0]!.init.redirect).toBe("error");
    expect(calls[0]!.init.credentials).toBe("omit");
    expect(seen.slice(0, 4)).toEqual([4, 6, 7, 6]); // dedupe by id is the consumer's job, as on the Mac
  });

  it("non-200 and network errors back off with a widening window, reset after a healthy connection", async () => {
    const naps: number[] = [];
    let n = 0;
    let clock = 0;
    let s!: EventStream;
    const fetchImpl = (async () => {
      n++;
      if (n === 4) clock += 31_000; // the 4th connection lives 31s
      if (n === 6) void s.stop();
      if (n === 1) throw new TypeError("Failed to fetch");
      return n === 4 ? sseResponse("") : sseResponse("", 503);
    }) as unknown as typeof fetch;
    s = new EventStream({
      serverUrl: "https://d.example", token: "t", cursor: memCursor(1), onConsumed: () => {},
      fetchImpl, now: () => clock, random: () => 1,
      sleep: async (ms) => { naps.push(ms); },
    });
    s.start();
    await new Promise((r) => setTimeout(r, 20));
    await s.stop();
    expect(naps.slice(0, 5)).toEqual([1000, 2000, 4000, 1000, 2000]);
  });
});
