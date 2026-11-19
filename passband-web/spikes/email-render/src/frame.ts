// Drives a loaded reading frame FROM THE PARENT. The frame is sandboxed with
// allow-same-origin and without allow-scripts, so no script runs in it, ours
// included. Everything the Mac app's injected measuringScript does
// (Views/EmailWebCore.swift) happens here instead, by reaching into the frame's
// document.

export interface FrameCallbacks {
  onHeight(px: number): void;
  onQuoted(hasQuoted: boolean): void;
  /** An http(s) or mailto link the reader clicked. Nothing else is reported. */
  onLink(href: string): void;
}

export interface FrameHandle {
  setQuotesCollapsed(collapsed: boolean): void;
  measure(): void;
  detach(): void;
}

// ---- quoted history: mirrors Lib/Quotes.swift and the injected script ----

const ATTRIBUTION = [
  /^On .{1,200} wrote:\s*$/,
  /^-{2,}\s*(original|forwarded) message\s*-{0,}$/i,
  /^Begin forwarded message:?\s*$/i,
];

export function isAttribution(t: string | null | undefined): boolean {
  const s = (t ?? '').trim();
  return s.length > 0 && ATTRIBUTION.some((re) => re.test(s));
}

function textLengthAfter(doc: Document, root: Node, node: Node): number {
  let len = 0;
  const walker = doc.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  let t: Node | null;
  while ((t = walker.nextNode())) {
    if (node.contains(t)) continue;
    if (node.compareDocumentPosition(t) & Node.DOCUMENT_POSITION_FOLLOWING) {
      len += (t.textContent ?? '').trim().length;
    }
  }
  return len;
}

export function findQuoteNodes(doc: Document): HTMLElement[] {
  const body = doc.body;
  if (!body) return [];
  const quotes = Array.from(body.querySelectorAll('blockquote')).filter(
    (b) => !(b.parentElement && b.parentElement.closest('blockquote')),
  );
  for (const q of quotes) {
    if (textLengthAfter(doc, body, q) > 200) continue;
    const nodes: HTMLElement[] = [];
    const prev = (q.previousElementSibling ??
      (q.parentElement && q.parentElement.previousElementSibling)) as HTMLElement | null;
    if (
      prev &&
      (prev.textContent ?? '').trim().length < 300 &&
      isAttribution((prev.textContent ?? '').replace(/\s+/g, ' '))
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

// ---- height ----

/**
 * Content height, never documentElement.scrollHeight: the root's scrollHeight
 * is floored at the viewport, and the viewport IS the frame being sized.
 */
export function contentHeight(doc: Document): number {
  const b = doc.body;
  if (!b) return doc.documentElement?.scrollHeight ?? 0;
  return Math.max(Math.ceil(b.scrollHeight), Math.ceil(b.getBoundingClientRect().height));
}

// ---- attach ----

export interface AttachOptions {
  /** TEST ONLY: leave links uncaptured, to exercise the escape guard. */
  noLinkCapture?: boolean;
}

export function attachFrame(
  iframe: HTMLIFrameElement,
  opts: AttachOptions,
  cb: FrameCallbacks,
): FrameHandle | null {
  const doc = iframe.contentDocument;
  const win = iframe.contentWindow;
  if (!doc || !win) return null; // Not same-origin: the sandbox lost allow-same-origin.

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
    for (const n of quoteNodes) n.style.display = collapsed ? 'none' : '';
    measure();
  };
  setQuotesCollapsed(true);
  cb.onQuoted(quoteNodes.length > 0);

  // The parent's ResizeObserver, observing an element in the child document.
  const ro = new ResizeObserver(measure);
  if (doc.body) ro.observe(doc.body);

  const onImg = () => measure();
  const imgs = Array.from(doc.images);
  for (const img of imgs) {
    img.addEventListener('load', onImg);
    img.addEventListener('error', onImg);
  }

  // Capture phase on the child document, with a parent-realm function: the
  // sandbox disables scripting for the frame's own realm, not for ours.
  const onClick = (e: MouseEvent) => {
    const target = e.target as Element | null;
    const a = target && typeof target.closest === 'function' ? target.closest('a[href]') : null;
    if (!a) return;
    e.preventDefault();
    const href = a.getAttribute('href') ?? '';
    if (/^(https?:\/\/|mailto:)/i.test(href)) cb.onLink(href);
  };
  if (!opts.noLinkCapture) {
    doc.addEventListener('click', onClick, true);
    // Middle-click and keyboard activation take the same road.
    doc.addEventListener('auxclick', onClick, true);
  }

  measure();
  requestAnimationFrame(measure);

  return {
    setQuotesCollapsed,
    measure,
    detach() {
      ro.disconnect();
      for (const img of imgs) {
        img.removeEventListener('load', onImg);
        img.removeEventListener('error', onImg);
      }
      doc.removeEventListener('click', onClick, true);
      doc.removeEventListener('auxclick', onClick, true);
    },
  };
}
