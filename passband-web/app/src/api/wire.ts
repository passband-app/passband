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
  total?: number;
  bands?: { standing?: number; new?: number; open?: number };
}

export function stats(v: Json): Stats | null {
  if (!isObj(v)) return null;
  const b = isObj(v.bands) ? v.bands : undefined;
  return {
    account_email: optStr(v.account_email),
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
