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
  /** A "$142.00" the one-liner mentions, shown beside it (the Mac's MoneyScan). */
  amount?: string;
  /**
   * Auth mail (a sign-in code, a reset). The Mac opens these in an audited
   * one-time reveal, never the reader, and keeps them off the walk.
   */
  sealed?: boolean;
  /** A robot or brand sender's domain, for a favicon. Never a person's. */
  domain?: string;
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
  /** Editorial content (an essay, a post) still inside its week: tinted on the Mac. */
  content?: boolean;
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
  /** "Keychron · #KC-48211" or "Rainforest · 2 packages"; absent when there is nothing to add. */
  detail?: string;
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
  /** "autopay", "bill", "update". */
  pill?: Pill;
  /** The due chip, beside the kind: "due Oct 2", "3d PAST DUE". */
  due?: Pill;
  /** Formatted with its currency, or absent ("statement" rows). */
  amount?: string;
  overdue: boolean;
  threadId?: string;
}

export interface ReceiptRow {
  key: string;
  merchant: string;
  /** Absent when the receipt named no amount. */
  amount?: string;
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

/** For-your-eyes rows before the "{n} more" expander (the Mac's `BoardFold`). */
export const EYES_FOLD = 10;
/** Reading cards before that zone's expander. */
export const READING_FOLD = 24;
/** Billing rows before its expander. */
export const BILLING_FOLD = 8;
