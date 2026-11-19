// The card text rules from Lib/Format.swift, Model/SubjectText.swift and
// Lib/Reading.swift.
import { describe, expect, test } from "vitest";
import { parseInstant } from "../src/lib/time";
import { wallToInstant } from "../src/lib/zoned";
import {
  cleanSummary,
  deadlineChip,
  greetingWord,
  isToday,
  loudAge,
  money,
  moneyIn,
  truncate,
  withoutEmoji,
} from "../src/sitrep/format";

const at = (s: string) => parseInstant(s)!;
const LA = "America/Los_Angeles";

describe("deadlineChip", () => {
  const now = at("2026-09-15T19:00:00Z"); // Tue Sep 15, 12:00 in LA
  test("date-only", () => {
    expect(deadlineChip("2026-09-12", now, LA)).toEqual({ text: "3d PAST DUE", overdue: true });
    expect(deadlineChip("2026-09-15", now, LA)).toEqual({ text: "due today", overdue: false });
    expect(deadlineChip("2026-09-18", now, LA)).toEqual({ text: "due Sep 18", overdue: false });
  });
  test("timestamps keep their instant", () => {
    expect(deadlineChip("2026-09-15T16:00:00Z", now, LA)).toEqual({ text: "3 HOURS PAST DUE", overdue: true });
    expect(deadlineChip("2026-09-15T18:20:00Z", now, LA)).toEqual({ text: "40 MIN PAST DUE", overdue: true });
    expect(deadlineChip("2026-09-20T12:00:00Z", now, LA)).toEqual({ text: "due Sep 20", overdue: false });
  });
  test("the day boundary is the owner's, not UTC's", () => {
    // 2026-09-16 02:00Z is still Sep 15 in LA.
    expect(deadlineChip("2026-09-15", at("2026-09-16T02:00:00Z"), LA)?.text).toBe("due today");
    expect(deadlineChip("2026-09-15", at("2026-09-16T02:00:00Z"), "UTC")?.text).toBe("1d PAST DUE");
  });
  test("nothing, or garbage, is no chip", () => {
    expect(deadlineChip(undefined, now, LA)).toBeNull();
    expect(deadlineChip("soon", now, LA)).toBeNull();
  });
});

describe("loudAge", () => {
  const now = at("2026-09-15T19:00:00Z");
  test("units and plurals", () => {
    expect(loudAge(now + 1000, now)).toBe("NOW");
    expect(loudAge(now - 60_000, now)).toBe("1 MIN");
    expect(loudAge(now - 3600_000, now)).toBe("1 HOUR");
    expect(loudAge(now - 2 * 86_400_000, now)).toBe("2 DAYS");
    expect(loudAge(now - 21 * 86_400_000, now)).toBe("3 WEEKS");
  });
});

describe("money", () => {
  test("MoneyScan finds the first dollar run", () => {
    expect(moneyIn("Your bill of $142.08 is due")).toBe("$142.08");
    expect(moneyIn("Pay $ 1,204.50 now")).toBe("$1,204.50");
    expect(moneyIn("Costs $5. Thanks")).toBe("$5");
    expect(moneyIn("No $ amount here")).toBeNull();
    expect(moneyIn("€25.00 only")).toBeNull();
  });
  test("amounts carry their currency, two decimals", () => {
    expect(money(6.5, "USD")).toBe("$6.50");
    expect(money(25, "EUR")).toBe("€25.00");
    expect(money(142.08)).toBe("$142.08");
    expect(money(null)).toBe("—");
    expect(money(3, "XYZ1")).toBe("3.00 XYZ1");
  });
});

describe("text", () => {
  test("truncate counts graphemes and includes the ellipsis", () => {
    expect(truncate("hello world", 20)).toBe("hello world");
    expect(truncate("hello world", 6)).toBe("hello…");
    expect(truncate("éééé", 3)).toBe("éé…");
  });
  test("emoji are stripped and whitespace flattened", () => {
    expect(withoutEmoji("  🎁 Rivera   Exhaust Works ")).toBe("Rivera Exhaust Works");
    expect(withoutEmoji("Q1 ⌨️ keyboard")).toBe("Q1 keyboard");
    expect(withoutEmoji("Plain #1001")).toBe("Plain #1001");
  });
  test("cleanSummary drops a leading genre label", () => {
    expect(cleanSummary("Promotional email from Rainforest: deals picked for you")).toBe("Deals picked for you");
    expect(cleanSummary("Newsletter: markets react calmly")).toBe("Markets react calmly");
    expect(cleanSummary("Holiday promotion for linen sheets")).toBe("Linen sheets");
    expect(cleanSummary("Why your cat stares at the wall")).toBe("Why your cat stares at the wall");
  });
});

describe("the owner's clock", () => {
  test("greeting by hour", () => {
    expect(greetingWord(wallToInstant(2026, 9, 15, 8, 0, 0, LA), LA)).toBe("Good morning");
    expect(greetingWord(wallToInstant(2026, 9, 15, 13, 0, 0, LA), LA)).toBe("Good afternoon");
    expect(greetingWord(wallToInstant(2026, 9, 15, 19, 0, 0, LA), LA)).toBe("Good evening");
  });
  test("isToday on the owner's calendar, date-only included", () => {
    const now = at("2026-09-16T02:00:00Z");
    expect(isToday("2026-09-15T20:00:00Z", now, LA)).toBe(true);
    expect(isToday("2026-09-15", now, LA)).toBe(true);
    expect(isToday("2026-09-15T20:00:00Z", now, "UTC")).toBe(false);
    expect(isToday(undefined, now, LA)).toBe(false);
  });
  test("wall times across the spring-forward gap and fall-back overlap resolve", () => {
    expect(new Date(wallToInstant(2026, 3, 8, 12, 0, 0, LA)).toISOString()).toBe("2026-03-08T19:00:00.000Z");
    expect(new Date(wallToInstant(2026, 11, 1, 1, 30, 0, LA)).toISOString()).toBe("2026-11-01T08:30:00.000Z");
  });
});
