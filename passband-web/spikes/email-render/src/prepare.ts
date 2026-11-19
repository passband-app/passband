// Turns a (daemon-sanitized) mail body into the srcdoc the reading frame loads.
//
// This is the CLIENT pass, behind the daemon's ammonia and in front of the
// frame's own sandbox + CSP. None of the three is meant to be the only one:
// the sandbox and the CSP must hold even if this pass misses something, and
// this pass removes what neither of them governs (a <base>, a meta refresh,
// <link rel=dns-prefetch>).
//
// It parses with DOMParser, whose documents are inert: no script runs and no
// resource is fetched while we read or edit them.

export interface PrepareOptions {
  /** The per-message remote-image decision. Off: no network image at all. */
  allowRemote: boolean;
  /** Where remote images are fetched through, e.g. `https://tenant/client/image?u=`. */
  proxyBase: string;
  /** Our own stylesheet, injected ahead of the mail's. */
  sheet?: string;
}

export interface Prepared {
  srcdoc: string;
  /** Tracking pixels removed. */
  blockedTrackers: number;
  /** The mail references at least one network image (offer "load images"). */
  hasRemoteImages: boolean;
  /** The policy written into the frame, for tests and for the debug overlay. */
  csp: string;
}

// ---------------------------------------------------------------- policy

export function proxyOrigin(proxyBase: string): string {
  return new URL(proxyBase).origin;
}

/**
 * The frame's policy. `default-src 'none'` covers script, frame, font, media,
 * object and connect. `base-uri` and `form-action` are NOT covered by
 * default-src, so they are spelled out.
 */
export function mailCSP(allowRemote: boolean, proxyBase: string): string {
  const img = allowRemote ? `data: ${proxyOrigin(proxyBase)}` : 'data:';
  return [
    "default-src 'none'",
    "style-src 'unsafe-inline'",
    `img-src ${img}`,
    "base-uri 'none'",
    "form-action 'none'",
  ].join('; ');
}

// ---------------------------------------------------------------- trackers
// Port of Lib/Trackers.swift. Same asymmetric bias: strip only on strong
// signals (tiny declared size, CSS-hidden, a known endpoint).

const KNOWN_TRACKERS = [
  /list-manage\.com\/track\/open/i,
  /ct\.sendgrid\.net\/wf\/open/i,
  /email\.mailgun\.net\/o\//i,
  /pstmrk\.it/i,
  /t\.hubspotemail\.net/i,
  /track\.hubspot\.com\/__ptq\.gif/i,
  /pi\.pardot\.com\/open/i,
  /track\.customer\.io\/e\/o/i,
  /google-analytics\.com\/collect/i,
  /doubleclick\.net/i,
];

function isNetworkSrc(src: string): boolean {
  const s = src.trim().toLowerCase();
  return s.startsWith('http://') || s.startsWith('https://') || s.startsWith('//');
}

function pxDim(raw: string | null | undefined): number | null {
  if (raw == null) return null;
  const m = /^(\d+(?:\.\d+)?)(?:px)?$/i.exec(raw.trim());
  return m ? Number(m[1]) : null;
}

function styleProp(style: string, prop: string): string | null {
  const m = new RegExp(`(?:^|;)\\s*${prop}\\s*:\\s*([^;]+)`, 'i').exec(style);
  return m ? m[1].trim() : null;
}

export function isTracker(img: Element): boolean {
  const style = img.getAttribute('style') ?? '';
  const w = pxDim(img.getAttribute('width')) ?? pxDim(styleProp(style, 'width'));
  const h = pxDim(img.getAttribute('height')) ?? pxDim(styleProp(style, 'height'));
  if (w != null && h != null && w <= 2 && h <= 2) return true;
  const ls = style.toLowerCase();
  if (styleProp(ls, 'display') === 'none' || styleProp(ls, 'visibility') === 'hidden') return true;
  const src = img.getAttribute('src') ?? '';
  return isNetworkSrc(src) && KNOWN_TRACKERS.some((re) => re.test(src));
}

// ---------------------------------------------------------------- the pass

/** Elements that never belong in a reading frame. Removed with their content. */
const DROP = [
  'script', 'iframe', 'frame', 'frameset', 'object', 'embed', 'applet', 'portal',
  'base', 'link', 'meta', 'noscript', 'template', 'audio', 'video', 'source', 'track',
  'svg', 'math', 'canvas', 'dialog',
];

/** Elements whose children are kept but whose element is not. */
const UNWRAP = ['form', 'button', 'input', 'select', 'textarea', 'picture'];

/** Attributes that can fetch something or carry script. */
const DROP_ATTRS = new Set([
  'srcset', 'background', 'poster', 'ping', 'action', 'formaction', 'lowsrc',
  'dynsrc', 'longdesc', 'usemap', 'manifest', 'xmlns', 'target', 'download',
]);

const SAFE_HREF = /^(https?:|mailto:)/i;

function rewriteCssUrls(css: string, allowRemote: boolean, proxyBase: string): string {
  // Best effort only; the frame's img-src is what actually decides. A remote
  // url() becomes the proxy (images on) or nothing (images off). Anything this
  // misses (image-set("…") strings, escapes) is refused by the CSP.
  return css.replace(/url\(\s*(['"]?)([^'")]*)\1\s*\)/gi, (_m, _q, raw: string) => {
    const u = raw.trim();
    if (/^data:image\//i.test(u)) return `url("${u}")`;
    if (isNetworkSrc(u) && allowRemote) {
      const abs = u.startsWith('//') ? `https:${u}` : u;
      return `url("${proxyBase}${encodeURIComponent(abs)}")`;
    }
    return 'none';
  });
}

export function prepareMail(html: string, opts: PrepareOptions): Prepared {
  const { allowRemote, proxyBase } = opts;
  const doc = new DOMParser().parseFromString(html, 'text/html');

  for (const sel of DROP) doc.querySelectorAll(sel).forEach((n) => n.remove());
  for (const sel of UNWRAP) {
    doc.querySelectorAll(sel).forEach((n) => n.replaceWith(...Array.from(n.childNodes)));
  }

  let blockedTrackers = 0;
  let hasRemoteImages = false;

  for (const el of Array.from(doc.querySelectorAll('*'))) {
    for (const attr of Array.from(el.attributes)) {
      const name = attr.name.toLowerCase();
      if (name.startsWith('on') || DROP_ATTRS.has(name)) {
        el.removeAttribute(attr.name);
        continue;
      }
      if (name === 'href' || name === 'xlink:href') {
        if (!SAFE_HREF.test(attr.value.trim())) el.removeAttribute(attr.name);
        continue;
      }
      if (name === 'style') {
        if (/url\(/i.test(attr.value) && /(?:https?:)?\/\//i.test(attr.value)) hasRemoteImages = true;
        el.setAttribute('style', rewriteCssUrls(attr.value, allowRemote, proxyBase));
      }
    }
  }

  for (const img of Array.from(doc.querySelectorAll('img'))) {
    if (isTracker(img)) {
      img.remove();
      blockedTrackers++;
      continue;
    }
    const src = (img.getAttribute('src') ?? '').trim();
    if (/^data:image\//i.test(src)) continue;
    if (isNetworkSrc(src)) {
      hasRemoteImages = true;
      const abs = src.startsWith('//') ? `https:${src}` : src;
      if (allowRemote) {
        img.setAttribute('src', proxyBase + encodeURIComponent(abs));
      } else {
        img.removeAttribute('src');
        img.setAttribute('data-pb-src', abs);
      }
      continue;
    }
    // cid:, relative, anything else: nothing to fetch it from in this spike.
    img.removeAttribute('src');
  }

  const styles = Array.from(doc.querySelectorAll('style'));
  for (const s of styles) {
    if (/url\(\s*['"]?(?:https?:)?\/\//i.test(s.textContent ?? '')) hasRemoteImages = true;
    s.textContent = rewriteCssUrls(s.textContent ?? '', allowRemote, proxyBase)
      .replace(/@import[^;]*;?/gi, '')
      .replace(/@font-face\s*\{[^}]*\}/gi, '');
    s.remove();
  }

  const csp = mailCSP(allowRemote, proxyBase);
  const head =
    `<meta http-equiv="Content-Security-Policy" content="${csp}">` +
    `<meta name="referrer" content="no-referrer">` +
    `<meta http-equiv="x-dns-prefetch-control" content="off">` +
    `<meta charset="utf-8">` +
    `<style id="passband-sheet">${opts.sheet ?? DEFAULT_SHEET}</style>` +
    styles.map((s) => `<style>${s.textContent}</style>`).join('');

  const srcdoc = `<!doctype html><html><head>${head}</head><body>${doc.body.innerHTML}</body></html>`;
  return { srcdoc, blockedTrackers, hasRemoteImages, csp };
}

export const DEFAULT_SHEET =
  'html,body{margin:0}' +
  'body{padding:16px;font:14px/1.5 -apple-system,system-ui,sans-serif;color:#1d1d1f;' +
  'overflow:hidden;overflow-wrap:anywhere}' +
  'img{max-width:100%;height:auto}' +
  'blockquote{margin:8px 0;padding-left:10px;border-left:2px solid #d8dee6;color:#555}';

/**
 * FRAME-ONLY: the raw mail behind nothing but the frame's own head. Used by the
 * tests to prove the sandbox + CSP hold with the client pass switched off.
 */
export function wrapRaw(html: string, allowRemote: boolean, proxyBase: string): Prepared {
  const csp = mailCSP(allowRemote, proxyBase);
  const head =
    `<meta http-equiv="Content-Security-Policy" content="${csp}">` +
    `<meta name="referrer" content="no-referrer">` +
    `<meta http-equiv="x-dns-prefetch-control" content="off">` +
    `<style id="passband-sheet">${DEFAULT_SHEET}</style>`;
  return {
    srcdoc: `<!doctype html><html><head>${head}</head><body>${html}</body></html>`,
    blockedTrackers: 0,
    hasRemoteImages: false,
    csp,
  };
}
