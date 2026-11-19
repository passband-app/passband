// The Shipments card, ported from the Mac client's `Shipment` helpers
// (Model/WireTypes.swift) and `ShipmentCard` (Views/SitrepZones.swift).

import type { Carrier, Shipment, ShipmentStatus } from "../api/wire";
import { parseCalendarDay, parseInstant } from "../lib/time";
import { clockText, dayNumber, monthDayOfDay, WEEKDAYS, wallParts, weekdayOfDay, zonedDay } from "../lib/zoned";
import { isToday, withoutEmoji } from "./format";
import type { PillTone } from "./model";

export const STATUS_LABEL: Record<ShipmentStatus, string> = {
  ordered: "ordered",
  shipped: "shipped",
  out_for_delivery: "out for delivery",
  delivered: "delivered",
  exception: "exception",
};

export const STATUS_TONE: Record<ShipmentStatus, PillTone> = {
  out_for_delivery: "due",
  shipped: "accent",
  exception: "overdue",
  delivered: "positive",
  ordered: "neutral",
};

export const CARRIER_DOMAIN: Record<Carrier, string | undefined> = {
  ups: "ups.com",
  usps: "usps.com",
  fedex: "fedex.com",
  dhl: "dhl.com",
  amazon: undefined,
  unknown: undefined,
};

/** The item's name; else the merchant; else "Package". Never the carrier. */
export function displayTitle(s: Shipment): string {
  const item = withoutEmoji(s.item_name);
  if (item) return item;
  const store = withoutEmoji(s.merchant ?? "");
  return store || "Package";
}

/** Merchant (unless it is already the title), order refs with one "#", and the box count. */
export function orderLine(s: Shipment): string | null {
  const parts: string[] = [];
  const store = withoutEmoji(s.merchant ?? "");
  if (store && store !== displayTitle(s)) parts.push(store);
  const refs = (s.orders ?? [])
    .map((o) => withoutEmoji(o.order_ref).replace(/^#+/, "").trim())
    .filter((r) => r.length > 0)
    .map((r) => `#${r}`);
  if (refs.length) parts.push(refs.join(", "));
  const boxes = (s.legs ?? []).length + 1;
  if (boxes > 1) parts.push(`${boxes} packages`);
  return parts.length ? parts.join(" · ") : null;
}

/** Stays while any box is still coming, or landed today. */
export function staysOnRail(s: Shipment, isTodayFn: (v: string | undefined) => boolean): boolean {
  const anyUndelivered = s.status !== "delivered" || (s.legs ?? []).some((l) => l.status !== "delivered");
  return anyUndelivered || isTodayFn(s.delivered_at ?? s.last_update);
}

const WEEKDAY_WINDOW = 6;

function dayOf(value: string | undefined, tz: string): number | null {
  if (!value) return null;
  const d = parseCalendarDay(value);
  if (d) return dayNumber(d.year, d.month, d.day);
  const t = parseInstant(value);
  return t === null ? null : zonedDay(t, tz);
}

/** "today", a weekday inside six days either way, else "Sep 30". */
export function dayWord(value: string | undefined, nowMs: number, tz: string): string {
  const day = dayOf(value, tz);
  if (day === null) return "";
  const days = day - zonedDay(nowMs, tz);
  if (days === 0) return "today";
  return Math.abs(days) <= WEEKDAY_WINDOW ? WEEKDAYS[weekdayOfDay(day)]! : monthDayOfDay(day);
}

/** "delivered 2:10 PM" today, "delivered Tue" earlier; otherwise the status word. */
export function statusText(s: Shipment, nowMs: number, tz: string): string {
  const label = STATUS_LABEL[s.status];
  if (s.status !== "delivered") return label;
  const landed = s.delivered_at ?? s.last_update;
  const t = parseInstant(landed);
  if (t === null) return label;
  if (isToday(landed, nowMs, tz)) {
    const p = wallParts(t, tz);
    return `${label} ${clockText(p.hour, p.minute)}`;
  }
  return `${label} ${dayWord(landed, nowMs, tz)}`;
}

/** "arrives Sat", "expected Sep 3" once it has passed; none when delivered or due today. */
export function etaText(s: Shipment, nowMs: number, tz: string): string | null {
  if (s.status === "delivered" || !s.eta) return null;
  const day = dayOf(s.eta, tz);
  if (day === null) return null;
  const today = zonedDay(nowMs, tz);
  if (s.status === "out_for_delivery" && day === today) return null;
  if (day < today) return `expected ${monthDayOfDay(day)}`;
  return `arrives ${dayWord(s.eta, nowMs, tz)}`;
}
