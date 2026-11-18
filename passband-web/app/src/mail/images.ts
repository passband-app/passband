// Fills a prepared frame's image placeholders with blob: URLs, from the parent.
//
//   cid:    always. The bytes are part of the message (an attachment), so
//           loading them tells nobody anything.
//   remote: only after the reader opts in, and only through the ImageSource,
//           which fetches via the daemon's proxy. The browser never contacts
//           the sender's host.
//
// Every URL this creates is owned by one frame and revoked when it goes away.

import { normalizeCid, remoteToken } from "./prepare";
import type { Attachment } from "../api/wire";

export interface ImageSource {
  attachment(attachmentId: number, signal?: AbortSignal): Promise<Blob>;
  remote(url: string, signal?: AbortSignal): Promise<Blob>;
}

/** At most this many image fetches in flight, across every frame. */
export const MAX_IMAGE_FETCHES = 4;

export class Limiter {
  private active = 0;
  private queue: (() => void)[] = [];
  constructor(private readonly max: number) {}

  async run<T>(task: () => Promise<T>, signal?: AbortSignal): Promise<T> {
    if (this.active >= this.max) {
      await new Promise<void>((resolve, reject) => {
        const go = () => {
          signal?.removeEventListener("abort", onAbort);
          resolve();
        };
        const onAbort = () => {
          this.queue = this.queue.filter((q) => q !== go);
          reject(new DOMException("aborted", "AbortError"));
        };
        this.queue.push(go);
        signal?.addEventListener("abort", onAbort, { once: true });
      });
    }
    if (signal?.aborted) throw new DOMException("aborted", "AbortError");
    this.active++;
    try {
      return await task();
    } finally {
      this.active--;
      this.queue.shift()?.();
    }
  }

  get inFlight(): number {
    return this.active;
  }
}

const limiter = new Limiter(MAX_IMAGE_FETCHES);

/** The attachment a cid: reference names, by content id. */
export function cidIndex(attachments: Attachment[]): Map<string, number> {
  const m = new Map<string, number>();
  for (const a of attachments) if (a.content_id) m.set(normalizeCid(a.content_id), a.id);
  return m;
}

/**
 * Only image types become a blob the frame may draw. Anything else (an SVG, an
 * HTML page served as an "image") is dropped rather than retyped.
 */
const RASTER = /^image\/(png|jpe?g|gif|webp|avif|bmp|x-icon|vnd\.microsoft\.icon)$/i;

export class FrameImages {
  private urls = new Set<string>();
  private ctl = new AbortController();
  private resolved = new Map<number, string>();

  constructor(
    private readonly doc: Document,
    private readonly source: ImageSource,
    private readonly cids: Map<string, number>,
    private readonly remotes: string[],
    private readonly onChange: () => void,
  ) {}

  /** Inline (cid:) images: always loaded. */
  async loadInline(): Promise<void> {
    const imgs = Array.from(this.doc.querySelectorAll<HTMLImageElement>("img[data-pb-cid]"));
    await Promise.all(
      imgs.map(async (img) => {
        const id = this.cids.get(img.getAttribute("data-pb-cid") ?? "");
        if (id === undefined) return;
        const url = await this.blobUrl(() => this.source.attachment(id, this.ctl.signal));
        if (url) {
          img.src = url;
          this.onChange();
        }
      }),
    );
  }

  /** Remote images and CSS backgrounds: only when the reader asked. */
  async loadRemote(): Promise<void> {
    await Promise.all(
      this.remotes.map(async (remote, n) => {
        if (this.resolved.has(n)) return;
        const url = await this.blobUrl(() => this.source.remote(remote, this.ctl.signal));
        if (!url) return;
        this.resolved.set(n, url);
        for (const img of Array.from(
          this.doc.querySelectorAll<HTMLImageElement>(`img[data-pb-remote="${n}"]`),
        )) {
          img.src = url;
        }
        this.replaceToken(n, url);
        this.onChange();
      }),
    );
  }

  private replaceToken(n: number, url: string): void {
    const token = remoteToken(n);
    // Match the token exactly, never as a prefix of a longer one (1 vs 12).
    const re = new RegExp(`${token.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}(?=["')])`, "g");
    for (const el of Array.from(this.doc.querySelectorAll<HTMLElement>("[style]"))) {
      const s = el.getAttribute("style") ?? "";
      if (s.includes(token)) el.setAttribute("style", s.replace(re, url));
    }
    for (const st of Array.from(this.doc.querySelectorAll("style"))) {
      if (st.id === "passband-sheet") continue;
      const t = st.textContent ?? "";
      if (t.includes(token)) st.textContent = t.replace(re, url);
    }
  }

  private async blobUrl(fetchBlob: () => Promise<Blob>): Promise<string | null> {
    try {
      const blob = await limiter.run(fetchBlob, this.ctl.signal);
      if (this.ctl.signal.aborted || !RASTER.test(blob.type)) return null;
      const url = URL.createObjectURL(blob);
      this.urls.add(url);
      return url;
    } catch {
      return null;
    }
  }

  dispose(): void {
    this.ctl.abort();
    for (const u of this.urls) URL.revokeObjectURL(u);
    this.urls.clear();
  }
}
