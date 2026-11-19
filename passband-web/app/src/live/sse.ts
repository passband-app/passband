// SSE framing for GET /client/events, ported from
// passband/Sources/Passband/Model/EventStream.swift (SSEParser and the byte
// loop in EventStream.frames). Pure: no network, no timers.

/** One dispatched frame: the `id:` in force when it was dispatched, plus the joined `data:` payload. */
export interface SSEFrame {
  id: string | null;
  data: string;
}

/**
 * Hard cap on one frame's accumulated data, and on one line. A frame is
 * notification display copy; past this the server is broken or hostile.
 */
export const MAX_FRAME_BYTES = 256 * 1024;

const utf8 = new TextEncoder();
const utf8Len = (s: string) => utf8.encode(s).length;

/**
 * Incremental frame assembler, fed one line at a time. Rules on this wire:
 * `:` keep-alive comments are ignored and are NOT a frame boundary; a trailing
 * CR is stripped; several `data:` lines join with "\n"; `id:` persists across
 * frames; unknown fields are skipped.
 */
export class SSEParser {
  private data: string[] = [];
  private bytes = 0;
  private overflowed = false;
  /** Survives dispatch: later frames without an `id:` inherit it. */
  private lastEventId: string | null = null;

  feed(rawLine: string): SSEFrame | null {
    let line = rawLine.endsWith("\r") ? rawLine.slice(0, -1) : rawLine;
    if (line.length === 0) return this.dispatch();
    if (line.startsWith(":")) return null;

    let field: string;
    let value: string;
    const colon = line.indexOf(":");
    if (colon >= 0) {
      field = line.slice(0, colon);
      value = line.slice(colon + 1);
      // Exactly ONE leading space is framing, not value.
      if (value.startsWith(" ")) value = value.slice(1);
    } else {
      field = line;
      value = "";
    }

    switch (field) {
      case "data":
        this.bytes += utf8Len(value) + 1;
        if (this.bytes > MAX_FRAME_BYTES) {
          this.overflowed = true;
          this.data = [];
        } else if (!this.overflowed) {
          this.data.push(value);
        }
        break;
      case "id":
        if (!value.includes("\0")) this.lastEventId = value;
        break;
      default:
        break;
    }
    return null;
  }

  private dispatch(): SSEFrame | null {
    const out =
      !this.overflowed && this.data.length > 0
        ? { id: this.lastEventId, data: this.data.join("\n") }
        : null;
    this.data = [];
    this.bytes = 0;
    this.overflowed = false;
    return out;
  }
}

/**
 * Splits a byte stream on LF (safe on UTF-8: 0x0A never appears inside a
 * multi-byte sequence) and keeps EMPTY lines, which are SSE's only frame
 * terminator. A line longer than the cap throws LineOverflow: the caller ends
 * the connection and backs off, as the Swift loop does.
 */
export class LineOverflow extends Error {}

export class LineSplitter {
  private pending: Uint8Array[] = [];
  private pendingLen = 0;
  private decoder = new TextDecoder("utf-8"); // non-fatal: invalid bytes become U+FFFD

  push(chunk: Uint8Array): string[] {
    const lines: string[] = [];
    let start = 0;
    for (let i = 0; i < chunk.length; i++) {
      if (chunk[i] !== 0x0a) continue;
      const piece = chunk.subarray(start, i);
      if (this.pendingLen + piece.length > MAX_FRAME_BYTES) throw new LineOverflow();
      lines.push(this.take(piece));
      start = i + 1;
    }
    const rest = chunk.subarray(start);
    if (rest.length) {
      if (this.pendingLen + rest.length > MAX_FRAME_BYTES) throw new LineOverflow();
      this.pending.push(rest.slice());
      this.pendingLen += rest.length;
    }
    return lines;
  }

  private take(tail: Uint8Array): string {
    if (this.pendingLen === 0) return this.decoder.decode(tail);
    const all = new Uint8Array(this.pendingLen + tail.length);
    let o = 0;
    for (const p of this.pending) {
      all.set(p, o);
      o += p.length;
    }
    all.set(tail, o);
    this.pending = [];
    this.pendingLen = 0;
    return this.decoder.decode(all);
  }
}

/** The daemon's `Event` row (squelch_core::types::Event), as the Swift client decodes it. */
export interface WireEvent {
  id: number;
  kind: string;
  message_id: number;
  thread_id: string;
  tier: string;
  importance: number;
  sender: string;
  one_line: string;
  deadline: string | null;
  created_at: string;
  sealed_kind?: string | null;
  is_auth?: boolean;
}

const isInt = (v: unknown): v is number => typeof v === "number" && Number.isInteger(v);
const isStr = (v: unknown): v is string => typeof v === "string";

/** Decode an Event the way Swift's synthesized Codable would: required keys typed, enums lenient. */
export function decodeEvent(data: string): WireEvent | null {
  let v: unknown;
  try {
    v = JSON.parse(data);
  } catch {
    return null;
  }
  if (typeof v !== "object" || v === null) return null;
  const o = v as Record<string, unknown>;
  if (
    !isInt(o.id) || !isStr(o.kind) || !isInt(o.message_id) || !isStr(o.thread_id) ||
    !isStr(o.tier) || !isInt(o.importance) || !isStr(o.sender) || !isStr(o.one_line) ||
    !isStr(o.created_at) || !(o.deadline === undefined || o.deadline === null || isStr(o.deadline))
  ) {
    return null;
  }
  return o as unknown as WireEvent;
}

/** Id-less side-channel frames (no cursor movement). */
export type Signal =
  | { kind: "open_draft"; payload: Record<string, unknown> }
  | { kind: "renotify"; payload: Record<string, unknown> }
  | { kind: "calendar_reminder"; payload: Record<string, unknown> };

export type Consumed =
  | { type: "event"; event: WireEvent; advanceTo: number }
  | { type: "signal"; signal: Signal }
  | { type: "undecodable"; advanceTo: number | null };

/**
 * What a frame means for the client, in the Swift `consume` order: an Event
 * advances the cursor; an id-less signal never does; an undecodable frame
 * with a numeric id STILL advances it, so one malformed row cannot wedge the
 * feed behind it on every reconnect.
 *
 * The open_draft signal is opt-in (`opens=1`), which the Mac sends and the
 * web client does not; renotify / calendar_reminder are only sent on that
 * opt-in stream too. Recognised here so a frame is never mistaken for an
 * undecodable event that moves the cursor.
 */
export function consume(frame: SSEFrame): Consumed {
  const event = decodeEvent(frame.data);
  if (event) return { type: "event", event, advanceTo: event.id };
  let o: unknown = null;
  try {
    o = JSON.parse(frame.data);
  } catch {
    /* fall through */
  }
  if (o && typeof o === "object") {
    const rec = o as Record<string, unknown>;
    if ("open_draft" in rec) return { type: "signal", signal: { kind: "open_draft", payload: rec } };
  }
  // renotify / calendar_reminder payloads are recognised by their SSE `event:`
  // name on the Mac; the web client never asks for them, so they fall to the
  // id rule below. That is safe only because those frames carry no `id:` of
  // their own and so inherit an id the cursor already holds (max-only).
  const n = frame.id !== null && /^-?\d+$/.test(frame.id) ? Number(frame.id) : null;
  return { type: "undecodable", advanceTo: n };
}
