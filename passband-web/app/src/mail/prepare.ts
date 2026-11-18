// Turns a (daemon-sanitized) mail body into the document the reading frame
// loads. Ported from passband-web/spikes/email-render.
//
// This is the CLIENT pass, behind the daemon's sanitizer and in front of the
// frame's own sandbox + CSP. None of the three is meant to be the only one: the
// sandbox and the CSP must hold even if this pass misses something, and this
// pass removes what neither of them governs (a <base>, a meta refresh,
// <link rel=dns-prefetch>).
//
// WHY A PARSE AND RE-SERIALIZE, NOT A SPLICE. The Mac client edits the original
// string (Lib/HTMLImg.swift) so the frame parses exactly what the daemon
// sanitized, avoiding "mutation XSS", where markup changes meaning between two
// parses. Here that class is not what keeps mail inert. The frame is sandboxed
// WITHOUT allow-scripts, so nothing the second parse produces can run; and its
// CSP has no script-src and img-src `data: blob:`, so nothing it produces can
// fetch. What this pass does need is a real parser's view of attributes, CSS
// and nesting, which a hand-written splice over hostile HTML gets wrong. The
// parse is inert (DOMParser runs no script and loads nothing), and goes through
// the app's one Trusted Types policy (`parseMail`).
//
// IMAGES. No network URL survives into the document. A remote <img> loses its
// src and carries `data-pb-remote="<n>"`; a remote CSS url() becomes the inert
// `data:,pb-remote-<n>`; a cid: image carries `data-pb-cid`. The parent fills
// them in with blob: URLs (see images.ts): cid always, remote only after the
// reader opts in, and always through the daemon's proxy.

import { parseMail } from "./trusted";

export interface Prepared {
  /** The whole document, CSP first in <head>. */
  srcdoc: string;
  /** Remote image URLs, indexed by the placeholders in the document. */
  remotes: string[];
  /** Tracking pixels removed. */
  blockedTrackers: number;
  /** The frame's own policy, for tests. */
  csp: string;
}

// ---------------------------------------------------------------- policy

/**
 * The frame's own policy. `default-src 'none'` covers script, frame, font,
 * media, object and connect; `base-uri` and `form-action` are not covered by
 * default-src, so they are spelled out. Images: inline data and the parent's
 * blob: URLs only. The frame also inherits the app page's policy (a srcdoc
 * document does), and both must allow a load.
 */
export const MAIL_CSP =
  "default-src 'none'; style-src 'unsafe-inline'; img-src data: blob:; base-uri 'none'; form-action 'none'";

// ---------------------------------------------------------------- helpers (pure)

export function isNetworkSrc(src: string): boolean {
  const s = src.trim().toLowerCase();
  return s.startsWith("http://") || s.startsWith("https://") || s.startsWith("//");
}

/** `//host/x` → `https://host/x`; other network URLs unchanged. */
export function absoluteRemote(src: string): string {
  const s = src.trim();
  return s.startsWith("//") ? `https:${s}` : s;
}

export function pxDim(raw: string | null | undefined): number | null {
  if (raw == null) return null;
  const m = /^(\d+(?:\.\d+)?)(?:px)?$/i.exec(raw.trim());
  return m ? Number(m[1]) : null;
}

export function styleProp(style: string, prop: string): string | null {
  const m = new RegExp(`(?:^|;)\\s*${prop}\\s*:\\s*([^;]+)`, "i").exec(style);
  return m?.[1] ? m[1].trim() : null;
}

/** `<logo1>` / `cid:logo1` / `LOGO1` → `logo1`, for matching attachments. */
export function normalizeCid(raw: string): string {
  return raw.trim().replace(/^cid:/i, "").replace(/^<|>$/g, "").trim().toLowerCase();
}

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

export interface ImgAttrs {
  src?: string | null;
  width?: string | null;
  height?: string | null;
  style?: string | null;
}

export function isTracker(a: ImgAttrs): boolean {
  const style = a.style ?? "";
  const w = pxDim(a.width) ?? pxDim(styleProp(style, "width"));
  const h = pxDim(a.height) ?? pxDim(styleProp(style, "height"));
  if (w != null && h != null && w <= 2 && h <= 2) return true;
  const ls = style.toLowerCase();
  if (styleProp(ls, "display") === "none" || styleProp(ls, "visibility") === "hidden") return true;
  const src = a.src ?? "";
  return isNetworkSrc(src) && KNOWN_TRACKERS.some((re) => re.test(src));
}

/** The inert stand-in a remote CSS url() becomes until the reader opts in. */
export const remoteToken = (n: number) => `data:,pb-remote-${n}`;

/**
 * Rewrite every url() in a stylesheet or style attribute. Inline data images
 * stay; a remote URL becomes a numbered inert token (recorded in `remotes`);
 * everything else (cid:, relative, other schemes) becomes `none`. Best effort
 * only: what this misses (escapes, image-set strings) is refused by the CSP.
 */
export function rewriteCssUrls(css: string, remotes: string[]): string {
  return css.replace(/url\(\s*(['"]?)([^'")]*)\1\s*\)/gi, (_m, _q, raw: string) => {
    const u = raw.trim();
    if (/^data:image\//i.test(u)) return `url("${u}")`;
    if (isNetworkSrc(u)) {
      remotes.push(absoluteRemote(u));
      return `url("${remoteToken(remotes.length - 1)}")`;
    }
    return "none";
  });
}

/** Strip at-rules that fetch: @import and @font-face. */
export function stripFetchingAtRules(css: string): string {
  return css.replace(/@import[^;]*;?/gi, "").replace(/@font-face\s*\{[^}]*\}/gi, "");
}

/** Text for <pre>/<title>/attributes, never markup. */
export function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

/**
 * Our own sheet. The frame is pinned light, as on the Mac: mail is designed
 * for a white page, and inverting someone's newsletter breaks it.
 */
export const MAIL_SHEET =
  "html{background:#fff;color-scheme:light}html,body{margin:0}" +
  "body{padding:4px 2px;font:14px/1.5 -apple-system,BlinkMacSystemFont,system-ui,sans-serif;" +
  "color:#1d1d1f;overflow:hidden;overflow-wrap:anywhere}" +
  "img{max-width:100%;height:auto}" +
  "blockquote{margin:8px 0;padding-left:10px;border-left:2px solid #d8dee6;color:#555}";

export function frameHead(styles: string[] = []): string {
  return (
    `<meta http-equiv="Content-Security-Policy" content="${MAIL_CSP}">` +
    `<meta name="referrer" content="no-referrer">` +
    `<meta http-equiv="x-dns-prefetch-control" content="off">` +
    `<meta charset="utf-8">` +
    `<style id="passband-sheet">${MAIL_SHEET}</style>` +
    // A stylesheet can never close its own element: `</style` is escaped.
    styles.map((s) => `<style>${s.replace(/<\/(style)/gi, "<\\/$1")}</style>`).join("")
  );
}

// ---------------------------------------------------------------- the pass (DOM)

/** Elements that never belong in a reading frame. Removed with their content. */
const DROP = [
  "script", "iframe", "frame", "frameset", "object", "embed", "applet", "portal",
  "base", "link", "meta", "noscript", "template", "audio", "video", "source", "track",
  "svg", "math", "canvas", "dialog",
];

/** Elements whose children are kept but whose element is not. */
const UNWRAP = ["form", "button", "input", "select", "textarea", "picture"];

/** Attributes that can fetch something or carry script. */
const DROP_ATTRS = new Set([
  "srcset", "background", "poster", "ping", "action", "formaction", "lowsrc",
  "dynsrc", "longdesc", "usemap", "manifest", "xmlns", "target", "download",
  // A link's own referrer policy outranks the frame's no-referrer <meta>.
  "referrerpolicy", "rel",
]);

const SAFE_HREF = /^(https?:|mailto:)/i;

/**
 * Where a clicked link may go on its own, or null if it may not go anywhere.
 *
 * Every link click is caught from the parent (frame.ts) and the app opens it.
 * WebKit is the exception: it fires no listener at all inside a sandboxed
 * document that cannot run script, so in Safari the link itself is what
 * opens. That is why the frame carries `allow-popups
 * allow-popups-to-escape-sandbox`, and why every link that survives is sent to
 * a new tab with no opener and no referrer. The rule matches `safeLinkTarget`
 * in ui/links.ts: http(s) only, never with credentials in it.
 */
export function linkTarget(href: string): string | null {
  let u: URL;
  try {
    u = new URL(href.trim());
  } catch {
    return null;
  }
  if (u.protocol !== "https:" && u.protocol !== "http:") return null;
  if (u.username || u.password) return null;
  return u.toString();
}

export function prepareMail(html: string): Prepared {
  const doc = parseMail(html);
  const remotes: string[] = [];

  for (const sel of DROP) doc.querySelectorAll(sel).forEach((n) => n.remove());
  for (const sel of UNWRAP) {
    doc.querySelectorAll(sel).forEach((n) => n.replaceWith(...Array.from(n.childNodes)));
  }

  for (const el of Array.from(doc.querySelectorAll("*"))) {
    for (const attr of Array.from(el.attributes)) {
      const name = attr.name.toLowerCase();
      if (name.startsWith("on") || DROP_ATTRS.has(name)) {
        el.removeAttribute(attr.name);
      } else if (name === "href" || name === "xlink:href") {
        if (!SAFE_HREF.test(attr.value.trim())) el.removeAttribute(attr.name);
      } else if (name === "style") {
        el.setAttribute("style", rewriteCssUrls(attr.value, remotes));
      }
    }
  }

  // Links. http(s) opens in a new tab that cannot reach back or see where it
  // came from (Safari's only path, see linkTarget); anything else keeps no
  // place to go. mailto: stays readable and does nothing, as in the app.
  for (const a of Array.from(doc.querySelectorAll("a[href], area[href]"))) {
    const href = a.getAttribute("href") ?? "";
    if (/^mailto:/i.test(href.trim())) continue;
    const target = linkTarget(href);
    if (!target) {
      a.removeAttribute("href");
      continue;
    }
    a.setAttribute("href", target);
    a.setAttribute("target", "_blank");
    a.setAttribute("rel", "noopener noreferrer");
  }

  let blockedTrackers = 0;
  for (const img of Array.from(doc.querySelectorAll("img"))) {
    const src = (img.getAttribute("src") ?? "").trim();
    if (
      isTracker({
        src,
        width: img.getAttribute("width"),
        height: img.getAttribute("height"),
        style: img.getAttribute("style"),
      })
    ) {
      img.remove();
      blockedTrackers++;
      continue;
    }
    if (/^data:image\//i.test(src)) continue;
    img.removeAttribute("src");
    if (isNetworkSrc(src)) {
      remotes.push(absoluteRemote(src));
      img.setAttribute("data-pb-remote", String(remotes.length - 1));
    } else if (/^cid:/i.test(src)) {
      img.setAttribute("data-pb-cid", normalizeCid(src));
    }
    // Relative or any other scheme: nothing to load it from.
  }

  const styles: string[] = [];
  for (const s of Array.from(doc.querySelectorAll("style"))) {
    styles.push(stripFetchingAtRules(rewriteCssUrls(s.textContent ?? "", remotes)));
    s.remove();
  }

  const srcdoc = `<!doctype html><html><head>${frameHead(styles)}</head><body>${doc.body.innerHTML}</body></html>`;
  return { srcdoc, remotes, blockedTrackers, csp: MAIL_CSP };
}
