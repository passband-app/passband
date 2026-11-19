// Drives a loaded reading frame FROM THE PARENT. The frame is sandboxed with
// allow-same-origin and without allow-scripts, so no script runs in it, ours
// included. Everything the Mac app's injected measuring script does
// (Views/EmailWebCore.swift) happens here instead, by reaching into the frame's
// document. Ported from passband-web/spikes/email-render.

export interface FrameCallbacks {
  onHeight(px: number): void;
  onQuoted(hasQuoted: boolean): void;
  /** An http(s) link the reader clicked. Nothing else is reported. */
  onLink(href: string): void;
}

export interface FrameHandle {
  setQuotesCollapsed(collapsed: boolean): void;
  measure(): void;
  /** Re-attach load listeners after the parent filled in image sources. */
  watchImages(): void;
  detach(): void;
}

// ---- quoted history: mirrors Lib/Quotes.swift and the injected script ----

const ATTRIBUTION = [
  /^On .{1,200} wrote:\s*$/,
  /^-{2,}\s*(original|forwarded) message\s*-{0,}$/i,
  /^Begin forwarded message:?\s*$/i,
];

export function isAttribution(t: string | null | undefined): boolean {
  const s = (t ?? "").trim();
  return s.length > 0 && ATTRIBUTION.some((re) => re.test(s));
}

/** Only http(s) leaves the frame; the app then decides whether to open it. */
export function capturedLink(href: string | null | undefined): string | null {
  const h = (href ?? "").trim();
  return /^https?:\/\//i.test(h) ? h : null;
}

function textLengthAfter(doc: Document, root: Node, node: Node): number {
  let len = 0;
  const walker = doc.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  let t: Node | null;
  while ((t = walker.nextNode())) {
    if (node.contains(t)) continue;
    if (node.compareDocumentPosition(t) & Node.DOCUMENT_POSITION_FOLLOWING) {
      len += (t.textContent ?? "").trim().length;
    }
  }
  return len;
}

export function findQuoteNodes(doc: Document): HTMLElement[] {
  const body = doc.body;
  if (!body) return [];
  const quotes = Array.from(body.querySelectorAll("blockquote")).filter(
    (b) => !(b.parentElement && b.parentElement.closest("blockquote")),
  );
  for (const q of quotes) {
    // Bottom-posting: substantial text after the quote means it is context
    // for what follows, not history to hide.
    if (textLengthAfter(doc, body, q) > 200) continue;
    const nodes: HTMLElement[] = [];
    const prev = (q.previousElementSibling ??
      (q.parentElement && q.parentElement.previousElementSibling)) as HTMLElement | null;
    if (
      prev &&
      (prev.textContent ?? "").trim().length < 300 &&
      isAttribution((prev.textContent ?? "").replace(/\s+/g, " "))
    ) {
      nodes.push(prev);
    }
    nodes.push(q);
    for (const r of quotes) {
      if (r !== q && q.compareDocumentPosition(r) & Node.DOCUMENT_POSITION_FOLLOWING) nodes.push(r);
    }
    return nodes;
  }
  return [];
}

/**
 * Content height, never documentElement.scrollHeight: the root's scrollHeight
 * is floored at the viewport, and the viewport IS the frame being sized.
 */
export function contentHeight(doc: Document): number {
  const b = doc.body;
  if (!b) return doc.documentElement?.scrollHeight ?? 0;
  return Math.max(Math.ceil(b.scrollHeight), Math.ceil(b.getBoundingClientRect().height));
}

export function attachFrame(iframe: HTMLIFrameElement, cb: FrameCallbacks): FrameHandle | null {
  const doc = iframe.contentDocument;
  if (!doc || !iframe.contentWindow) return null; // Not same-origin: the sandbox lost allow-same-origin.

  let last = -1;
  const measure = () => {
    const h = contentHeight(doc);
    if (h > 0 && Math.abs(h - last) > 1) {
      last = h;
      cb.onHeight(h);
    }
  };

  const quoteNodes = findQuoteNodes(doc);
  const setQuotesCollapsed = (collapsed: boolean) => {
    for (const n of quoteNodes) n.style.display = collapsed ? "none" : "";
    measure();
  };
  setQuotesCollapsed(true);
  cb.onQuoted(quoteNodes.length > 0);

  // The parent's ResizeObserver, observing an element in the child document.
  const ro = new ResizeObserver(measure);
  if (doc.body) ro.observe(doc.body);

  const watched = new Set<HTMLImageElement>();
  const watchImages = () => {
    for (const img of Array.from(doc.images)) {
      if (watched.has(img)) continue;
      watched.add(img);
      img.addEventListener("load", measure);
      img.addEventListener("error", measure);
    }
  };
  watchImages();

  // Capture phase on the child document, with a parent-realm function: the
  // sandbox disables scripting for the frame's own realm, not for ours. EVERY
  // click on a link is cancelled here, whatever its scheme: the sandbox does
  // not stop a frame navigating ITSELF, so this capture, the input gate in
  // MailFrame, and the app CSP's `frame-src 'self'` together are what keep a
  // link from loading inside the frame. WebKit never calls this (it fires no
  // listener in a script-less sandboxed document); there, the link's own
  // target=_blank from prepare.ts opens it instead.
  const onClick = (e: MouseEvent) => {
    const target = e.target as Element | null;
    const a = target && typeof target.closest === "function" ? target.closest("a[href], area[href]") : null;
    if (!a) return;
    e.preventDefault();
    const href = capturedLink(a.getAttribute("href"));
    if (href) cb.onLink(href);
  };
  doc.addEventListener("click", onClick, true);
  // Middle-click and keyboard activation take the same road.
  doc.addEventListener("auxclick", onClick, true);

  measure();
  requestAnimationFrame(measure);

  return {
    setQuotesCollapsed,
    measure,
    watchImages,
    detach() {
      ro.disconnect();
      for (const img of watched) {
        img.removeEventListener("load", measure);
        img.removeEventListener("error", measure);
      }
      doc.removeEventListener("click", onClick, true);
      doc.removeEventListener("auxclick", onClick, true);
    },
  };
}
