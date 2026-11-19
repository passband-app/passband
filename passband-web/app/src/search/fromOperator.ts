// The `from:` operator as the search field sees it while it is being typed,
// ported from the Mac's Lib/FromOperator.swift (and `SearchOrder` beside it).
//
// The daemon parses `from:<text>` out of a finished query; this is the other
// end, deciding when the field should offer senders and what accepting one does
// to the text. THE TRAILING TOKEN IS THE CARET: the reader is "in" a `from:`
// operator exactly while the last whitespace-separated token starts with
// `from:` and the text does not end in whitespace.

const PREFIX = "from:";
/** The operators the daemon parses that take a value, lowercased. */
const OPERATORS = ["from:", "to:", "after:", "before:"];
/** The one operator that carries its own value: complete as typed. */
const SCOPE_OPERATOR = "in:sent";
/** Past this many words the reader is describing mail, not naming a person. */
const SENDER_WORDS = 3;

const WS = /[\s​]+/u;

export function tokens(query: string): string[] {
  return query.split(WS).filter((t) => t.length > 0);
}

/** The last token, or null when the text is empty or ends in whitespace. */
function trailingToken(query: string): string | null {
  if (query.length === 0 || /[\s​]$/u.test(query)) return null;
  const parts = query.split(/[\s​]/u);
  return parts[parts.length - 1] ?? null;
}

/**
 * The text after `from:` in the trailing token: "" when only the operator is
 * typed, null when the reader is not in a `from:` operator at all.
 */
export function fragment(query: string): string | null {
  const token = trailingToken(query);
  if (token === null || token.length < PREFIX.length) return null;
  if (token.slice(0, PREFIX.length).toLowerCase() !== PREFIX) return null;
  return token.slice(PREFIX.length);
}

/**
 * The query with the trailing `from:` token completed to `from:<address> `.
 * A query not in the operator comes back unchanged.
 */
export function accepting(address: string, query: string): string {
  const token = trailingToken(query);
  if (fragment(query) === null || token === null) return query;
  return `${query.slice(0, query.length - token.length)}${PREFIX}${address} `;
}

/** True while every token is a valueless operator (`from:`, `after:`): not a search yet. */
export function awaitingValue(query: string): boolean {
  const t = tokens(query);
  return t.length > 0 && t.every((tok) => OPERATORS.includes(tok.toLowerCase()));
}

function isOperator(token: string): boolean {
  const lower = token.toLowerCase();
  return lower === SCOPE_OPERATOR || OPERATORS.some((op) => lower.startsWith(op));
}

/**
 * What to look senders up for when the reader typed plain words: the trimmed
 * query, or null for one character, any operator, or more words than a name.
 */
export function senderFragment(query: string): string | null {
  const t = tokens(query);
  if (t.length === 0 || t.length > SENDER_WORDS) return null;
  if (t.some(isOperator)) return null;
  const f = t.join(" ");
  return [...f].length >= 2 ? f : null;
}

/** Whether a sender still answers `fragment` (substring of address or name, case folded). */
export function matches(addr: string, name: string | null | undefined, frag: string): boolean {
  const needle = frag.toLowerCase();
  return addr.toLowerCase().includes(needle) || (name?.toLowerCase().includes(needle) ?? false);
}

/** The query that lists one sender's mail, with the closing space. */
export function listing(address: string): string {
  return `${PREFIX}${address} `;
}

/** Operators and nothing else, each with a value: a plain newest-first listing. */
export function isListing(query: string): boolean {
  const t = tokens(query);
  if (t.length === 0) return false;
  return t.every((tok) => {
    const lower = tok.toLowerCase();
    return (
      lower === SCOPE_OPERATOR ||
      OPERATORS.some((op) => lower.startsWith(op) && lower.length > op.length)
    );
  });
}

/**
 * How a search asks for its order (the Mac's `SearchOrder.ask`): plain ranked
 * order with sent mail kept on a daemon that advertises `search_plain_order`,
 * the old unfinished-first ask otherwise, and a listing never grouped.
 */
export function searchOrder(
  term: string,
  plainOrder: boolean,
): { unfinishedFirst: boolean; includeSent: boolean } {
  if (isListing(term)) return { unfinishedFirst: false, includeSent: false };
  return plainOrder
    ? { unfinishedFirst: false, includeSent: true }
    : { unfinishedFirst: true, includeSent: false };
}
