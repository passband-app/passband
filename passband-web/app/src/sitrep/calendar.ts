// The Calendar card's rows, ported from the Mac client: one row per EVENT, not
// per email (`CalendarUpdate.folded`, Model/WireTypes.swift), and only events
// still to come (`CalendarVisibility.admits`, without the per-device
// seen-ledger the Mac keeps).

import type { Feed } from "../api/wire";
import { parseCalendarDay, parseInstant } from "../lib/time";
import { dayNumber, validZone, wallToInstant, zonedDay } from "../lib/zoned";

export interface CalendarEvent {
  id: number;
  message_id: number;
  thread_id?: string;
  event_title?: string;
  starts_at?: string;
  received_at: string;
  start_timezone?: string;
  change?: string;
}

/** Event records out of the `records` feed, folded (`AgentFeed.calendar`). */
export function calendarEvents(f: Feed): CalendarEvent[] {
  const rows: CalendarEvent[] = [];
  for (const item of f.items) {
    item.decision.records.forEach((r, index) => {
      if (r.kind !== "event") return;
      rows.push({
        id: item.message_id * 1000 + index,
        message_id: item.message_id,
        thread_id: item.thread_id,
        event_title: r.title,
        starts_at: r.start?.value,
        received_at: item.received_at,
        start_timezone: r.start?.timezone,
        change: r.change,
      });
    });
  }
  return foldCalendar(rows);
}

const words = (t: string) => t.toLowerCase().split(/[^\p{L}\p{N}]+/u).filter((w) => w.length > 0);

export function foldCalendar(rows: CalendarEvent[]): CalendarEvent[] {
  const title = (r: CalendarEvent): string[] | null => {
    const w = words(r.event_title ?? "");
    return w.length === 0 ? null : w;
  };
  const eq = (a: string[], b: string[]) => a.length === b.length && a.every((x, i) => x === b[i]);
  const matches = (a: string[], b: string[]): boolean => {
    if (eq(a, b)) return true;
    const [short, long] = a.length <= b.length ? [a, b] : [b, a];
    if (Array.from(short.join(" ")).length < 8 || long.length <= short.length) return false;
    for (let i = 0; i <= long.length - short.length; i++) {
      if (eq(long.slice(i, i + short.length), short)) return true;
    }
    return false;
  };
  const day = (r: CalendarEvent): string | null =>
    r.starts_at && r.starts_at.length >= 10 ? r.starts_at.slice(0, 10) : null;
  const sameStart = (a: CalendarEvent, b: CalendarEvent): boolean => {
    const x = a.starts_at;
    const y = b.starts_at;
    if (!x || !y || day(a) !== day(b)) return false;
    return x.length === 10 || y.length === 10 || x.slice(0, 16) === y.slice(0, 16);
  };
  const threadTwin = (a: CalendarEvent, b: CalendarEvent): boolean => {
    if (a.thread_id === undefined || a.thread_id !== b.thread_id || a.message_id === b.message_id) return false;
    const x = a.starts_at;
    const y = b.starts_at;
    if (!x || !y || x.length <= 10 || y.length <= 10) return false;
    return sameStart(a, b);
  };
  const news = (r: CalendarEvent) => r.change === "updated" || r.change === "cancelled";

  const groups: number[][] = [];
  const undated: number[] = [];
  const joins = (g: number[], i: number): boolean => {
    const first = rows[g[0]!]!;
    if (news(first)) return false;
    const a = title(first);
    const b = title(rows[i]!);
    return a !== null && b !== null && matches(a, b);
  };
  const holds = (g: number[], i: number): boolean => {
    if (news(rows[g[0]!]!)) return false;
    const row = rows[i]!;
    if (joins(g, i) && g.some((j) => sameStart(rows[j]!, row))) return true;
    return g.some((j) => threadTwin(rows[j]!, row)) && !g.some((j) => rows[j]!.message_id === row.message_id);
  };

  rows.forEach((row, i) => {
    if (title(row) === null || news(row)) {
      groups.push([i]);
    } else if (day(row) === null) {
      undated.push(i);
    } else {
      const g = groups.findIndex((grp) => holds(grp, i));
      if (g >= 0) groups[g]!.push(i);
      else groups.push([i]);
    }
  });
  for (const i of undated) {
    const dated = groups
      .map((_, gi) => gi)
      .filter((gi) => day(rows[groups[gi]![0]!]!) !== null && joins(groups[gi]!, i));
    if (dated.length === 1) {
      groups[dated[0]!]!.push(i);
    } else {
      const g =
        dated.length === 0 ? groups.findIndex((grp) => day(rows[grp[0]!]!) === null && joins(grp, i)) : -1;
      if (g >= 0) groups[g]!.push(i);
      else groups.push([i]);
    }
  }
  return groups
    .sort((a, b) => Math.min(...a) - Math.min(...b))
    .map((g) => {
      let best = g[0]!;
      for (const c of g.slice(1)) {
        const x = rows[c]!;
        const y = rows[best]!;
        const xd = day(x) !== null;
        const yd = day(y) !== null;
        let better: boolean;
        if (xd !== yd) better = xd;
        else if (x.received_at !== y.received_at) better = x.received_at < y.received_at;
        else better = c < best;
        if (better) best = c;
      }
      return rows[best]!;
    });
}

export type EventStart = { kind: "day"; day: number } | { kind: "instant"; ms: number };

/** A start in any shape the record carries: a date, an RFC 3339 instant, or a wall time in its zone. */
export function eventStart(value: string | undefined, eventZone: string | undefined, ownerZone: string): EventStart | null {
  if (!value) return null;
  const d = parseCalendarDay(value);
  if (d) return { kind: "day", day: dayNumber(d.year, d.month, d.day) };
  const t = parseInstant(value);
  if (t !== null) return { kind: "instant", ms: t };
  const m = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})(?::(\d{2})(?:\.\d{1,3})?)?$/.exec(value);
  if (!m) return null;
  const zone = validZone(eventZone ?? ownerZone);
  return {
    kind: "instant",
    ms: wallToInstant(Number(m[1]), Number(m[2]), Number(m[3]), Number(m[4]), Number(m[5]), Number(m[6] ?? 0), zone),
  };
}

/** Still to come: a timed event until it starts, an all-day one through its day. */
export function admits(start: EventStart | null, nowMs: number, ownerZone: string, eventZone?: string): boolean {
  if (!start) return true;
  if (start.kind === "instant") return nowMs < start.ms;
  return zonedDay(nowMs, validZone(eventZone ?? ownerZone)) <= start.day;
}
