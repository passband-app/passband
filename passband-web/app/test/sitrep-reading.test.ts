// Ported from passband/Tests/ReadingTests.swift (24 checks).
import { describe, expect, test } from "vitest";
import { feed, isReadingContent, type FeedItem } from "../src/api/wire";
import { parseInstant } from "../src/lib/time";
import { deriveReading, isContent, nextReading, pruneReading, readingRow, type ReadingRow } from "../src/sitrep/reading";
import { displayName } from "../src/sitrep/sender";

const row = (id: number, sender: string, date: string): ReadingRow => ({
  id,
  thread_id: `thread-${id}`,
  sender,
  one_line: `Message ${id}`,
  surfaced_at: date,
});
const at = (s: string) => parseInstant(s)!;

describe("Reading.derive", () => {
  const oldHuman = row(1, "friend@example.com", "2020-01-01T00:00:00Z");
  const receiptCopy = row(2, "shop@example.com", "2026-09-01T00:00:00Z");
  const recentHuman = row(3, "friend@example.com", "2026-09-02T00:00:00Z");

  test("no sender, copy, age, or tier removes agent-selected mail", () => {
    const cards = deriveReading([oldHuman, receiptCopy, recentHuman]);
    expect(cards).toHaveLength(2);
    expect(cards[0]!.items.map((i) => i.id)).toEqual([3, 1]);
    expect(cards[1]!.items.map((i) => i.id)).toEqual([2]);
    const pruned = pruneReading(cards, new Set([3]));
    const friend = pruned.find((c) => c.address === "friend@example.com")!;
    expect(friend.latestThreadId).toBe("thread-1");
    expect(friend.count).toBe(1);
    expect(pruned.map((c) => c.address)).toEqual(["shop@example.com", "friend@example.com"]);
    expect(deriveReading([])).toEqual([]);
  });

  test("no silent cap: derive keeps every sender", () => {
    const many = Array.from({ length: 40 }, (_, i) => row(100 + i, `s${i}@news.com`, "2026-09-01T00:00:00Z"));
    expect(deriveReading(many)).toHaveLength(40);
  });

  const pinned = at("2026-09-11T00:00:00Z");
  const promoNew = row(10, "deals@store.com", "2026-09-20T00:00:00Z");
  const essayOld = row(11, "writer@substack.com", "2026-09-10T00:00:00Z");
  const postMid = row(12, "blog@lab.dev", "2026-09-15T00:00:00Z");
  const writerPromo = row(13, "writer@substack.com", "2026-09-21T00:00:00Z");
  const postOld = row(14, "blog@lab.dev", "2026-09-05T00:00:00Z");

  test("content first, newest first within each half; the cap applies after the split", () => {
    const split = deriveReading([promoNew, essayOld, postMid], new Set([11, 12]), pinned);
    expect(split.map((s) => s.address)).toEqual(["blog@lab.dev", "writer@substack.com", "deals@store.com"]);
    expect(split.map(isContent)).toEqual([true, true, false]);
    const capped = deriveReading([promoNew, essayOld], new Set([11]), pinned, 1);
    expect(capped.map((s) => s.address)).toEqual(["writer@substack.com"]);
  });

  test("a sender is placed by its newest message, and prune re-derives that", () => {
    const mixed = deriveReading([essayOld, writerPromo], new Set([11]), pinned);
    expect(isContent(mixed[0]!)).toBe(false);
    expect(isContent(pruneReading(mixed, new Set([13]))[0]!)).toBe(true);
    const board = deriveReading([promoNew, essayOld, writerPromo], new Set([11]), pinned);
    expect(board.map((s) => s.address)).toEqual(["writer@substack.com", "deals@store.com"]);
    const before = deriveReading([promoNew, postOld, essayOld, writerPromo], new Set([11, 14]), pinned);
    expect(before.map((s) => s.address)).toEqual(["blog@lab.dev", "writer@substack.com", "deals@store.com"]);
    expect(pruneReading(before, new Set([13])).map((s) => s.address)).toEqual([
      "writer@substack.com",
      "blog@lab.dev",
      "deals@store.com",
    ]);
  });

  test("the walk follows the board as it stood when the tile was opened", () => {
    const before = deriveReading([promoNew, postOld, essayOld, writerPromo], new Set([11, 14]), pinned);
    const order = before.map((s) => s.address);
    const next = (address: string, resolved: number[], notThread = "") =>
      nextReading(address, order, pruneReading(before, new Set(resolved)), notThread)?.address ?? null;
    expect(next("blog@lab.dev", [])).toBe("writer@substack.com");
    expect(next("writer@substack.com", [13])).toBe("deals@store.com");
    expect(next("writer@substack.com", [13, 11])).toBe("deals@store.com");
    expect(next("blog@lab.dev", [13, 11])).toBe("deals@store.com");
    const partly = nextReading("blog@lab.dev", order, pruneReading(before, new Set([13])), "");
    expect(partly?.latestThreadId).toBe("thread-11");
    expect(partly?.items.map((i) => i.id)).toEqual([11]);
    expect(next("blog@lab.dev", [], "thread-13")).toBe("deals@store.com");
    expect(next("deals@store.com", [])).toBeNull();
    expect(next("blog@lab.dev", [13, 11, 10])).toBeNull();
    expect(next("nobody@example.com", [])).toBeNull();
  });

  test("content keeps the top for a week, then files back in by date", () => {
    const now = at("2026-09-30T00:00:00Z");
    const staleEssay = row(20, "old@essays.com", "2026-09-01T00:00:00Z");
    const freshPromo = row(21, "deals@store.com", "2026-09-29T00:00:00Z");
    const weekEssay = row(22, "new@essays.com", "2026-09-24T00:00:00Z");
    const aged = deriveReading([staleEssay, freshPromo, weekEssay], new Set([20, 22]), now);
    expect(aged.map((s) => s.address)).toEqual(["new@essays.com", "deals@store.com", "old@essays.com"]);
    expect(aged.map(isContent)).toEqual([true, false, false]);
  });
});

function item(kinds: string[], readingKind?: string): FeedItem {
  const f = feed({
    version: 2,
    ranked_at: "2026-09-30T12:00:00Z",
    items: [
      {
        message_id: 1,
        thread_id: "t",
        from_addr: "a@b.example",
        subject: "s",
        received_at: "2026-09-01T00:00:00Z",
        score: 0,
        decision: { kinds, destinations: ["reading"], ...(readingKind ? { reading_kind: readingKind } : {}), summary: "x", reason: "y" },
        attention: {
          show_in_fye: false,
          state: "informational",
          summary: "",
          factors: { urgency: 0, action_need: 0, personal_relevance: 0, importance: 0, attention_at: null },
        },
      },
    ],
  });
  return f!.items[0]!;
}

describe("the wire", () => {
  test("reading_kind wins; decisions stored before it fall back to kinds", () => {
    expect(isReadingContent(item(["promotional"], "content"))).toBe(true);
    expect(isReadingContent(item(["editorial"], "promotion"))).toBe(false);
    expect(isReadingContent(item(["editorial"]))).toBe(true);
    expect(isReadingContent(item(["editorial", "promotional"]))).toBe(false);
  });

  test("a card is titled from the feed's from_name, and an older daemon without it still decodes", () => {
    const att = {
      show_in_fye: false,
      state: "informational",
      summary: "",
      factors: { urgency: 0, action_need: 0, personal_relevance: 0, importance: 0, attention_at: null },
    };
    const f = feed({
      version: 2,
      ranked_at: "2026-09-30T12:00:00Z",
      items: [
        {
          message_id: 7,
          thread_id: "parts",
          from_addr: "8c1f0e7d2b9a4c6e5f3d1a0b9c8e7f6d@emails.partsdepot.example",
          from_name: "PartsDepot.com National eStore",
          subject: "Fall Into Shipping Savings",
          received_at: "2026-09-30T11:00:00Z",
          score: 1,
          decision: { kinds: ["promotional"], destinations: ["reading"], summary: "New gear", reason: "Bulk" },
          attention: att,
        },
        {
          message_id: 8,
          thread_id: "old",
          from_addr: "offers@mail.artsupply.example",
          subject: "Sale",
          received_at: "2026-09-29T11:00:00Z",
          score: 1,
          decision: { kinds: ["promotional"], destinations: ["reading"], summary: "Sale", reason: "Bulk" },
          attention: att,
        },
      ],
    })!;
    const named = deriveReading(f.items.map(readingRow));
    expect(named).toHaveLength(2);
    expect(displayName(named[0]!.sender)).toBe("PartsDepot.com National eStore");
    expect(named[0]!.address).toBe("8c1f0e7d2b9a4c6e5f3d1a0b9c8e7f6d@emails.partsdepot.example");
    expect(named[1]!.sender).toBe("offers@mail.artsupply.example");
  });
});
