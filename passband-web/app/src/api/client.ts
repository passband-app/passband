// Typed calls for the routes the web client uses. Every call goes through the
// transport; nothing here sees a token or a server URL.
import { ApiError, type Transport } from "./transport";
import {
  feed,
  fyeHeadline,
  shipments,
  type Feed,
  type FyeHeadline,
  type Shipment,
  attentionUpdate,
  page,
  sendResult,
  stats,
  statusResult,
  threadView,
  type AttentionStatus,
  type AttentionUpdate,
  type Page,
  type SendResult,
  type Stats,
  type StatusResult,
  type ThreadView,
} from "./wire";

/** Gmail's inbox label: archive's undo puts it back. */
export const INBOX_LABEL = "INBOX";

export type Band = "standing" | "new" | "open";

export interface UpdatesParams {
  band?: Band;
  limit?: number;
  cursor?: string;
  since?: string;
}

async function decode<T>(resp: Response, one: (v: unknown) => T | null): Promise<T> {
  let json: unknown;
  try {
    json = await resp.json();
  } catch {
    throw new ApiError("decode");
  }
  const out = one(json);
  if (out === null) throw new ApiError("decode");
  return out;
}

export class Client {
  constructor(readonly transport: Transport) {}

  async stats(): Promise<Stats> {
    return decode(await this.transport.request("/client/stats"), stats);
  }

  async updates(p: UpdatesParams = {}): Promise<Page<AttentionUpdate>> {
    const resp = await this.transport.request("/client/updates", {
      query: { band: p.band, limit: p.limit, cursor: p.cursor, since: p.since },
    });
    return decode(resp, (v) => page(v, attentionUpdate));
  }

  async thread(threadId: string): Promise<ThreadView> {
    const resp = await this.transport.request(`/client/thread/${encodeURIComponent(threadId)}`);
    return decode(resp, threadView);
  }

  /** One agent feed: `fye` (for your eyes), `records`, or `reading`. */
  async feed(destination: "fye" | "records" | "reading", limit = 200): Promise<Feed> {
    const resp = await this.transport.request("/client/v2/feed", { query: { destination, limit } });
    return decode(resp, feed);
  }

  /** Packages, delivered ones included (the rail keeps today's deliveries). */
  async shipments(includeDelivered = true): Promise<Shipment[]> {
    const resp = await this.transport.request("/client/shipments", {
      query: { include_delivered: includeDelivered ? "true" : "false" },
    });
    return decode(resp, shipments);
  }

  /**
   * The sitrep's one-sentence headline. `today` is the owner's local date
   * (YYYY-MM-DD), because "due tomorrow" is a question about their calendar.
   */
  async fyeHeadline(today: string): Promise<FyeHeadline> {
    const resp = await this.transport.request("/client/v2/fye/headline", {
      query: { today },
      timeoutMs: 30_000,
    });
    return decode(resp, fyeHeadline);
  }

  /** Tell the daemon the thread was opened (read state; read tracking). */
  async opened(threadId: string): Promise<void> {
    await this.transport.request(`/client/thread/${encodeURIComponent(threadId)}/opened`, {
      method: "POST",
      body: {},
    });
  }

  async setStatus(messageId: number, status: AttentionStatus): Promise<StatusResult> {
    const resp = await this.transport.request(`/client/updates/${messageId}/status`, {
      method: "POST",
      body: { status },
    });
    return decode(resp, statusResult);
  }

  async archive(messageId: number): Promise<StatusResult> {
    const resp = await this.transport.request("/client/actions/archive", {
      method: "POST",
      body: { message_id: messageId, confirm: true },
    });
    return decode(resp, statusResult);
  }

  async label(messageId: number, add: string[] = [], remove: string[] = []): Promise<void> {
    await this.transport.request("/client/actions/label", {
      method: "POST",
      body: { message_id: messageId, add, remove, confirm: true },
    });
  }

  /**
   * Sign out: revoke THIS browser's token on the daemon, then forget it here.
   * The local credential goes even when the revoke cannot be confirmed (an
   * older daemon without the route, or no network), so signing out always
   * leaves this browser signed out.
   */
  async signOut(): Promise<void> {
    try {
      await this.transport.request("/client/session", { method: "DELETE", timeoutMs: 8_000 });
    } catch {
      // Fall through: forgetting locally is still right.
    } finally {
      await this.transport.forget();
    }
  }

  /** An attachment's bytes (also how `cid:` inline images are loaded). */
  async attachmentBlob(attachmentId: number, signal?: AbortSignal): Promise<Blob> {
    const resp = await this.transport.request(`/client/attachments/${attachmentId}`, {
      timeoutMs: 30_000,
      signal,
    });
    return resp.blob();
  }

  /**
   * A brand's favicon, fetched by the daemon (`/client/favicon`) so the
   * browser never contacts a third party. Only ever asked for robot and brand
   * domains (`eligibleFaviconDomain`), never a person's. A 404 means "no icon".
   */
  async favicon(domain: string, signal?: AbortSignal): Promise<Blob> {
    const resp = await this.transport.request("/client/favicon", {
      query: { domain },
      timeoutMs: 15_000,
      signal,
    });
    return resp.blob();
  }

  /**
   * A remote mail image, fetched by the daemon on the reader's behalf
   * (`/client/image`), so the browser never contacts the sender's host.
   */
  async remoteImageBlob(url: string, signal?: AbortSignal): Promise<Blob> {
    const resp = await this.transport.request("/client/image", {
      query: { u: url },
      timeoutMs: 20_000,
      signal,
    });
    return resp.blob();
  }

  /**
   * A reply, in markdown (the daemon renders the HTML half). `subject` is left
   * out so the daemon derives `Re: <parent subject>`, as the Mac client does.
   */
  async reply(replyToMessageId: number, body: string): Promise<SendResult> {
    const resp = await this.transport.request("/client/actions/send", {
      method: "POST",
      body: {
        reply_to_message_id: replyToMessageId,
        body,
        body_format: "markdown",
        confirm: true,
      },
      timeoutMs: 60_000,
    });
    return decode(resp, sendResult);
  }
}
