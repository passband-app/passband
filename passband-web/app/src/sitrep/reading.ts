// The Reading zone's cards, ported from the Mac client's Lib/Reading.swift.
//
// Membership is the triage agent's (the `reading` feed); the client only
// groups by sender, never filters. Content (essays, posts) sorts ahead of
// promotions for a week, newest first within each half.

import type { FeedItem } from "../api/wire";
import { parseInstant } from "../lib/time";
import { senderAddress, senderString } from "./sender";

/** The message fields a card is built from (`AttentionUpdate`'s subset). */
export interface ReadingRow {
  id: number;
  thread_id: string;
  /** "Name <addr>" or the address. */
  sender: string;
  one_line: string;
  surfaced_at?: string;
}

export interface ReadingSender {
  address: string;
  sender: string;
  count: number;
  summary: string;
  /** Epoch seconds of the newest message. */
  latest: number;
  latestThreadId: string;
  /** Newest first. */
  items: ReadingRow[];
  contentIds: Set<number>;
  contentFreshSince: number;
}

export const CONTENT_WINDOW_S = 7 * 24 * 3600;

/** Content keeps the top only while its NEWEST message is content, and for a week. */
export function isContent(s: ReadingSender): boolean {
  const newest = s.items[0];
  if (!newest || !s.contentIds.has(newest.id)) return false;
  return s.latest >= s.contentFreshSince;
}

const dateOf = (r: ReadingRow) => (parseInstant(r.surfaced_at) ?? 0) / 1000;

/** A feed item as a Reading row (`readingRow`): the message summary, dated by arrival. */
export function readingRow(item: FeedItem): ReadingRow {
  return {
    id: item.message_id,
    thread_id: item.thread_id,
    sender: senderString(item.from_addr, item.from_name),
    one_line: item.decision.summary,
    surfaced_at: item.received_at,
  };
}

function boardOrder(a: ReadingSender, b: ReadingSender): number {
  const ca = isContent(a);
  const cb = isContent(b);
  if (ca !== cb) return ca ? -1 : 1;
  if (a.latest === b.latest) return a.address < b.address ? -1 : a.address > b.address ? 1 : 0;
  return b.latest - a.latest;
}

export function deriveReading(
  updates: ReadingRow[],
  content: Set<number> = new Set(),
  nowMs: number = Date.now(),
  limit: number = Number.POSITIVE_INFINITY,
): ReadingSender[] {
  const freshSince = nowMs / 1000 - CONTENT_WINDOW_S;
  const groups = new Map<string, ReadingRow[]>();
  for (const u of updates) {
    const key = senderAddress(u.sender);
    const list = groups.get(key);
    if (list) list.push(u);
    else groups.set(key, [u]);
  }
  const senders: ReadingSender[] = [];
  for (const [address, messages] of groups) {
    const ordered = [...messages].sort((x, y) => {
      const l = dateOf(x);
      const r = dateOf(y);
      return l === r ? y.id - x.id : r - l;
    });
    const latest = ordered[0];
    if (!latest) continue;
    senders.push({
      address,
      sender: latest.sender,
      count: ordered.length,
      summary: latest.one_line,
      latest: dateOf(latest),
      latestThreadId: latest.thread_id,
      items: ordered,
      contentIds: new Set(ordered.map((m) => m.id).filter((id) => content.has(id))),
      contentFreshSince: freshSince,
    });
  }
  return senders.sort(boardOrder).slice(0, limit);
}

/** Drop resolved messages; a sender with none left goes (`Reading.prune`). */
export function pruneReading(senders: ReadingSender[], resolved: Set<number>): ReadingSender[] {
  if (resolved.size === 0) return senders;
  const out: ReadingSender[] = [];
  for (const s of senders) {
    const live = s.items.filter((m) => !resolved.has(m.id));
    if (live.length === s.items.length) {
      out.push(s);
      continue;
    }
    const newest = live[0];
    if (!newest) continue;
    out.push({
      ...s,
      items: live,
      count: live.length,
      latest: dateOf(newest),
      latestThreadId: newest.thread_id,
      summary: newest.one_line ? newest.one_line : s.summary,
    });
  }
  return out.sort(boardOrder);
}

/** The next sender in a walk that began on the board `order` (`Reading.next`). */
export function nextReading(
  after: string,
  order: string[],
  board: ReadingSender[],
  notThread: string,
): ReadingSender | null {
  const here = order.indexOf(after);
  if (here < 0) return null;
  for (const candidate of order.slice(here + 1)) {
    const s = board.find((x) => x.address === candidate);
    if (s && s.latestThreadId !== "" && s.latestThreadId !== notThread) return s;
  }
  return null;
}
