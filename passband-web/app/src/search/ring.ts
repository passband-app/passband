// The last few queries, as a value (the Mac's Lib/RecentSearchRing.swift).
// Newest first, deduped by what the daemon would do with a query (case folded,
// whitespace collapsed), ten kept, and an absurdly long query refused rather
// than truncated — a truncated query is a different search.

export const CAPACITY = 10;
export const MAX_LENGTH = 200;

/** Whitespace as Foundation trims it, including the zero-width space. */
const WS_RUN = /[\s​]+/gu;
const WS_ENDS = /^[\s​]+|[\s​]+$/gu;

/** What makes two queries the same question. */
export function identity(query: string): string {
  return query
    .split(WS_RUN)
    .filter((t) => t.length > 0)
    .map((t) => t.toLowerCase())
    .join(" ");
}

/** Length in user-perceived characters, as Swift's `String.count`. */
function graphemes(s: string): number {
  const Seg = (Intl as { Segmenter?: typeof Intl.Segmenter }).Segmenter;
  if (Seg) {
    let n = 0;
    for (const _ of new Seg(undefined, { granularity: "grapheme" }).segment(s)) n++;
    return n;
  }
  return [...s].length;
}

/** Fold a submitted query into the ring. Unrememberable queries leave it (re-folded). */
export function adding(query: string, ring: readonly string[]): string[] {
  const kept = query.replace(WS_ENDS, "");
  if (graphemes(kept) > MAX_LENGTH) return [...ring];
  const seen = new Set<string>();
  const next: string[] = [];
  for (const candidate of [kept, ...ring]) {
    const id = identity(candidate);
    if (id === "" || seen.has(id)) continue;
    seen.add(id);
    next.push(candidate);
    if (next.length === CAPACITY) break;
  }
  return next;
}

const KEY = "pb_recent_searches_v1";

/** Per-browser memory of the ring; a blocked or corrupt store reads as empty. */
export function loadRing(storage: Pick<Storage, "getItem"> | undefined): string[] {
  try {
    const v: unknown = JSON.parse(storage?.getItem(KEY) ?? "[]");
    return Array.isArray(v) ? v.filter((x): x is string => typeof x === "string").slice(0, CAPACITY) : [];
  } catch {
    return [];
  }
}

export function saveRing(storage: Pick<Storage, "setItem"> | undefined, ring: readonly string[]): void {
  try {
    storage?.setItem(KEY, JSON.stringify(ring));
  } catch {
    // Storage blocked: the ring simply is not remembered.
  }
}
