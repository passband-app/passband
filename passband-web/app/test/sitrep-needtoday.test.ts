// Ported from passband/Tests/NeedTodayTests.swift: FYE membership and order
// are the server's, deadlines keep their precision, and the records feed
// projects into receipts, calendar and billing without inventing anything.
import { describe, expect, test } from "vitest";
import { feed, type Feed } from "../src/api/wire";
import { parseCalendarDay, parseInstant } from "../src/lib/time";
import { calendarEvents } from "../src/sitrep/calendar";
import { attentionDue, billing, eyes, receipts, type Clock } from "../src/sitrep/derive";
import { deadlineChip } from "../src/sitrep/format";
import { readingRow } from "../src/sitrep/reading";

const att = (over: object = {}) => ({
  show_in_fye: true,
  state: "informational",
  summary: "",
  factors: { urgency: 0, action_need: 0, personal_relevance: 0, importance: 0, attention_at: null },
  ...over,
});

const json = {
  version: 2,
  ranked_at: "2026-09-15T12:00:00Z",
  items: [
    {
      message_id: 9,
      thread_id: "personal",
      from_addr: "friend@example.com",
      subject: "Dinner",
      received_at: "2026-09-15T11:00:00Z",
      score: 55,
      decision: { kinds: ["correspondence"], destinations: [], summary: "Dinner invitation", reason: "Personal invitation" },
      attention: att({ summary: "Dinner invitation", factors: { urgency: 0, action_need: 0, personal_relevance: 1, importance: 0.1, attention_at: null } }),
    },
    {
      message_id: 2,
      thread_id: "bill",
      from_addr: "billing@example.com",
      subject: "Invoice",
      received_at: "2026-09-01T11:00:00Z",
      score: 50,
      decision: { kinds: ["bill"], destinations: [], summary: "Invoice", reason: "Payment outstanding" },
      attention: att({
        state: "needs_user",
        summary: "Pay invoice",
        factors: { urgency: 1, action_need: 1, personal_relevance: 0, importance: 1, attention_at: { value: "2026-09-30", timezone: null } },
      }),
    },
  ],
};

const LA = "America/Los_Angeles";
const at = (s: string) => parseInstant(s)!;

describe("NeedTodayTests", () => {
  const f = feed(json)!;

  test("importance does not reorder server results; date-only precision is kept", () => {
    const clock: Clock = { now: at("2026-09-15T19:00:00Z"), zone: LA };
    expect(eyes(f, clock).map((e) => e.id)).toEqual([9, 2]);
    expect(eyes(null, clock)).toEqual([]);
    expect(attentionDue(f.items[1]!)).toBe("2026-09-30");
    expect(f.items[1]!.decision.destinations).toEqual([]);
    expect(readingRow(f.items[1]!).one_line).toBe("Invoice");
  });

  test("a date-only deadline is due through its whole local day; a timestamp keeps its instant", () => {
    const lateDueDay = at("2026-09-30T23:59:00-07:00");
    expect(deadlineChip("2026-09-30", lateDueDay, LA)?.overdue).toBe(false);
    expect(deadlineChip("2026-09-30", at("2026-10-01T00:01:00-07:00"), LA)?.overdue).toBe(true);
    expect(deadlineChip("2026-09-30T12:00:00-07:00", lateDueDay, LA)?.overdue).toBe(true);
    expect(parseCalendarDay("2026-02-30")).toBeNull();
  });

  test("the records feed projects receipts, calendar and billing", () => {
    const projected: Feed = feed({
      ...json,
      items: [
        {
          ...json.items[0],
          received_at: "2026-09-15T18:00:00Z",
          decision: {
            ...json.items[0]!.decision,
            records: [
              { kind: "receipt", merchant: "Shop", amount: 12.5, currency: "USD" },
              { kind: "receipt", merchant: "Other Shop", amount: 20, currency: "USD" },
              { kind: "event", title: "Dinner", start: { value: "2026-10-01", timezone: "America/Los_Angeles" } },
              { kind: "financial_update", institution: "Bank", description: "Statement ready" },
              { kind: "bill", merchant: "Utility", amount: 42, currency: "USD", due: { value: "2026-10-02" }, autopay: true },
            ],
          },
        },
      ],
    })!;
    const clock: Clock = { now: at("2026-09-15T19:00:00Z"), zone: LA };
    const r = receipts(projected, clock);
    expect(r).toHaveLength(2);
    expect(new Set(r.map((x) => x.key)).size).toBe(2);
    expect(r[0]!.amount).toBe("$12.50");
    const cal = calendarEvents(projected);
    expect(cal[0]!.starts_at).toBe("2026-10-01");
    expect(cal[0]!.start_timezone).toBe("America/Los_Angeles");
    const b = billing(projected, clock);
    expect(b).toHaveLength(2);
    expect(b[0]!.pill?.text).toBe("update");
    expect(b[1]).toMatchObject({ payee: "Utility", amount: "$42.00", pill: { text: "autopay" } });
    expect(b[1]!.due?.text).toBe("due Oct 2");
    const empty = feed({ ...json, items: [] })!;
    expect([receipts(empty, clock), calendarEvents(empty), billing(empty, clock)]).toEqual([[], [], []]);
  });

  test("only version 2 is a feed", () => {
    expect(feed({ ...json, version: 1 })).toBeNull();
    expect(feed({ ...json, version: 3 })).toBeNull();
  });
});
