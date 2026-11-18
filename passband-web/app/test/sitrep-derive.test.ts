// The whole board from real daemon answers (synthetic mail from the local
// seeded daemon, saved under test/fixtures/sitrep), on a pinned clock and zone.
import { describe, expect, test } from "vitest";
import type { Client } from "../src/api/client";
import { InPageTransport, MemoryCredentialStore, ApiError } from "../src/api/transport";
import { Client as RealClient } from "../src/api/client";
import { feed, fyeHeadline, shipments, stats } from "../src/api/wire";
import { parseInstant } from "../src/lib/time";
import { billing, deriveSitrep, heroLine, loadSitrep, ownerDate, type Clock } from "../src/sitrep/derive";
import fyeJson from "./fixtures/sitrep/feed-fye.json";
import headlineJson from "./fixtures/sitrep/headline.json";
import readingJson from "./fixtures/sitrep/feed-reading.json";
import recordsJson from "./fixtures/sitrep/feed-records.json";
import shipmentsJson from "./fixtures/sitrep/shipments.json";
import statsJson from "./fixtures/sitrep/stats.json";

// Wednesday 2026-11-18, 12:40 PM in Los Angeles; the fixtures were written at 20:32Z.
const clock: Clock = { now: parseInstant("2026-11-18T20:40:00Z")!, zone: "America/Los_Angeles" };

const sources = () => ({
  stats: stats(statsJson),
  fye: feed(fyeJson),
  headline: fyeHeadline(headlineJson)?.headline ?? null,
  records: feed(recordsJson),
  reading: feed(readingJson),
  shipments: shipments(shipmentsJson),
});

describe("deriveSitrep on the seeded daemon's answers", () => {
  const s = deriveSitrep(sources(), clock);

  test("the greeting uses the given name, by the owner's clock", () => {
    expect(s.greeting).toBe("Good afternoon, Sam");
    expect(deriveSitrep(sources(), { ...clock, zone: "Asia/Tokyo" }).greeting).toBe("Good morning, Sam");
  });

  test("no headline from the daemon: the calm line stands in", () => {
    expect(s.headline).toBe("Your inbox, quietly sorted.");
    expect(heroLine(null, 0)).toBe("You're all clear.");
    expect(heroLine("The offsite has a date.", 3)).toBe("The offsite has a date.");
  });

  test("for your eyes keeps the server's order and carries due pills", () => {
    expect(s.eyes.map((e) => e.id)).toEqual([3, 2, 1, 5, 4]);
    expect(s.eyes.map((e) => e.sender)).toEqual(["Jamie", "Lakeside Dental", "Harborview Leasing", "Mom", "Alex"]);
    expect(s.eyes.map((e) => e.pill?.text)).toEqual(["due Nov 20", "due today", "3d PAST DUE", undefined, undefined]);
    expect(s.eyes.map((e) => e.overdue)).toEqual([false, false, true, false, false]);
    expect(s.eyes[2]!.pill?.tone).toBe("overdue");
    expect(s.eyes[0]!.initials).toBe("JA");
    expect(s.eyes[1]!.initials).toBe("LD");
    expect(s.eyes.every((e) => e.sealed === false)).toBe(true);
    expect(s.eyes.every((e) => e.avatarHue >= 0 && e.avatarHue < 10)).toBe(true);
  });

  test("people never get a favicon domain", () => {
    expect(s.eyes.find((e) => e.sender === "Mom")!.domain).toBeUndefined();
    expect(s.eyes.find((e) => e.sender === "Jamie")!.domain).toBeUndefined();
  });

  test("calendar: one row per event still to come, in the feed's order", () => {
    expect(s.calendar.map((c) => [c.title, c.when, c.pill?.text])).toEqual([
      ["Team offsite", "Nov 30", "updated"],
      ["Dr. Ortiz · checkup", "Nov 20", undefined],
    ]);
  });

  test("receipts are today's, on the owner's calendar", () => {
    expect(s.receipts.map((r) => [r.merchant, r.amount])).toEqual([["Bluebird Coffee", "$6.50"]]);
    const allThree = deriveSitrep(sources(), { now: parseInstant("2026-11-16T20:00:00Z")!, zone: "UTC" });
    expect(allThree.receipts.map((r) => r.amount)).toEqual(["€25.00"]);
  });

  test("billing shows the last day, or back to when the card was last seen", () => {
    expect(s.billing).toEqual([]);
    const seen = parseInstant("2026-11-16T00:00:00Z")!;
    const rows = billing(feed(recordsJson), clock, seen);
    expect(rows.map((r) => [r.payee, r.pill?.text, r.due?.text, r.amount])).toEqual([
      ["Metro Power & Light", "autopay", "due Nov 27", "$142.08"],
      ["First Harbor Bank", "update", undefined, undefined],
    ]);
  });

  test("shipments: both packages ride the rail with their status", () => {
    expect(s.shipments.map((r) => [r.title, r.status.text, r.status.tone, r.detail, r.eta])).toEqual([
      ["Q1 mechanical keyboard", "out for delivery", "due", undefined, undefined],
      ["Linen notebooks, set of 3", "shipped", "accent", undefined, undefined],
    ]);
  });

  test("reading: one card per sender, every message kept", () => {
    expect(s.reading).toHaveLength(6);
    const total = s.reading.reduce((n, g) => n + g.count, 0);
    expect(total).toBe(feed(readingJson)!.items.length);
    for (const g of s.reading) {
      expect(g.snippet.length).toBeLessThanOrEqual(90);
      expect(g.threadId).not.toBe("");
    }
  });

  test("the owner's date for the headline route is their calendar day", () => {
    expect(ownerDate(clock)).toBe("2026-11-18");
    expect(ownerDate({ now: parseInstant("2026-11-19T02:00:00Z")!, zone: "America/Los_Angeles" })).toBe("2026-11-18");
    expect(ownerDate({ now: parseInstant("2026-11-19T02:00:00Z")!, zone: "UTC" })).toBe("2026-11-19");
  });
});

describe("loadSitrep", () => {
  function fakeClient(fail: Partial<Record<string, ApiError>> = {}): Client {
    const answer = <T>(name: string, value: T) => (fail[name] ? Promise.reject(fail[name]) : Promise.resolve(value));
    return {
      stats: () => answer("stats", stats(statsJson)!),
      feed: (d: string) =>
        answer(d, feed(d === "fye" ? fyeJson : d === "records" ? recordsJson : readingJson)!),
      shipments: () => answer("shipments", shipments(shipmentsJson)!),
      fyeHeadline: () => answer("headline", { headline: "The offsite has a date and your keyboard lands today.", settled: true }),
    } as unknown as Client;
  }

  test("everything answers: the whole board, with the daemon's headline", async () => {
    const r = await loadSitrep(fakeClient(), clock);
    expect(r.signedOut).toBe(false);
    expect(r.sitrep.headline).toBe("The offsite has a date and your keyboard lands today.");
    expect(Object.values(r.sources).every((x) => x === "ok")).toBe(true);
  });

  test("one source failing empties only its own zone", async () => {
    const r = await loadSitrep(fakeClient({ records: new ApiError("http", 503) }), clock);
    expect(r.sources.records).toBe("failed");
    expect(r.sitrep.calendar).toEqual([]);
    expect(r.sitrep.receipts).toEqual([]);
    expect(r.sitrep.eyes).toHaveLength(5);
    expect(r.sitrep.shipments).toHaveLength(2);
  });

  test("an older daemon without a route reads as feature absent", async () => {
    const r = await loadSitrep(fakeClient({ headline: new ApiError("not_found", 404) }), clock);
    expect(r.sources.headline).toBe("absent");
    expect(r.sitrep.headline).toBe("Your inbox, quietly sorted.");
  });

  test("a 401 anywhere says signed out", async () => {
    const r = await loadSitrep(fakeClient({ shipments: new ApiError("unauthorized", 401) }), clock);
    expect(r.signedOut).toBe(true);
  });
});

// End to end against a running daemon, when one is named:
//   SITREP_DAEMON=http://127.0.0.1:8849 SITREP_TOKEN=spike-token npx vitest run test/sitrep-derive.test.ts
const daemon = process.env.SITREP_DAEMON;
describe.runIf(Boolean(daemon))("loadSitrep against a live daemon", () => {
  test("every source answers and the board is drawn", async () => {
    const transport = new InPageTransport(
      new MemoryCredentialStore({ server: daemon!, token: process.env.SITREP_TOKEN ?? "spike-token" }),
    );
    const r = await loadSitrep(new RealClient(transport), { now: Date.now(), zone: "America/Los_Angeles" });
    expect(r.signedOut).toBe(false);
    expect(r.sources).toMatchObject({ stats: "ok", fye: "ok", records: "ok", reading: "ok", shipments: "ok" });
    expect(r.sitrep.eyes.length).toBeGreaterThan(0);
    expect(r.sitrep.reading.length).toBeGreaterThan(0);
    expect(r.sitrep.greeting).toMatch(/^Good (morning|afternoon|evening)/);
  });
});
