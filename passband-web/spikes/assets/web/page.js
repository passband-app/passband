// Test harness page. Exposes window.spike.* for the Playwright tests; there is
// no UI to speak of. Config comes from the query string: ?daemon=&token=
import { prepare, frame, settled, newsletter } from "./mail.js";

const q = new URLSearchParams(location.search);
const DAEMON = q.get("daemon");
const TOKEN = q.get("token");
// The token arrives in the query string ONLY because this is a test page; the
// tests strip it from the URL bar before any mail renders (history.replaceState)
// and assert no subresource URL ever carries it.
history.replaceState(null, "", location.pathname);

async function api(path) {
  const r = await fetch(`${DAEMON}${path}`, {
    headers: { Authorization: `Bearer ${TOKEN}` },
    credentials: "omit",
    redirect: "error",
  });
  if (!r.ok) throw new Error(`${path}: ${r.status}`);
  return r.json();
}

async function swReady() {
  const reg = await navigator.serviceWorker.register("/sw.js", { scope: "/" });
  await navigator.serviceWorker.ready;
  if (!navigator.serviceWorker.controller) {
    await new Promise((r) => navigator.serviceWorker.addEventListener("controllerchange", r, { once: true }));
  }
  const ack = new Promise((r) =>
    navigator.serviceWorker.addEventListener("message", (e) => e.data?.type === "config-ok" && r(), { once: true }),
  );
  navigator.serviceWorker.controller.postMessage({ type: "config", origin: DAEMON, token: TOKEN });
  await ack;
  return reg;
}

/** Fetch every pending daemon URL with the bearer and assign blob: URLs in the frame. */
async function fillBlobs(f, pending) {
  const doc = f.contentDocument;
  const urls = [];
  await Promise.all(
    pending.map(async (u, i) => {
      const img = doc.querySelector(`img[data-pb-src="${i}"]`);
      try {
        const r = await fetch(u, {
          headers: { Authorization: `Bearer ${TOKEN}` },
          credentials: "omit",
          redirect: "error",
          referrerPolicy: "no-referrer",
        });
        if (!r.ok) throw new Error(String(r.status));
        const url = URL.createObjectURL(await r.blob());
        urls.push(url);
        img.src = url;
      } catch {
        img.removeAttribute("data-pb-src"); // broken image, no request elsewhere
      }
    }),
  );
  return urls;
}

async function render(html, attachments, mode, sandbox) {
  const host = document.getElementById("mail");
  host.replaceChildren();
  const { srcdoc, pending } = prepare(html, DAEMON, attachments, mode);
  const f = frame(srcdoc, sandbox);
  host.append(f);
  await new Promise((r) => f.addEventListener("load", r, { once: true }));
  // An opaque-origin frame (sandbox without allow-same-origin) cannot be
  // inspected from here at all: give its requests a moment and report nothing.
  if (!f.contentDocument) {
    await new Promise((r) => setTimeout(r, 1500));
    return { imgs: null, blobs: [] };
  }
  let blobs = [];
  if (mode === "blob") blobs = await fillBlobs(f, pending);
  const imgs = await settled(f);
  return { imgs, blobs };
}

window.spike = {
  daemon: DAEMON,
  swReady,
  controlled: () => !!navigator.serviceWorker.controller,
  async thread(id, mode, sandbox) {
    const t = await api(`/client/thread/${id}`);
    const m = t.messages.find((x) => x.html) || t.messages[0];
    return render(m.html, m.attachments, mode, sandbox);
  },
  async newsletter(n, attachmentId, mode) {
    const { html, attachments } = newsletter(n, attachmentId);
    const t0 = performance.now();
    const out = await render(html, attachments, mode);
    return { ms: performance.now() - t0, loaded: out.imgs.filter((i) => i.ok).length, blobs: out.blobs.length };
  },
  /** An <img> in the PAGE (no frame, no CSP) at an arbitrary URL. */
  pageImg(src) {
    return new Promise((r) => {
      const i = new Image();
      i.onload = () => r(true);
      i.onerror = () => r(false);
      i.src = src;
      document.body.append(i);
    });
  },
};
document.title = "ready";
