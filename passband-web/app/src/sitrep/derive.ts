// Wire -> the sitrep the screens draw. Pure functions with `now` and the
// owner's time zone injected, mirroring the Mac client's Views/SitrepView.swift
// and SitrepZones.swift, plus `loadSitrep`, which fetches every source in
// parallel and lets any one of them fail on its own.

import type { Client } from "../api/client";
import { ApiError } from "../api/transport";
import { isReadingContent, type Feed, type FeedItem, type Shipment, type Stats } from "../api/wire";
import { parseInstant } from "../lib/time";
import { clockText, monthDayOfDay, validZone, wallParts, zonedDay } from "../lib/zoned";
import { admits, calendarEvents, eventStart } from "./calendar";
import { cleanSummary, deadlineChip, greetingWord, isToday, money, moneyIn, shortDate, truncate } from "./format";
import type { BillingRow, CalendarRow, EyesItem, ReadingGroup, ReceiptRow, ShipmentRow, Sitrep } from "./model";
import { deriveReading, isContent, readingRow } from "./reading";
import { avatarSlot, displayName, eligibleFaviconDomain, initials, senderString } from "./sender";
import { CARRIER_DOMAIN, STATUS_TONE, displayTitle, etaText, orderLine, staysOnRail, statusText } from "./shipments";

export interface Clock {
  /** Epoch milliseconds. */
  now: number;
  /** The owner's IANA zone (the browser's, in the app). */
  zone: string;
}

// ---------------------------------------------------------------- hero

/** "Good morning, Sam"; no name when the daemon has none (never the address). */
export function greeting(stats: Stats | null, clock: Clock): string {
  const word = greetingWord(clock.now, clock.zone);
  const name = stats?.account_given_name?.trim();
  return name ? `${word}, ${name}` : word;
}

/** The hero line (`HeroLine.title`): the daemon's sentence, or a calm true one. */
export function heroLine(headline: string | null | undefined, eyesCount: number, empty?: string): string {
  if (eyesCount <= 0) return empty ?? "You're all clear.";
  if (headline && headline.trim()) return headline;
  return "Your inbox, quietly sorted.";
}

/** The owner's date for the headline route: YYYY-MM-DD on their calendar. */
export function ownerDate(clock: Clock): string {
  const p = wallParts(clock.now, clock.zone);
  return `${p.year}-${String(p.month).padStart(2, "0")}-${String(p.day).padStart(2, "0")}`;
}

// ---------------------------------------------------------------- for your eyes

const AUTH_LABEL: Record<string, string> = {
  otp: "Login code",
  password_reset: "Password reset",
  magic_link: "Sign-in link",
  login_alert: "Sign-in alert",
  security_alert: "Security alert",
  verification: "Verification",
  account_change: "Account change",
  app_grant: "App access",
  signin_receipt: "Sign-in receipt",
};

export function authLabel(kind: string | undefined): string {
  return (kind && AUTH_LABEL[kind]) || "Auth message";
}

/**
 * The row's deadline: `attention.factors.attention_at`, as the Mac reads it.
 * Where the daemon left that empty but stated a due date on an open action,
 * the earliest such date stands in, so the date the daemon knows is not lost.
 */
export function attentionDue(item: FeedItem): string | undefined {
  const at = item.attention.attention_at?.value;
  if (at) return at;
  const dues = item.attention.actions
    .filter((a) => !a.resolved && a.due?.value)
    .map((a) => a.due!.value)
    .sort();
  return dues[0];
}

/** One FYE row. Order is the server's; nothing here re-ranks or filters. */
export function eyesItem(item: FeedItem, clock: Clock): EyesItem {
  const sender = senderString(item.from_addr, item.from_name);
  const line = item.attention.summary || item.decision.summary;
  const auth = item.decision.auth;
  const base = {
    id: item.message_id,
    threadId: item.thread_id,
    sender: displayName(sender),
    initials: initials(sender),
    avatarHue: avatarSlot(sender),
    hasAttachment: false,
    domain: eligibleFaviconDomain(sender) ?? undefined,
  };
  if (auth && auth.kinds.length > 0) {
    const label = authLabel(auth.kinds[0]);
    return {
      ...base,
      line: line || label,
      pill: { text: auth.critical ? "Critical" : label, tone: auth.critical ? "overdue" : "lock" },
      overdue: false,
      sealed: true,
    };
  }
  const chip = deadlineChip(attentionDue(item), clock.now, clock.zone);
  return {
    ...base,
    line,
    pill: chip ? { text: chip.text, tone: chip.overdue ? "overdue" : "due" } : undefined,
    overdue: chip?.overdue ?? false,
    amount: moneyIn(line) ?? undefined,
    sealed: false,
  };
}

export function eyes(f: Feed | null, clock: Clock): EyesItem[] {
  return f ? f.items.map((i) => eyesItem(i, clock)) : [];
}

// ---------------------------------------------------------------- reading

export function reading(f: Feed | null, clock: Clock): ReadingGroup[] {
  if (!f) return [];
  const content = new Set(f.items.filter(isReadingContent).map((i) => i.message_id));
  return deriveReading(f.items.map(readingRow), content, clock.now).map((s) => ({
    key: s.address,
    sender: displayName(s.sender),
    count: s.count,
    snippet: s.summary ? truncate(cleanSummary(s.summary), 90) : "",
    threadId: s.latestThreadId,
    domain: eligibleFaviconDomain(s.sender) ?? undefined,
    initials: initials(s.sender),
    avatarHue: avatarSlot(s.sender),
    content: isContent(s),
  }));
}

// ---------------------------------------------------------------- calendar

/** "2:30 PM" today, else "Nov 6"; a date-only start is its day. */
export function calendarWhen(value: string | undefined, eventZone: string | undefined, clock: Clock): string {
  const start = eventStart(value, eventZone, clock.zone);
  if (!start) return "";
  if (start.kind === "day") return monthDayOfDay(start.day);
  if (zonedDay(start.ms, clock.zone) === zonedDay(clock.now, clock.zone)) {
    const p = wallParts(start.ms, clock.zone);
    return clockText(p.hour, p.minute);
  }
  return shortDate(start.ms, clock.zone);
}

export function calendar(f: Feed | null, clock: Clock): CalendarRow[] {
  if (!f) return [];
  return calendarEvents(f)
    .filter((e) => admits(eventStart(e.starts_at, e.start_timezone, clock.zone), clock.now, clock.zone, e.start_timezone))
    .map((e) => ({
      key: String(e.id),
      title: e.event_title || "calendar event",
      when: calendarWhen(e.starts_at, e.start_timezone, clock),
      pill:
        e.change === "updated"
          ? { text: "updated", tone: "neutral" as const }
          : e.change === "cancelled"
            ? { text: "canceled", tone: "overdue" as const }
            : undefined,
      threadId: e.thread_id,
    }));
}

// ---------------------------------------------------------------- shipments

export function shipmentRows(list: Shipment[] | null, clock: Clock): ShipmentRow[] {
  if (!list) return [];
  return list
    .filter((s) => staysOnRail(s, (v) => isToday(v, clock.now, clock.zone)))
    .map((s) => ({
      key: String(s.id),
      title: displayTitle(s),
      detail: orderLine(s) ?? undefined,
      status: { text: statusText(s, clock.now, clock.zone), tone: STATUS_TONE[s.status] },
      eta: etaText(s, clock.now, clock.zone) ?? undefined,
      domain: CARRIER_DOMAIN[s.carrier],
      threadId: s.thread_id || undefined,
    }));
}

// ---------------------------------------------------------------- billing

/** The Mac's `SitrepWindow`: the last 24 hours, or since the zone was last seen if that reaches further. */
export const BILLING_WINDOW_MS = 24 * 3600 * 1000;

export function billingCutoff(now: number, lastSeen?: number | null): number {
  const floor = now - BILLING_WINDOW_MS;
  return lastSeen !== null && lastSeen !== undefined && lastSeen < floor ? lastSeen : floor;
}

export function billing(f: Feed | null, clock: Clock, lastSeen?: number | null): BillingRow[] {
  if (!f) return [];
  const cutoff = billingCutoff(clock.now, lastSeen);
  const out: BillingRow[] = [];
  for (const item of f.items) {
    const received = parseInstant(item.received_at);
    if (received !== null && received < cutoff) continue;
    item.decision.records.forEach((r, index) => {
      const bill = r.kind === "bill";
      if (!bill && r.kind !== "financial_update") return;
      const payee = (bill ? r.merchant : r.institution) || displayName(item.from_addr) || "bank";
      const due = deadlineChip(r.due?.value, clock.now, clock.zone);
      out.push({
        key: String(item.message_id * 1000 + index),
        payee,
        pill: { text: r.autopay === true ? "autopay" : bill ? "bill" : "update", tone: "neutral" },
        due: due ? { text: due.text, tone: due.overdue ? "overdue" : "neutral" } : undefined,
        amount: r.amount !== undefined ? money(r.amount, r.currency) : undefined,
        overdue: due?.overdue ?? false,
        threadId: item.thread_id,
      });
    });
  }
  return out;
}

// ---------------------------------------------------------------- receipts

/** Today's receipts only (the Mac's card). */
export function receipts(f: Feed | null, clock: Clock): ReceiptRow[] {
  if (!f) return [];
  const out: ReceiptRow[] = [];
  for (const item of f.items) {
    if (!isToday(item.received_at, clock.now, clock.zone)) continue;
    item.decision.records.forEach((r, index) => {
      if (r.kind !== "receipt") return;
      out.push({
        key: String(item.message_id * 1000 + index),
        merchant: displayName(senderString(item.from_addr, r.merchant)),
        amount: r.amount !== undefined ? money(r.amount, r.currency) : undefined,
        threadId: item.thread_id,
      });
    });
  }
  return out;
}

// ---------------------------------------------------------------- the whole board

export interface SitrepSources {
  stats: Stats | null;
  fye: Feed | null;
  headline: string | null;
  records: Feed | null;
  reading: Feed | null;
  shipments: Shipment[] | null;
}

export function deriveSitrep(src: SitrepSources, clock: Clock, billingLastSeen?: number | null): Sitrep {
  const clk = { now: clock.now, zone: validZone(clock.zone) };
  const eyesRows = eyes(src.fye, clk);
  return {
    greeting: greeting(src.stats, clk),
    headline: heroLine(src.headline, eyesRows.length),
    eyes: eyesRows,
    reading: reading(src.reading, clk),
    calendar: calendar(src.records, clk),
    shipments: shipmentRows(src.shipments, clk),
    billing: billing(src.records, clk, billingLastSeen),
    receipts: receipts(src.records, clk),
  };
}

/** What a failed source means: absent (a 404 from an older daemon) or just unavailable now. */
export type SourceState = "ok" | "absent" | "failed";

export interface LoadedSitrep {
  sitrep: Sitrep;
  sources: Record<keyof SitrepSources, SourceState>;
  /** Signed out mid-load: the app should go back to sign-in. */
  signedOut: boolean;
}

async function settle<T>(p: Promise<T>): Promise<{ value: T | null; state: SourceState; auth: boolean }> {
  try {
    return { value: await p, state: "ok", auth: false };
  } catch (e) {
    const auth = e instanceof ApiError && (e.kind === "unauthorized" || e.kind === "signed_out");
    const absent = e instanceof ApiError && e.kind === "not_found";
    return { value: null, state: absent ? "absent" : "failed", auth };
  }
}

/**
 * Fetch everything the board needs in parallel. Any one source failing leaves
 * its zone empty and the rest drawn; the headline is asked for only after the
 * FYE feed, since an empty board never shows one.
 */
export async function loadSitrep(
  client: Client,
  clock: Clock,
  opts: { billingLastSeen?: number | null } = {},
): Promise<LoadedSitrep> {
  const [stats, fye, records, readingFeed, ships] = await Promise.all([
    settle(client.stats()),
    settle(client.feed("fye", 200)),
    settle(client.feed("records", 1000)),
    settle(client.feed("reading", 200)),
    settle(client.shipments(true)),
  ]);
  let headline: { value: { headline?: string } | null; state: SourceState; auth: boolean } = {
    value: null,
    state: "absent",
    auth: false,
  };
  if (fye.value && fye.value.items.length > 0) {
    headline = await settle(client.fyeHeadline(ownerDate(clock)));
  }
  const all = [stats, fye, records, readingFeed, ships, headline];
  return {
    sitrep: deriveSitrep(
      {
        stats: stats.value,
        fye: fye.value,
        headline: headline.value?.headline ?? null,
        records: records.value,
        reading: readingFeed.value,
        shipments: ships.value,
      },
      clock,
      opts.billingLastSeen,
    ),
    sources: {
      stats: stats.state,
      fye: fye.state,
      headline: headline.state,
      records: records.state,
      reading: readingFeed.state,
      shipments: ships.state,
    },
    signedOut: all.some((s) => s.auth),
  };
}
