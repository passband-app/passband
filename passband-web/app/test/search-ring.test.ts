// Ported from Tests/RecentSearchRingTests.swift. The question-line cases
// (`RecentEntry`) are not ported: the web client has no deeper-search lane, so
// the ring only ever holds searches.
import { describe, expect, test } from "vitest";
import { CAPACITY, MAX_LENGTH, adding, identity, loadRing, saveRing } from "../src/search/ring";

const gives = (q: string, ring: string[], want: string[]) => expect(adding(q, ring)).toEqual(want);

describe("RecentSearchRing", () => {
  test("newest first", () => {
    gives("wifi", [], ["wifi"]);
    gives("venmo", ["wifi"], ["venmo", "wifi"]);
    gives("rent", ["venmo", "wifi"], ["rent", "venmo", "wifi"]);
    gives("  wifi  ", [], ["wifi"]);
  });
  test("re-asking moves it up", () => {
    gives("wifi", ["venmo", "wifi", "rent"], ["wifi", "venmo", "rent"]);
    gives("rent", ["venmo", "wifi", "rent"], ["rent", "venmo", "wifi"]);
    gives("venmo", ["venmo", "wifi"], ["venmo", "wifi"]);
  });
  test("the spelling kept is the newest one", () => {
    gives("Venmo Receipt", ["venmo receipt"], ["Venmo Receipt"]);
    gives("venmo  receipt", ["Venmo Receipt"], ["venmo  receipt"]);
    gives("FROM:dan@example.com", ["from:dan@example.com"], ["FROM:dan@example.com"]);
  });
  test("ten, and the eleventh pushes the first out", () => {
    let ring: string[] = [];
    for (let i = 1; i <= 12; i++) ring = adding(`q${i}`, ring);
    expect(ring).toHaveLength(CAPACITY);
    const want = Array.from({ length: 13 }, (_, i) => `q${13 - i}`).slice(0, 10);
    gives("q13", ring, want);
  });
  test("nothing worth remembering is refused", () => {
    const ring = ["wifi"];
    for (const q of ["", "   ", "\n\t ", "​", "    "]) gives(q, ring, ring);
  });
  test("a long query is refused, not truncated", () => {
    const ring = ["wifi"];
    const atCap = "a".repeat(MAX_LENGTH);
    gives(atCap, ring, [atCap, "wifi"]);
    gives(atCap + "a", ring, ring);
    gives(`  ${atCap}  `, ring, [atCap, "wifi"]);
  });
  test("an older list heals on the way past", () => {
    gives("rent", ["Wifi", "wifi", "wifi "], ["rent", "Wifi"]);
  });
  test("identity is what the daemon would do", () => {
    expect(identity("wifi password")).toBe(identity("WiFi   Password"));
    expect(identity(" wifi\tpassword ")).toBe(identity("wifi password"));
    expect(identity("from:Dan@Example.com")).toBe(identity("FROM:dan@example.com"));
    expect(identity("wifi password")).not.toBe(identity("wifi"));
    expect(identity("wifi password")).not.toBe(identity("password wifi"));
    expect(identity("venmo")).not.toBe(identity("venmos"));
  });
});

describe("storage", () => {
  test("round trips, and blocked or corrupt storage reads as empty", () => {
    let held = "";
    saveRing({ setItem: (_k, v) => void (held = v) }, ["wifi", "rent"]);
    expect(loadRing({ getItem: () => held })).toEqual(["wifi", "rent"]);
    expect(loadRing({ getItem: () => "{nope" })).toEqual([]);
    expect(loadRing({ getItem: () => JSON.stringify([1, "x", null]) })).toEqual(["x"]);
    expect(
      loadRing({
        getItem: () => {
          throw new Error("blocked");
        },
      }),
    ).toEqual([]);
  });
});
