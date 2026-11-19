// Ported from passband/Tests/CalendarFoldTests.swift: one Calendar row per
// event, not per email. Plus which events are still to come.
import { describe, expect, test } from "vitest";
import { parseInstant } from "../src/lib/time";
import { admits, eventStart, foldCalendar, type CalendarEvent } from "../src/sitrep/calendar";

function row(
  id: number,
  title: string | undefined,
  start: string | undefined,
  received: string,
  opts: { change?: string; thread?: string; message?: number } = {},
): CalendarEvent {
  return {
    id,
    message_id: opts.message ?? id,
    thread_id: opts.thread ?? `t${id}`,
    event_title: title,
    starts_at: start,
    received_at: received,
    change: opts.change,
  };
}
const ids = (rows: CalendarEvent[]) => rows.map((r) => r.id);
const party = "Northside Collective Presents Halcyon Drift";

describe("CalendarFoldTests", () => {
  test("the ticket mail and its reminder are one row", () => {
    const order = row(1, party, "2026-11-06", "2026-11-04T01:11:34Z");
    const reminder = row(2, party, undefined, "2026-11-04T19:00:37Z");
    const gym = row(3, "Gym", undefined, "2026-11-04T12:00:00Z");
    expect(ids(foldCalendar([reminder, gym, order]))).toEqual([1, 3]);
    const dated = row(2, party, "2026-11-06", "2026-11-04T19:00:37Z");
    expect(ids(foldCalendar([dated, order]))).toEqual([1]);
    const short = row(4, "halcyon  DRIFT", "2026-11-06", "2026-11-05T00:00:00Z");
    expect(ids(foldCalendar([order, short]))).toEqual([1]);
  });

  test("a timed and a date-only start on one day are one", () => {
    const timed = row(1, party, "2026-11-06T19:00:00-08:00", "2026-11-04T01:00:00Z");
    const day = row(2, party, "2026-11-06", "2026-11-04T02:00:00Z");
    expect(ids(foldCalendar([day, timed]))).toEqual([1]);
  });

  test("another day or another title stays apart", () => {
    const fri = row(1, party, "2026-11-06", "2026-11-04T01:00:00Z");
    const sat = row(2, party, "2026-11-07", "2026-11-04T02:00:00Z");
    expect(foldCalendar([fri, sat])).toHaveLength(2);
    const other = row(3, "Dentist", "2026-11-06", "2026-11-04T03:00:00Z");
    expect(foldCalendar([fri, other])).toHaveLength(2);
  });

  test("a dateless row never bridges two days", () => {
    const a = row(1, "Standup", "2026-11-06", "2026-11-04T01:00:00Z");
    const b = row(2, "Standup", "2026-11-13", "2026-11-04T02:00:00Z");
    const c = row(3, "Standup", undefined, "2026-11-04T03:00:00Z");
    expect(ids(foldCalendar([a, b, c]))).toEqual([1, 2, 3]);
    const d = row(4, "Standup", undefined, "2026-11-04T04:00:00Z");
    expect(ids(foldCalendar([c, d]))).toEqual([3]);
  });

  test("order, and untitled rows", () => {
    const x = row(1, undefined, "2026-11-06", "2026-11-04T01:00:00Z");
    const y = row(2, "  ", "2026-11-06", "2026-11-04T02:00:00Z");
    expect(foldCalendar([x, y])).toHaveLength(2);
    const p1 = row(3, party, undefined, "2026-11-04T19:00:00Z");
    const g = row(4, "Gym", undefined, "2026-11-04T12:00:00Z");
    const p2 = row(5, party, "2026-11-06", "2026-11-04T01:00:00Z");
    expect(ids(foldCalendar([p1, g, p2]))).toEqual([5, 4]);
  });

  test("news keeps its own row", () => {
    const order = row(1, party, "2026-11-06T19:00:00-08:00", "2026-11-04T01:00:00Z");
    const moved = row(2, party, "2026-11-06T21:00:00-08:00", "2026-11-05T01:00:00Z", { change: "updated" });
    expect(ids(foldCalendar([order, moved]))).toEqual([1, 2]);
    const off = row(3, party, "2026-11-06", "2026-11-05T02:00:00Z", { change: "cancelled" });
    expect(ids(foldCalendar([order, off]))).toEqual([1, 3]);
    const later = row(4, party, "2026-11-06T21:00:00-08:00", "2026-11-05T03:00:00Z");
    expect(foldCalendar([order, later])).toHaveLength(2);
    const reminder = row(5, party, undefined, "2026-11-05T04:00:00Z", { change: "created" });
    expect(ids(foldCalendar([order, reminder]))).toEqual([1]);
  });

  test("loose titles do not fold", () => {
    const gym = row(1, "Gym", "2026-11-06", "2026-11-04T01:00:00Z");
    const meet = row(2, "Gymnastics meet", "2026-11-06", "2026-11-04T02:00:00Z");
    expect(foldCalendar([gym, meet])).toHaveLength(2);
    const art = row(3, "art", "2026-11-06", "2026-11-04T03:00:00Z");
    const mel = row(4, "party at mel's", "2026-11-06", "2026-11-04T04:00:00Z");
    expect(foldCalendar([art, mel])).toHaveLength(2);
    const sam = row(5, "Lunch with Sam", "2026-11-06", "2026-11-04T05:00:00Z");
    const lunch = row(6, "Lunch", "2026-11-06", "2026-11-04T06:00:00Z");
    const ana = row(7, "Lunch with Ana", "2026-11-06", "2026-11-04T07:00:00Z");
    expect(foldCalendar([sam, lunch, ana])).toHaveLength(3);
  });

  test("an offset does not make a new start", () => {
    const a = row(1, party, "2026-11-13T15:30:00-08:00", "2026-11-11T13:00:00Z");
    const b = row(2, party, "2026-11-13T15:30:00", "2026-11-11T06:00:00Z");
    expect(ids(foldCalendar([a, b]))).toEqual([2]);
    const c = row(3, party, "2026-11-13T16:30:00", "2026-11-11T07:00:00Z");
    expect(foldCalendar([a, c])).toHaveLength(2);
  });

  test("a relayed thread is one event", () => {
    const t = "relay";
    const first = row(1, "Apartment tour at 25 Main St", "2026-11-13T15:30:00", "2026-11-11T05:00:00Z", { change: "agreed", thread: t });
    const second = row(2, "Rental tour: 25 Main St", "2026-11-13T15:30:00", "2026-11-11T06:00:00Z", { change: "agreed", thread: t });
    const third = row(3, "Rental tour: 25 Main St", "2026-11-13T15:30:00-08:00", "2026-11-11T13:00:00Z", { change: "agreed", thread: t });
    const meetup = row(4, "Agent meetup", "2026-11-17T17:30:00-08:00", "2026-11-11T02:00:00Z");
    expect(ids(foldCalendar([third, second, first, meetup]))).toEqual([1, 4]);
    const chatter = row(5, "Rental tour: 25 Main St", undefined, "2026-11-11T14:00:00Z", { thread: t });
    expect(ids(foldCalendar([chatter, second]))).toEqual([2]);
    const car = row(11, "Car rental pickup", undefined, "2026-11-11T14:00:00Z", { thread: t });
    expect(foldCalendar([car, first])).toHaveLength(2);
    const flight = row(12, "Flight to Lisbon", "2026-11-13T07:00:00-08:00", "2026-11-11T01:00:00Z", { thread: "trip" });
    const hotel = row(13, "Hotel check-in", "2026-11-13", "2026-11-11T02:00:00Z", { thread: "trip" });
    const cruise = row(14, "Dinner cruise", "2026-11-13T19:00:00", "2026-11-11T03:00:00Z", { thread: "trip" });
    expect(foldCalendar([flight, hotel, cruise])).toHaveLength(3);
    expect(foldCalendar([cruise, hotel, flight])).toHaveLength(3);
    const later = row(6, "Second viewing", "2026-11-13T17:00:00", "2026-11-11T15:00:00Z", { thread: t });
    expect(foldCalendar([first, later])).toHaveLength(2);
    const moved = row(7, "Rental tour: 25 Main St", "2026-11-13T15:30:00", "2026-11-11T16:00:00Z", { change: "updated", thread: t });
    expect(foldCalendar([first, moved])).toHaveLength(2);
    const x = row(8, "Matinee", "2026-11-13T15:30:00", "2026-11-11T05:00:00Z", { thread: "o", message: 9 });
    const y = row(9, "Workshop", "2026-11-13T15:30:00", "2026-11-11T05:00:00Z", { thread: "o", message: 9 });
    expect(foldCalendar([x, y])).toHaveLength(2);
    const z = row(15, "Matinee reminder", "2026-11-13T15:30:00", "2026-11-12T05:00:00Z", { thread: "o", message: 3 });
    expect(foldCalendar([z, x, y])).toHaveLength(2);
    expect(foldCalendar([x, y, z])).toHaveLength(2);
    const other = row(10, "Dentist", "2026-11-13T15:30:00", "2026-11-11T05:00:00Z");
    expect(foldCalendar([first, other])).toHaveLength(2);
  });
});

describe("still to come (CalendarVisibility.admits, without the seen-ledger)", () => {
  const LA = "America/Los_Angeles";
  const now = parseInstant("2026-11-13T18:00:00Z")!; // 10:00 in LA
  test("a timed event until it starts; a wall time is read in its own zone", () => {
    expect(admits(eventStart("2026-11-13T15:30:00", undefined, LA), now, LA)).toBe(true);
    expect(admits(eventStart("2026-11-13T09:30:00", undefined, LA), now, LA)).toBe(false);
    expect(admits(eventStart("2026-11-13T09:30:00", "Asia/Tokyo", LA), now, LA, "Asia/Tokyo")).toBe(false);
    expect(admits(eventStart("2026-11-13T19:00:00Z", undefined, LA), now, LA)).toBe(true);
  });
  test("an all-day event through its whole day", () => {
    expect(admits(eventStart("2026-11-13", undefined, LA), now, LA)).toBe(true);
    expect(admits(eventStart("2026-11-12", undefined, LA), now, LA)).toBe(false);
    expect(admits(null, now, LA)).toBe(true);
  });
});
