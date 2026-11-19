// Ported from Tests/GmailHealthTests.swift and Tests/SpamWireTests.swift
// (`Fmt.date`), plus the `Fmt.relAge` thresholds in Lib/Format.swift.
import { describe, expect, test } from "vitest";
import { parseCalendarDay, parseInstant, relAge } from "../src/lib/time";

describe("parseInstant (Fmt.date)", () => {
  test("offset spelling, with and without fractional seconds", () => {
    expect(parseInstant("2026-08-26T00:01:25+00:00")).toBe(Date.UTC(2026, 7, 26, 0, 1, 25));
    expect(parseInstant("2026-08-26T01:12:02.249947+00:00")).toBe(
      Date.UTC(2026, 7, 26, 1, 12, 2, 249),
    );
  });
  test("the Z spelling", () => {
    expect(parseInstant("2026-08-26T00:01:25Z")).toBe(Date.UTC(2026, 7, 26, 0, 1, 25));
  });
  test("the fractional stamp parses, and a plain one still does", () => {
    expect(parseInstant("2026-09-01T21:05:09.897666Z")).toBe(
      Date.UTC(2026, 8, 1, 21, 5, 9, 897),
    );
    expect(parseInstant("2026-09-01T21:05:09Z")).not.toBeNull();
  });
  test("nanosecond precision, as the daemon writes it", () => {
    expect(parseInstant("2026-11-12T21:16:15.614410602Z")).toBe(
      Date.UTC(2026, 10, 12, 21, 16, 15, 614),
    );
  });
  test("a non-zero offset moves the instant", () => {
    expect(parseInstant("2026-01-01T09:00:00-08:00")).toBe(Date.UTC(2026, 0, 1, 17));
    expect(parseInstant("2026-01-01T05:30:00+05:30")).toBe(Date.UTC(2026, 0, 1, 0));
  });
  test("absence stays absence, and garbage does not become a date", () => {
    expect(parseInstant(null)).toBeNull();
    expect(parseInstant(undefined)).toBeNull();
    expect(parseInstant("")).toBeNull();
    expect(parseInstant("not a date")).toBeNull();
  });
  test("shapes a lenient host parser would accept are refused", () => {
    for (const s of [
      "2026-09-01",
      "2026-09-01 21:05:09Z",
      "2026-09-01T21:05Z",
      "2026-13-01T00:00:00Z",
      "2026-02-30T00:00:00Z",
      "2026-09-01T24:00:00Z",
      "2026-09-01T21:05:09",
      "Tue, 01 Sep 2026 21:05:09 GMT",
      "2026-09-01T21:05:09.1234567890Z",
    ]) {
      expect(parseInstant(s), s).toBeNull();
    }
  });
});

describe("parseCalendarDay", () => {
  test("a valid day", () => {
    expect(parseCalendarDay("2026-02-28")).toEqual({ year: 2026, month: 2, day: 28 });
    expect(parseCalendarDay("2028-02-29")).toEqual({ year: 2028, month: 2, day: 29 });
  });
  test("impossible days and instants are refused", () => {
    expect(parseCalendarDay("2026-02-29")).toBeNull();
    expect(parseCalendarDay("2026-09-01T00:00:00Z")).toBeNull();
    expect(parseCalendarDay(null)).toBeNull();
  });
});

describe("relAge (Fmt.relAge)", () => {
  const now = Date.UTC(2026, 8, 30, 15);
  const ago = (ms: number) => now - ms;
  const MIN = 60_000;
  test("thresholds", () => {
    expect(relAge(ago(-5 * MIN), now)).toBe("now");
    expect(relAge(ago(30_000), now)).toBe("now");
    expect(relAge(ago(12 * MIN), now)).toBe("12m");
    expect(relAge(ago(59 * MIN), now)).toBe("59m");
    expect(relAge(ago(4 * 60 * MIN), now)).toBe("4h");
    expect(relAge(ago(5 * 24 * 60 * MIN), now)).toBe("5d");
    expect(relAge(ago(13 * 24 * 60 * MIN), now)).toBe("13d");
    expect(relAge(ago(21 * 24 * 60 * MIN), now)).toBe("3w");
    expect(relAge(ago(90 * 24 * 60 * MIN), now)).toBe("3mo");
  });
  test("strings go through the strict parser", () => {
    expect(relAge("2026-09-30T14:00:00Z", now)).toBe("1h");
    expect(relAge("garbage", now)).toBe("");
    expect(relAge(null, now)).toBe("");
  });
});
