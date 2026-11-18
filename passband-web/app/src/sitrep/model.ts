// The sitrep, as the screens draw it. Everything here is DISPLAY-READY: the
// derivation from wire types (ranking, grouping, formatting, mirroring the Mac
// client's Views/SitrepView.swift and SitrepZones.swift and their Lib/ rules)
// lives in src/sitrep/derive.ts, and the screens do no formatting of their own.

export type PillTone = "overdue" | "due" | "accent" | "positive" | "neutral" | "lock";

export interface Pill {
  text: string;
  tone: PillTone;
}

/** One row of "For your eyes". */
export interface EyesItem {
  /** The message id the verbs (done, archive) act on. */
  id: number;
  threadId: string;
  sender: string;
  /** Up to two letters for the initials avatar. */
  initials: string;
  /** Stable index into the avatar palette, from the sender address. */
  avatarHue: number;
  line: string;
  pill?: Pill;
  hasAttachment: boolean;
  /** Overdue rows carry the coral rail on their leading edge. */
  overdue: boolean;
}

/** One sender's tile in "Reading". */
export interface ReadingGroup {
  key: string;
  sender: string;
  count: number;
  /** Newest message's subject or preview, one line. */
  snippet: string;
  /** Thread to open on click (the newest). */
  threadId: string;
  /** Sender's domain, for a favicon; never a person's address. */
  domain?: string;
  initials: string;
  avatarHue: number;
}

export interface CalendarRow {
  key: string;
  title: string;
  /** "Thu 2:30 PM", "Oct 14", "today". */
  when: string;
  pill?: Pill;
  threadId?: string;
}

export interface ShipmentRow {
  key: string;
  title: string;
  /** "Keychron · order KC-48211" or "Rainforest · 2 items". */
  detail: string;
  status: Pill;
  /** "arrives Sat", when known. */
  eta?: string;
  /** Carrier or merchant domain, for a favicon. */
  domain?: string;
  threadId?: string;
}

export interface BillingRow {
  key: string;
  payee: string;
  pill?: Pill;
  /** Formatted with its currency, or absent ("statement" rows). */
  amount?: string;
  overdue: boolean;
  threadId?: string;
}

export interface ReceiptRow {
  key: string;
  merchant: string;
  amount: string;
  threadId?: string;
}

export interface Sitrep {
  /** "Good morning, Sam" — the name only when the daemon knows one. */
  greeting: string;
  /** The one-sentence editorial headline; null while it is being written. */
  headline: string | null;
  eyes: EyesItem[];
  reading: ReadingGroup[];
  calendar: CalendarRow[];
  shipments: ShipmentRow[];
  billing: BillingRow[];
  receipts: ReceiptRow[];
}

export const EMPTY_SITREP: Sitrep = {
  greeting: "",
  headline: null,
  eyes: [],
  reading: [],
  calendar: [],
  shipments: [],
  billing: [],
  receipts: [],
};
