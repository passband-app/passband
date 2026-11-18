// The text rules the sitrep's rows and cards use, ported from the Mac client's
// Lib/Format.swift (`Fmt`), Model/SubjectText.swift and Lib/Reading.swift.
// Every rule that depends on "today" takes `now` and the owner's time zone.

import { parseCalendarDay, parseInstant } from "../lib/time";
import { dayNumber, monthDayOfDay, wallParts, zonedDay } from "../lib/zoned";
import type { Pill } from "./model";

/** Whitespace runs collapsed to one space, ends trimmed (`flattened`). */
export function flattened(s: string): string {
  return s.split(/\s+/u).filter((w) => w.length > 0).join(" ");
}

/**
 * Emoji removed (`withoutEmoji`): any grapheme whose first scalar presents as
 * emoji, or is emoji followed by VS16. Then flattened.
 */
export function withoutEmoji(s: string): string {
  const seg = new Intl.Segmenter("en", { granularity: "grapheme" });
  let out = "";
  for (const { segment } of seg.segment(s)) {
    const first = segment.codePointAt(0);
    if (first === undefined) continue;
    const ch = String.fromCodePoint(first);
    const presentation = /\p{Emoji_Presentation}/u.test(ch);
    const emojiWithVS = /\p{Emoji}/u.test(ch) && segment.includes("️");
    if (presentation || emojiWithVS) continue;
    out += segment;
  }
  return flattened(out);
}

/** Cut to `n` characters, the ellipsis included (`Fmt.truncate`). */
export function truncate(s: string, n: number): string {
  if (n <= 0) return "";
  const chars = Array.from(new Intl.Segmenter("en", { granularity: "grapheme" }).segment(s), (x) => x.segment);
  if (chars.length <= n) return s;
  return chars.slice(0, n - 1).join("").trim() + "…";
}

export function capitalizingFirst(s: string): string {
  const first = Array.from(s)[0];
  return first === undefined ? s : first.toUpperCase() + s.slice(first.length);
}

/** "Promotional email from X: New gear" -> "New gear" (`Reading.cleanSummary`). */
export function cleanSummary(summary: string): string {
  let out = summary.trim();
  out = out.replace(
    /^(promotional email|marketing email|newsletter|promo(?:tion)?)\s*(from\s+[^:,-]+)?[:,-]?\s+/i,
    "",
  );
  out = out.replace(/^\w[\w ]{0,24}?\bpromotion\s+(for|from|of)\s+/i, "");
  out = out.trim();
  if (!out) return summary.trim();
  return capitalizingFirst(out);
}

/** The first "$1,234.56"-shaped run in a line (`MoneyScan.amount`). */
export function moneyIn(text: string): string | null {
  const chars = Array.from(text);
  let i = 0;
  while (i < chars.length) {
    if (chars[i] !== "$") {
      i += 1;
      continue;
    }
    let j = i + 1;
    if (j < chars.length && chars[j] === " ") j += 1;
    let digits = "";
    while (j < chars.length && /[\p{N},.]/u.test(chars[j]!)) {
      digits += chars[j];
      j += 1;
    }
    while (digits.endsWith(".") || digits.endsWith(",")) digits = digits.slice(0, -1);
    if (/\p{N}/u.test(digits)) return `$${digits}`;
    i = j + 1;
  }
  return null;
}

/**
 * An amount with its currency, two decimals (`Fmt.usd`). Pinned to en-US: the
 * Mac formats in the system locale, which a page cannot ask for reliably.
 */
export function money(amount: number | null | undefined, currency?: string | null): string {
  if (amount === null || amount === undefined || Number.isNaN(amount)) return "—";
  const code = (currency || "USD").toUpperCase();
  try {
    return new Intl.NumberFormat("en-US", {
      style: "currency",
      currency: code,
      minimumFractionDigits: 2,
      maximumFractionDigits: 2,
    }).format(amount);
  } catch {
    return `${amount.toFixed(2)} ${code}`;
  }
}

function unit(n: number, word: string): string {
  return `${n} ${word}${n === 1 ? "" : "S"}`;
}

/** "45 MIN", "3 HOURS", "2 DAYS", "1 WEEK" (`Fmt.loudAge`). */
export function loudAge(thenMs: number, nowMs: number): string {
  const ms = nowMs - thenMs;
  if (ms < 0) return "NOW";
  const min = ms / 60_000;
  if (min < 60) return `${Math.round(min)} MIN`;
  const h = min / 60;
  if (h < 24) return unit(Math.round(h), "HOUR");
  const d = h / 24;
  if (d < 14) return unit(Math.round(d), "DAY");
  return unit(Math.round(d / 7), "WEEK");
}

/** "Sep 30" for an instant, on the owner's calendar (`Fmt.shortDate`). */
export function shortDate(ms: number, tz: string): string {
  return monthDayOfDay(zonedDay(ms, tz));
}

/** A stamp that falls on today in `tz`. A date-only value is its own day. */
export function isToday(value: string | null | undefined, nowMs: number, tz: string): boolean {
  if (!value) return false;
  const today = zonedDay(nowMs, tz);
  const day = parseCalendarDay(value);
  if (day) return dayNumber(day.year, day.month, day.day) === today;
  const t = parseInstant(value);
  return t !== null && zonedDay(t, tz) === today;
}

export interface DeadlineChip {
  text: string;
  overdue: boolean;
}

/**
 * The due pill (`Fmt.deadlineChip`). A date-only deadline is due through that
 * whole local day ("3d PAST DUE", "due today", "due Sep 30"); a timestamp keeps
 * its exact instant ("2 HOURS PAST DUE", "due Oct 2").
 */
export function deadlineChip(value: string | null | undefined, nowMs: number, tz: string): DeadlineChip | null {
  if (!value) return null;
  const day = parseCalendarDay(value);
  if (day) {
    const days = zonedDay(nowMs, tz) - dayNumber(day.year, day.month, day.day);
    if (days > 0) return { text: `${days}d PAST DUE`, overdue: true };
    if (days === 0) return { text: "due today", overdue: false };
    return { text: `due ${monthDayOfDay(dayNumber(day.year, day.month, day.day))}`, overdue: false };
  }
  const t = parseInstant(value);
  if (t === null) return null;
  if (t - nowMs < 0) return { text: `${loudAge(t, nowMs)} PAST DUE`, overdue: true };
  return { text: `due ${shortDate(t, tz)}`, overdue: false };
}

export function deadlinePill(value: string | null | undefined, nowMs: number, tz: string): Pill | undefined {
  const chip = deadlineChip(value, nowMs, tz);
  return chip ? { text: chip.text, tone: chip.overdue ? "overdue" : "due" } : undefined;
}

/** "Good morning" / "Good afternoon" / "Good evening" by the owner's clock. */
export function greetingWord(nowMs: number, tz: string): string {
  const h = wallParts(nowMs, tz).hour;
  if (h < 12) return "Good morning";
  if (h < 18) return "Good afternoon";
  return "Good evening";
}
