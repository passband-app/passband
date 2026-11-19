// Ported from Tests/SearchPreviewTests.swift (cleanup cases and the hit's wire
// compatibility), plus the highlight rule from Lib/SearchHighlight.swift.
import { describe, expect, test } from "vitest";
import { searchHit, searchPage, senderHit } from "../src/api/wire";
import { highlight } from "../src/search/highlight";
import { cleanPreview } from "../src/search/preview";

describe("cleanPreview (SearchPreview.clean)", () => {
  test.each([
    [
      "Final tickets are on sale now.Manage subscription https://mail.example/long-tracking-code",
      "Final tickets are on sale now.",
    ],
    [
      "Read more https://www.example.com/tickets?utm_source=email&token=secret#tracking",
      "Read more example.com/tickets",
    ],
    ["https://promo.activehosted.com/f/255/?t=eyJhTOKEN", "promo.activehosted.com"],
    ["https://example.com/AbCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz?token=x", "example.com"],
    [
      "Visit www.example.com/show-tickets?ref=tracking. Then https://example.org/music#token",
      "Visit example.com/show-tickets. Then example.org/music",
    ],
    ["See https://example.com/summer%20tickets?utm_medium=email", "See example.com/summer tickets"],
    ["promo.activehosted.com/f/255/?t=longToken", "promo.activehosted.com"],
    ["Save your tickets below.", "Save your tickets below."],
  ])("%j", (raw, want) => expect(cleanPreview(raw)).toBe(want));
});

describe("SearchHit wire compatibility", () => {
  test("an old daemon's hit: status unknown, not unfinished", () => {
    const old = searchHit({
      id: 1,
      thread_id: "t",
      from_addr: "a@b.co",
      subject: "Tickets",
      received_at: "2026-09-17T00:00:00Z",
      snippet: "Preview",
    });
    expect(old?.is_done).toBeUndefined();
    expect(old?.snippet_matches).toBeUndefined();
  });
  test("a current hit carries status and matches", () => {
    const hit = searchHit({
      id: 1,
      thread_id: "t",
      from_addr: "a@b.co",
      subject: "Tickets",
      received_at: "2026-09-17T00:00:00Z",
      snippet: "Preview",
      is_done: true,
      subject_matches: ["Tickets"],
      snippet_matches: [],
    });
    expect(hit).toMatchObject({ is_done: true, subject_matches: ["Tickets"], snippet_matches: [] });
  });
  test("a page drops a bad row and keeps the cursor and diagnostics", () => {
    const p = searchPage({
      items: [{ id: "x" }, { id: 2, thread_id: "t", from_addr: "a@b.example", subject: "S", received_at: "r", snippet: "" }],
      next_cursor: "c1",
      diagnostics: { strict_hits: 0, any_hits: 0, terms: [{ text: "zz", df: 0 }] },
    });
    expect(p?.items.map((h) => h.id)).toEqual([2]);
    expect(p?.next_cursor).toBe("c1");
    expect(p?.diagnostics?.strict_hits).toBe(0);
  });
  test("sender hits", () => {
    expect(senderHit({ addr: "deals@brightly.example", display_name: "Brightly", msg_count: 9, last_received_at: "x" })).toEqual({
      addr: "deals@brightly.example",
      display_name: "Brightly",
      msg_count: 9,
    });
    expect(senderHit({ display_name: "No address" })).toBeNull();
  });
});

describe("highlight (SearchHighlight.paint)", () => {
  const lit = (text: string, m: string[]) =>
    highlight(text, m)
      .filter((s) => s.hit)
      .map((s) => s.text);
  test("every occurrence, case-insensitively", () => {
    expect(lit("Offsite moved; the offsite is on", ["offsite"])).toEqual(["Offsite", "offsite"]);
  });
  test("several terms, overlapping runs merged", () => {
    expect(lit("wifi password", ["wifi", "fi pass"])).toEqual(["wifi pass"]);
  });
  test("no matches, or empty terms, light nothing and keep the text whole", () => {
    expect(highlight("plain text", [])).toEqual([{ text: "plain text", hit: false }]);
    expect(highlight("plain text", [""])).toEqual([{ text: "plain text", hit: false }]);
    expect(highlight("plain text", undefined)).toEqual([{ text: "plain text", hit: false }]);
  });
  test("regex characters in a term are literal", () => {
    expect(lit("price (USD) $6.50", ["(usd)", "$6.50"])).toEqual(["(USD)", "$6.50"]);
  });
  test("the segments always rebuild the original text", () => {
    const text = "Re: offsite — 14th works";
    expect(highlight(text, ["14th", "re"]).map((s) => s.text).join("")).toBe(text);
  });
});
