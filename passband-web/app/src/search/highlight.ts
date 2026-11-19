// The matched words in a hit, as SEGMENTS (the Mac's Lib/SearchHighlight.swift).
// The daemon says which words matched (`subject_matches` / `snippet_matches`);
// every occurrence of each, case-insensitively, is lit. The result is plain
// text runs for React to render as elements — nothing here is ever markup,
// because every string came from somebody's email.

export interface Segment {
  text: string;
  hit: boolean;
}

function escapeRegExp(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

export function highlight(text: string, matches: readonly string[] | undefined): Segment[] {
  if (!text) return [];
  const lit = new Array<boolean>(text.length).fill(false);
  for (const term of matches ?? []) {
    if (!term) continue;
    const re = new RegExp(escapeRegExp(term), "giu");
    for (const m of text.matchAll(re)) {
      const start = m.index ?? 0;
      for (let i = start; i < start + m[0].length; i++) lit[i] = true;
    }
  }
  const out: Segment[] = [];
  let from = 0;
  for (let i = 1; i <= text.length; i++) {
    if (i === text.length || lit[i] !== lit[from]) {
      out.push({ text: text.slice(from, i), hit: lit[from] ?? false });
      from = i;
    }
  }
  return out;
}
