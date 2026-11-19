// The `/client/*` shapes the web client reads, mirroring Model/WireTypes.swift.
//
// Decoding follows the Mac client's rules:
//   - an unknown enum value falls back (a newer daemon adding a tier must not
//     break an older page);
//   - fields a newer daemon appends are ignored, and fields an older daemon
//     lacks are optional;
//   - in a list, one malformed row costs only itself.
// A decoder returns null when a REQUIRED field is missing or mistyped.

type Json = unknown;
type Obj = Record<string, Json>;

const isObj = (v: Json): v is Obj => typeof v === "object" && v !== null && !Array.isArray(v);
const str = (v: Json): string | null => (typeof v === "string" ? v : null);
const optStr = (v: Json): string | undefined => (typeof v === "string" ? v : undefined);
const int = (v: Json): number | null =>
  typeof v === "number" && Number.isSafeInteger(v) ? v : null;
const optInt = (v: Json): number | undefined => int(v) ?? undefined;
const optBool = (v: Json): boolean | undefined => (typeof v === "boolean" ? v : undefined);

function lenientEnum<T extends string>(values: readonly T[], fallback: T) {
  return (v: Json): T =>
    typeof v === "string" && (values as readonly string[]).includes(v) ? (v as T) : fallback;
}

/** Decode every row that decodes; drop the rest. */
export function rows<T>(v: Json, one: (r: Json) => T | null): T[] {
  if (!Array.isArray(v)) return [];
  const out: T[] = [];
  for (const r of v) {
    const d = one(r);
    if (d !== null) out.push(d);
  }
  return out;
}

export const TIERS = ["past_due", "deadline", "signal", "noise"] as const;
export type Tier = (typeof TIERS)[number];
export const tier = lenientEnum(TIERS, "signal");

export const STATUSES = ["new", "open", "done"] as const;
export type AttentionStatus = (typeof STATUSES)[number];
export const status = lenientEnum(STATUSES, "new");

export interface AttentionUpdate {
  id: number;
  thread_id: string;
  tier: Tier;
  importance: number;
  sender: string;
  one_line: string;
  reason: string;
  status: AttentionStatus;
  deadline?: string;
  deadline_date?: string;
  from_name?: string;
  subject?: string;
  preview?: string;
  has_attachments?: boolean;
  received_at?: string;
  surfaced_at?: string;
  resolved_at?: string;
}

export function attentionUpdate(v: Json): AttentionUpdate | null {
  if (!isObj(v)) return null;
  const id = int(v.id);
  const thread_id = str(v.thread_id);
  const importance = int(v.importance);
  const sender = str(v.sender);
  const one_line = str(v.one_line);
  const reason = str(v.reason);
  if (
    id === null ||
    thread_id === null ||
    importance === null ||
    sender === null ||
    one_line === null ||
    reason === null
  ) {
    return null;
  }
  return {
    id,
    thread_id,
    tier: tier(v.tier),
    importance,
    sender,
    one_line,
    reason,
    status: status(v.status),
    deadline: optStr(v.deadline),
    deadline_date: optStr(v.deadline_date),
    from_name: optStr(v.from_name),
    subject: optStr(v.subject),
    preview: optStr(v.preview),
    has_attachments: optBool(v.has_attachments),
    received_at: optStr(v.received_at),
    surfaced_at: optStr(v.surfaced_at),
    resolved_at: optStr(v.resolved_at),
  };
}

export interface Page<T> {
  items: T[];
  next_cursor?: string;
}

export function page<T>(v: Json, one: (r: Json) => T | null): Page<T> | null {
  if (!isObj(v) || !Array.isArray(v.items)) return null;
  return { items: rows(v.items, one), next_cursor: optStr(v.next_cursor) };
}

export interface Attachment {
  id: number;
  filename: string;
  mime: string;
  size: number;
  downloadable?: boolean;
  content_id?: string;
}

export function attachment(v: Json): Attachment | null {
  if (!isObj(v)) return null;
  const id = int(v.id);
  const filename = str(v.filename);
  const mime = str(v.mime);
  const size = int(v.size);
  if (id === null || filename === null || mime === null || size === null) return null;
  return {
    id,
    filename,
    mime,
    size,
    downloadable: optBool(v.downloadable),
    content_id: optStr(v.content_id),
  };
}

export interface ClientMessage {
  id: number;
  from_addr: string;
  received_at: string;
  content: string;
  from_name?: string;
  html?: string;
  subject?: string;
  to_addrs?: string;
  cc_addrs?: string;
  attachments: Attachment[];
  is_sent?: boolean;
  /** A body the daemon has not hydrated yet (`POST /thread/{id}/hydrate`). */
  lite?: boolean;
}

export function clientMessage(v: Json): ClientMessage | null {
  if (!isObj(v)) return null;
  const id = int(v.id);
  const from_addr = str(v.from_addr);
  const received_at = str(v.received_at);
  const content = str(v.content);
  if (id === null || from_addr === null || received_at === null || content === null) return null;
  return {
    id,
    from_addr,
    received_at,
    content,
    from_name: optStr(v.from_name),
    html: optStr(v.html),
    subject: optStr(v.subject),
    to_addrs: optStr(v.to_addrs),
    cc_addrs: optStr(v.cc_addrs),
    attachments: rows(v.attachments, attachment),
    is_sent: optBool(v.is_sent),
    lite: optBool(v.lite),
  };
}

export interface ThreadView {
  thread_id: string;
  subject: string;
  messages: ClientMessage[];
}

export function threadView(v: Json): ThreadView | null {
  if (!isObj(v)) return null;
  const thread_id = str(v.thread_id);
  const subject = str(v.subject);
  if (thread_id === null || subject === null || !Array.isArray(v.messages)) return null;
  return { thread_id, subject, messages: rows(v.messages, clientMessage) };
}

export interface Stats {
  account_email?: string;
  /** The Google profile's given name, for the sitrep greeting. Never the address. */
  account_given_name?: string;
  total?: number;
  bands?: { standing?: number; new?: number; open?: number };
}

export function stats(v: Json): Stats | null {
  if (!isObj(v)) return null;
  const b = isObj(v.bands) ? v.bands : undefined;
  return {
    account_email: optStr(v.account_email),
    account_given_name: optStr(v.account_given_name),
    total: optInt(v.total),
    bands: b
      ? { standing: optInt(b.standing), new: optInt(b.new), open: optInt(b.open) }
      : undefined,
  };
}

/** `StatusResult`: only success matters to callers (as in the Mac client). */
export interface StatusResult {
  status: string;
  message_id?: number;
}

export function statusResult(v: Json): StatusResult | null {
  if (!isObj(v)) return null;
  const s = str(v.status);
  return s === null ? null : { status: s, message_id: optInt(v.message_id) };
}

export interface SendResult {
  status: string;
  thread_id?: string;
}

export function sendResult(v: Json): SendResult | null {
  if (!isObj(v)) return null;
  const s = str(v.status);
  return s === null ? null : { status: s, thread_id: optStr(v.thread_id) };
}

export interface PairResult {
  token: string;
  device_id: number;
  name: string;
}

export function pairResult(v: Json): PairResult | null {
  if (!isObj(v)) return null;
  const token = str(v.token);
  const device_id = int(v.device_id);
  const name = str(v.name);
  if (token === null || token.length === 0 || device_id === null || name === null) return null;
  return { token, device_id, name };
}

// ---------------------------------------------------------------- sitrep
//
// `/client/v2/feed?destination=fye|records|reading`, `/client/shipments` and
// `/client/v2/fye/headline`, mirroring Model/WireTypes.swift (AgentFeed,
// Shipment, FyeHeadline). Same leniency: a row missing a required field is
// dropped, an unknown enum falls back, an absent optional is absent.

const num = (v: Json): number | null => (typeof v === "number" && Number.isFinite(v) ? v : null);
const optNum = (v: Json): number | undefined => num(v) ?? undefined;
const strList = (v: Json): string[] => (Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : []);

/** A time the model read off a message: an instant, a wall time, or a bare date. */
export interface SupportedTime {
  value: string;
  timezone?: string;
}

function supportedTime(v: Json): SupportedTime | undefined {
  if (!isObj(v)) return undefined;
  const value = str(v.value);
  return value === null ? undefined : { value, timezone: optStr(v.timezone) };
}

export interface RecordProposal {
  kind: string;
  merchant?: string;
  amount?: number;
  currency?: string;
  title?: string;
  start?: SupportedTime;
  institution?: string;
  description?: string;
  due?: SupportedTime;
  autopay?: boolean;
  change?: string;
}

function recordProposal(v: Json): RecordProposal | null {
  if (!isObj(v)) return null;
  const kind = str(v.kind);
  if (kind === null) return null;
  return {
    kind,
    merchant: optStr(v.merchant),
    amount: optNum(v.amount),
    currency: optStr(v.currency),
    title: optStr(v.title),
    start: supportedTime(v.start),
    institution: optStr(v.institution),
    description: optStr(v.description),
    due: supportedTime(v.due),
    autopay: optBool(v.autopay),
    change: optStr(v.change),
  };
}

export interface MessageDecision {
  kinds: string[];
  destinations: string[];
  reading_kind?: string;
  summary: string;
  reason: string;
  records: RecordProposal[];
  /** Auth mail (sign-in codes, resets): its kinds and whether it is critical. */
  auth?: { kinds: string[]; critical: boolean };
}

function messageDecision(v: Json): MessageDecision | null {
  if (!isObj(v)) return null;
  const summary = str(v.summary);
  if (summary === null) return null;
  const auth = isObj(v.auth) ? { kinds: strList(v.auth.kinds), critical: v.auth.critical === true } : undefined;
  return {
    kinds: strList(v.kinds),
    destinations: strList(v.destinations),
    reading_kind: optStr(v.reading_kind),
    summary,
    reason: str(v.reason) ?? "",
    records: rows(v.records, recordProposal),
    auth,
  };
}

export interface AttentionAction {
  kind?: string;
  description?: string;
  due?: SupportedTime;
  resolved: boolean;
}

function attentionAction(v: Json): AttentionAction | null {
  if (!isObj(v)) return null;
  return {
    kind: optStr(v.kind),
    description: optStr(v.description),
    due: supportedTime(v.due),
    resolved: v.resolved === true,
  };
}

export interface Attention {
  show_in_fye: boolean;
  state: string;
  summary: string;
  importance: number;
  attention_at?: SupportedTime;
  /** What the owner is asked to do, each with its own due date (newer daemons). */
  actions: AttentionAction[];
}

function attention(v: Json): Attention | null {
  if (!isObj(v)) return null;
  const f = isObj(v.factors) ? v.factors : {};
  return {
    show_in_fye: v.show_in_fye === true,
    state: str(v.state) ?? "",
    summary: str(v.summary) ?? "",
    importance: num(f.importance) ?? 0,
    attention_at: supportedTime(f.attention_at),
    actions: rows(v.actions, attentionAction),
  };
}

export interface FeedItem {
  message_id: number;
  thread_id: string;
  from_addr: string;
  from_name?: string;
  subject: string;
  received_at: string;
  decision: MessageDecision;
  attention: Attention;
  score: number;
}

export function feedItem(v: Json): FeedItem | null {
  if (!isObj(v)) return null;
  const message_id = int(v.message_id);
  const thread_id = str(v.thread_id);
  const from_addr = str(v.from_addr);
  const received_at = str(v.received_at);
  const decision = messageDecision(v.decision);
  const att = attention(v.attention);
  if (message_id === null || thread_id === null || from_addr === null || received_at === null || !decision || !att) {
    return null;
  }
  return {
    message_id,
    thread_id,
    from_addr,
    from_name: optStr(v.from_name),
    subject: str(v.subject) ?? "",
    received_at,
    decision,
    attention: att,
    score: num(v.score) ?? 0,
  };
}

export interface Feed {
  version: number;
  total_count?: number;
  items: FeedItem[];
}

/** Only version 2 is a feed this client can read; anything else is refused. */
export function feed(v: Json): Feed | null {
  if (!isObj(v) || v.version !== 2 || !Array.isArray(v.items)) return null;
  return { version: 2, total_count: optInt(v.total_count), items: rows(v.items, feedItem) };
}

/** Editorial content (an essay, a post) as opposed to a promotion (`isReadingContent`). */
export function isReadingContent(item: FeedItem): boolean {
  if (item.decision.reading_kind !== undefined) return item.decision.reading_kind === "content";
  return item.decision.kinds.includes("editorial") && !item.decision.kinds.includes("promotional");
}

export const CARRIERS = ["ups", "usps", "fedex", "dhl", "amazon", "unknown"] as const;
export type Carrier = (typeof CARRIERS)[number];
export const carrier = lenientEnum(CARRIERS, "unknown");

export const SHIPMENT_STATUSES = ["ordered", "shipped", "out_for_delivery", "delivered", "exception"] as const;
export type ShipmentStatus = (typeof SHIPMENT_STATUSES)[number];
export const shipmentStatus = lenientEnum(SHIPMENT_STATUSES, "ordered");

export interface ShipmentLeg {
  id: number;
  carrier: Carrier;
  status: ShipmentStatus;
  last_update: string;
  delivered_at?: string;
}

function shipmentLeg(v: Json): ShipmentLeg | null {
  if (!isObj(v)) return null;
  const id = int(v.id);
  const last_update = str(v.last_update);
  if (id === null || last_update === null) return null;
  return {
    id,
    carrier: carrier(v.carrier),
    status: shipmentStatus(v.status),
    last_update,
    delivered_at: optStr(v.delivered_at),
  };
}

export interface ShipmentOrder {
  merchant?: string;
  order_ref: string;
}

function shipmentOrder(v: Json): ShipmentOrder | null {
  if (!isObj(v)) return null;
  const order_ref = str(v.order_ref);
  return order_ref === null ? null : { merchant: optStr(v.merchant), order_ref };
}

export interface Shipment {
  id: number;
  carrier: Carrier;
  item_name: string;
  status: ShipmentStatus;
  thread_id?: string;
  first_seen: string;
  last_update: string;
  eta?: string;
  carrier_status_raw?: string;
  delivered_at?: string;
  poll_failures?: number;
  merchant?: string;
  /** Absent from an older daemon; present (possibly empty) on a newer one. */
  orders?: ShipmentOrder[];
  legs?: ShipmentLeg[];
}

export function shipment(v: Json): Shipment | null {
  if (!isObj(v)) return null;
  const id = int(v.id);
  const item_name = str(v.item_name);
  const first_seen = str(v.first_seen);
  const last_update = str(v.last_update);
  if (id === null || item_name === null || first_seen === null || last_update === null) return null;
  return {
    id,
    carrier: carrier(v.carrier),
    item_name,
    status: shipmentStatus(v.status),
    thread_id: optStr(v.thread_id),
    first_seen,
    last_update,
    eta: optStr(v.eta),
    carrier_status_raw: optStr(v.carrier_status_raw),
    delivered_at: optStr(v.delivered_at),
    poll_failures: optInt(v.poll_failures),
    merchant: optStr(v.merchant),
    orders: Array.isArray(v.orders) ? rows(v.orders, shipmentOrder) : undefined,
    legs: Array.isArray(v.legs) ? rows(v.legs, shipmentLeg) : undefined,
  };
}

export function shipments(v: Json): Shipment[] | null {
  return Array.isArray(v) ? rows(v, shipment) : null;
}

export interface FyeHeadline {
  headline?: string;
  /** False when the daemon's model call failed (worth asking again). Absent reads as settled. */
  settled: boolean;
}

export function fyeHeadline(v: Json): FyeHeadline | null {
  if (!isObj(v)) return null;
  const h = optStr(v.headline);
  return { headline: h && h.trim() ? h : undefined, settled: v.settled !== false };
}

// ---------------------------------------------------------------- search

/** One hit from `/client/search` (the Mac's `SearchHit`). */
export interface SearchHit {
  id: number;
  thread_id: string;
  from_addr: string;
  from_name?: string;
  subject: string;
  received_at: string;
  snippet: string;
  /** Absent from an older daemon: status unknown, not "unfinished". */
  is_done?: boolean;
  subject_matches?: string[];
  snippet_matches?: string[];
  from_me?: boolean;
}

export function searchHit(v: Json): SearchHit | null {
  if (!isObj(v)) return null;
  const id = int(v.id);
  const thread_id = str(v.thread_id);
  const from_addr = str(v.from_addr);
  const subject = str(v.subject);
  const received_at = str(v.received_at);
  const snippet = str(v.snippet);
  if (
    id === null ||
    thread_id === null ||
    from_addr === null ||
    subject === null ||
    received_at === null ||
    snippet === null
  ) {
    return null;
  }
  return {
    id,
    thread_id,
    from_addr,
    from_name: optStr(v.from_name),
    subject,
    received_at,
    snippet,
    is_done: optBool(v.is_done),
    subject_matches: Array.isArray(v.subject_matches) ? strList(v.subject_matches) : undefined,
    snippet_matches: Array.isArray(v.snippet_matches) ? strList(v.snippet_matches) : undefined,
    from_me: optBool(v.from_me),
  };
}

export interface SearchDiagnostics {
  strict_hits: number;
  any_hits: number;
}

export interface SearchPage {
  items: SearchHit[];
  next_cursor?: string;
  match_kind?: string;
  diagnostics?: SearchDiagnostics;
  /** Search covers mail from this date on. */
  indexed_since?: string;
}

export function searchPage(v: Json): SearchPage | null {
  if (!isObj(v) || !Array.isArray(v.items)) return null;
  const d = isObj(v.diagnostics) ? v.diagnostics : undefined;
  const strict = d ? int(d.strict_hits) : null;
  const any = d ? int(d.any_hits) : null;
  return {
    items: rows(v.items, searchHit),
    next_cursor: optStr(v.next_cursor),
    match_kind: optStr(v.match_kind),
    diagnostics: strict !== null && any !== null ? { strict_hits: strict, any_hits: any } : undefined,
    indexed_since: optStr(v.indexed_since),
  };
}

/** One row of `/client/senders`: who has written to this account. */
export interface SenderHit {
  addr: string;
  display_name?: string;
  msg_count: number;
}

export function senderHit(v: Json): SenderHit | null {
  if (!isObj(v)) return null;
  const addr = str(v.addr);
  if (addr === null) return null;
  return { addr, display_name: optStr(v.display_name), msg_count: int(v.msg_count) ?? 0 };
}
