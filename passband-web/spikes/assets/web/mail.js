// Shared by both pages: turn a thread message's sanitized html into srcdoc for
// a sandboxed frame, pointing every image at the daemon instead of the open
// internet.
//
// Rules this module enforces (and the tests assert):
//   - cid:X       -> <daemon>/client/attachments/<id> via the thread's content_id map
//   - http(s)://  -> <daemon>/client/image?u=<encoded original>   (never fetched directly)
//   - anything else (data:, javascript:, relative, file:) -> removed
//   - srcset is dropped: one URL per image, the one we rewrote
//   - the frame's CSP names exactly the image source the mode needs.

/** Parse inertly. A DOMParser document loads nothing and runs nothing. */
function parse(html) {
  return new DOMParser().parseFromString(`<!doctype html><body>${html}`, "text/html");
}

/**
 * @param {string} html     the message's sanitized html
 * @param {string} daemon   daemon origin, e.g. http://127.0.0.1:8850
 * @param {Array<{id:number, content_id?:string}>} attachments
 * @param {"sw"|"blob"} mode
 * @returns {{srcdoc: string, pending: string[]}} pending = daemon URLs the blob
 *   mode still has to fetch and assign (in document order of data-pb-src).
 */
export function prepare(html, daemon, attachments, mode) {
  const byCid = new Map(
    attachments.filter((a) => a.content_id).map((a) => [a.content_id.replace(/^<|>$/g, ""), a.id]),
  );
  const doc = parse(html);
  const pending = [];
  for (const img of doc.querySelectorAll("img")) {
    img.removeAttribute("srcset");
    const raw = (img.getAttribute("src") || "").trim();
    let target = null;
    if (/^cid:/i.test(raw)) {
      const id = byCid.get(decodeURIComponent(raw.slice(4)));
      if (id != null) target = `${daemon}/client/attachments/${id}`;
    } else if (/^https?:\/\//i.test(raw)) {
      target = `${daemon}/client/image?u=${encodeURIComponent(raw)}`;
    }
    img.removeAttribute("src");
    if (!target) continue;
    if (mode === "sw") {
      img.setAttribute("src", target);
    } else {
      img.setAttribute("data-pb-src", String(pending.length));
      pending.push(target);
    }
  }
  const imgSrc = mode === "sw" ? daemon : "blob:";
  const csp = `default-src 'none'; style-src 'unsafe-inline'; img-src ${imgSrc}`;
  const srcdoc =
    `<!doctype html><html><head>` +
    `<meta http-equiv="Content-Security-Policy" content="${csp}">` +
    `<meta name="referrer" content="no-referrer">` +
    `</head><body>${doc.body.innerHTML}</body></html>`;
  return { srcdoc, pending };
}

/** A sandboxed frame: same-origin so the parent can reach in, no scripts. */
export function frame(srcdoc, sandbox = "allow-same-origin") {
  const f = document.createElement("iframe");
  f.setAttribute("sandbox", sandbox);
  f.setAttribute("referrerpolicy", "no-referrer");
  f.style.cssText = "width:100%;border:0;height:600px";
  f.srcdoc = srcdoc;
  return f;
}

/** Resolves when every <img> in the frame has settled (loaded or failed). */
export function settled(f) {
  return new Promise((resolve) => {
    const go = () => {
      const imgs = [...f.contentDocument.images];
      Promise.all(
        imgs.map((i) =>
          // `complete` is true once loaded OR failed (or no src): either way
          // its event has already fired and waiting for it would hang.
          i.complete
            ? null
            : new Promise((r) => {
                i.addEventListener("load", r, { once: true });
                i.addEventListener("error", r, { once: true });
              }),
        ),
      ).then(() =>
        resolve(imgs.map((i) => ({ src: i.getAttribute("src"), ok: i.naturalWidth > 0 }))),
      );
    };
    if (f.contentDocument?.readyState === "complete") go();
    else f.addEventListener("load", go, { once: true });
  });
}

/** A synthetic newsletter: n images, all served by the daemon's real attachment route. */
export function newsletter(n, attachmentId) {
  let html = "<h1>Weekly digest</h1>";
  for (let i = 0; i < n; i++) {
    html += `<p>Item ${i}<br><img src="cid:img${i}" width="1" height="1"></p>`;
  }
  // Every cid maps to the same stored bytes, with a distinct query so each is
  // its own request (no HTTP-cache collapsing hides the per-image cost).
  const attachments = Array.from({ length: n }, (_, i) => ({ id: `${attachmentId}?n=${i}`, content_id: `img${i}` }));
  return { html, attachments };
}
