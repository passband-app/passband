// Timestamps on the wire, ported from the Mac client's `Fmt` (Lib/Format.swift).
//
// The daemon writes RFC 3339 with anywhere from zero to nine fractional digits
// and either `Z` or a numeric offset. `Date.parse` is only specified for
// exactly three digits, so this parses the shape itself and never hands a wire
// string to the host's lenient parser. A date-only string is NOT an instant
// and is refused here; see `parseCalendarDay`.

const RFC3339 =
  /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})(?:\.(\d{1,9}))?(Z|[+-]\d{2}:\d{2})$/i;

/** Epoch milliseconds for an RFC 3339 instant, or null for anything else. */
export function parseInstant(iso: string | null | undefined): number | null {
  if (!iso) return null;
  const m = RFC3339.exec(iso);
  if (!m) return null;
  const [, y, mo, d, h, mi, s, frac, zone] = m;
  const year = Number(y);
  const month = Number(mo);
  const day = Number(d);
  const hour = Number(h);
  const minute = Number(mi);
  const second = Number(s);
  if (month < 1 || month > 12 || day < 1 || day > daysIn(year, month)) return null;
  if (hour > 23 || minute > 59 || second > 60) return null;
  const ms = frac ? Math.floor(Number(`0.${frac}`) * 1000) : 0;
  let offsetMin = 0;
  if (zone && zone.toUpperCase() !== "Z") {
    const sign = zone[0] === "-" ? -1 : 1;
    const oh = Number(zone.slice(1, 3));
    const om = Number(zone.slice(4, 6));
    if (oh > 23 || om > 59) return null;
    offsetMin = sign * (oh * 60 + om);
  }
  return (
    Date.UTC(year, month - 1, day, hour, minute, Math.min(second, 59), ms) -
    offsetMin * 60_000
  );
}

/** `YYYY-MM-DD` as its parts, or null. A calendar day, not an instant. */
export function parseCalendarDay(
  s: string | null | undefined,
): { year: number; month: number; day: number } | null {
  if (!s) return null;
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(s);
  if (!m) return null;
  const year = Number(m[1]);
  const month = Number(m[2]);
  const day = Number(m[3]);
  if (month < 1 || month > 12 || day < 1 || day > daysIn(year, month)) return null;
  return { year, month, day };
}

function daysIn(year: number, month: number): number {
  return new Date(Date.UTC(year, month, 0)).getUTCDate();
}

/** Compact age, as `Fmt.relAge`: "now", "12m", "4h", "5d", "3w", "2mo". */
export function relAge(then: number | string | null | undefined, now: number): string {
  const t = typeof then === "string" ? parseInstant(then) : then ?? null;
  if (t === null) return "";
  const ms = now - t;
  if (ms < 0) return "now";
  const min = ms / 1000 / 60;
  if (min < 1) return "now";
  if (min < 60) return `${Math.round(min)}m`;
  const h = min / 60;
  if (h < 24) return `${Math.round(h)}h`;
  const d = h / 24;
  if (d < 14) return `${Math.round(d)}d`;
  const w = d / 7;
  if (w < 8) return `${Math.round(w)}w`;
  return `${Math.round(d / 30)}mo`;
}
