// Calendar arithmetic in an EXPLICIT time zone, for the rules the Mac client
// runs on `Calendar.current` ("is this today", "how many days past due").
//
// Nothing here reads the host's local zone: every function takes the zone, and
// the only clock is the `now` the caller passes. `Intl.DateTimeFormat` with a
// named `timeZone` is used only to split an instant into wall-clock parts; all
// words ("Sep", "Tue", "2:30 PM") are spelled here rather than by a locale, so
// the output is the same in every browser and every test zone.

export const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
export const WEEKDAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

export interface WallParts {
  year: number;
  month: number;
  day: number;
  hour: number;
  minute: number;
  second: number;
  /** 0 = Sunday. */
  weekday: number;
}

const formatters = new Map<string, Intl.DateTimeFormat>();

/** A zone the host can resolve, or "UTC". */
export function validZone(tz: string | null | undefined): string {
  if (!tz) return "UTC";
  if (formatters.has(tz)) return tz;
  try {
    formatter(tz);
    return tz;
  } catch {
    return "UTC";
  }
}

function formatter(tz: string): Intl.DateTimeFormat {
  let f = formatters.get(tz);
  if (!f) {
    f = new Intl.DateTimeFormat("en-US", {
      timeZone: tz,
      hourCycle: "h23",
      year: "numeric",
      month: "numeric",
      day: "numeric",
      hour: "numeric",
      minute: "numeric",
      second: "numeric",
      weekday: "short",
    });
    formatters.set(tz, f);
  }
  return f;
}

/** The wall clock in `tz` at instant `ms`. */
export function wallParts(ms: number, tz: string): WallParts {
  const out: Partial<WallParts> = {};
  for (const p of formatter(validZone(tz)).formatToParts(new Date(ms))) {
    switch (p.type) {
      case "year":
        out.year = Number(p.value);
        break;
      case "month":
        out.month = Number(p.value);
        break;
      case "day":
        out.day = Number(p.value);
        break;
      case "hour":
        out.hour = Number(p.value) % 24;
        break;
      case "minute":
        out.minute = Number(p.value);
        break;
      case "second":
        out.second = Number(p.value);
        break;
      case "weekday":
        out.weekday = WEEKDAYS.indexOf(p.value);
        break;
    }
  }
  return out as WallParts;
}

/** Days since the epoch for a calendar date: differences are whole days. */
export function dayNumber(year: number, month: number, day: number): number {
  return Math.floor(Date.UTC(year, month - 1, day) / 86_400_000);
}

/** The calendar day in `tz` that instant `ms` falls on, as a day number. */
export function zonedDay(ms: number, tz: string): number {
  const p = wallParts(ms, tz);
  return dayNumber(p.year, p.month, p.day);
}

export function weekdayOfDay(dayNum: number): number {
  return new Date(dayNum * 86_400_000).getUTCDay();
}

/** The instant a wall-clock time in `tz` names (the earlier one across a fall-back). */
export function wallToInstant(
  year: number,
  month: number,
  day: number,
  hour: number,
  minute: number,
  second: number,
  tz: string,
): number {
  const naive = Date.UTC(year, month - 1, day, hour, minute, second);
  const offsetAt = (ms: number) => {
    const p = wallParts(ms, tz);
    return Date.UTC(p.year, p.month - 1, p.day, p.hour, p.minute, p.second) - ms;
  };
  const first = naive - offsetAt(naive);
  const second2 = naive - offsetAt(first);
  return Math.min(first, second2);
}

/** "Sep 30", the way the Mac's "MMM d" and `.month(.abbreviated).day()` read. */
export function monthDayOfDay(dayNum: number): string {
  const d = new Date(dayNum * 86_400_000);
  return `${MONTHS[d.getUTCMonth()]} ${d.getUTCDate()}`;
}

/** "2:30 PM", the way `.dateTime.hour().minute()` reads in en_US. */
export function clockText(hour: number, minute: number): string {
  const h = hour % 12 === 0 ? 12 : hour % 12;
  return `${h}:${String(minute).padStart(2, "0")} ${hour < 12 ? "AM" : "PM"}`;
}
