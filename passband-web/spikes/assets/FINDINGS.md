# Spike 2: authenticated images, attachments and the image proxy

**Verdict: go.**

## Tests

- **Rust:** 8 unit tests in `squelch-api/src/image_proxy.rs`.
- **Browser:** 7 Playwright tests (`node --test test/assets.test.mjs`).

## Daemon: `GET /client/image?u=<url>`

The route sits inside the `/client` tree, so it needs the bearer. It fetches a
remote image on the reader's behalf, and the browser never contacts the sender's
image host.

**Which URLs it will fetch**
- http and https only, on ports 80 and 443 only.
- Names resolve through the unsubscribe fetcher's `PublicOnly` resolver, which
  keeps public addresses only. The connection goes to exactly the addresses the
  resolver kept, so a name cannot pass the check and then connect somewhere
  private.
- Redirects are followed by hand, at most 3 hops, and every hop is checked
  again.
- https is tried first.

**What it sends**
- No cookies, no `Referer`, no `Authorization`.
- One generic `User-Agent`.

**What it returns**
- The type is sniffed from the bytes, and only PNG, JPEG, GIF, WebP and AVIF are
  served. SVG and HTML are refused even when labelled `image/*`.
- Bytes go out through the same `attachment_response` headers as attachments
  (`nosniff` and friends).

**Limits**
- 8 MB per image and a 10 s timeout.
- At most 8 fetches in flight.
- Errors never echo the URL.

## Browser: getting authenticated bytes into the page

| | Service worker adds the header | Fetch with bearer, then a `blob:` URL |
|---|---|---|
| Plain `<img>` in the app | works | works |
| `<img>` inside the sandboxed `srcdoc` mail frame (`allow-same-origin`) | works: the frame is controlled by the parent's worker in Chromium | works with `img-src blob:` in the frame CSP |
| Before the worker controls the page (first visit, hard reload) | fails closed (401) | works |
| Token visible to page script | no | yes |
| 50-image newsletter (Chromium, small images) | ~120 ms | ~130 ms |

In every mode:
- the token never appears in a URL;
- the bearer is never sent to another origin, or to a daemon path outside the
  allow-list;
- no request reaches the sender's image host.

## Decision

Spike 2 alone would favour blob URLs, because they work before the worker takes
control.

The web client's token-custody decision changes that. If the token lives only
in the service worker, so page script never reads it:
- **Images:** the worker attaches the bearer. The page either lets `<img>` load
  through the worker directly, or asks for the bytes and wraps them in a `blob:`
  URL. Either way the page never sees the token.
- **Before the worker controls the page:** nothing authenticated can happen,
  images or JSON. So the app shell checks `navigator.serviceWorker.controller`
  and reloads until it is controlled. A hard reload (which bypasses the worker)
  is handled the same way.

Pairing then runs inside the worker too, so the token is never in page memory,
even at the moment it is minted.

## Still to check by hand in Safari and Firefox

- A `srcdoc` frame being controlled by the parent's service worker.
- `blob:` images under the frame CSP.
- Memory with full-size images (the spike's images were tiny).
