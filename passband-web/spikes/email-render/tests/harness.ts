import type { Page, Request } from '@playwright/test';

export const APP_ORIGIN = 'http://localhost:5182';
export const PROXY_ORIGIN = 'https://proxy.passband.test';

export interface Recorder {
  /** Requests that reached the network layer (every one passes the route). */
  requests: string[];
  /** Attempts the renderer refused on CSP before any network activity. */
  cspBlocked: string[];
  /** Request events that neither reached the route nor failed on CSP. */
  unexplained(): string[];
  navigations: string[];
  popups: number;
  console: string[];
  /** Requests to any origin outside the allowlist. */
  offending(allowProxy: boolean): string[];
}

const svg = (w: number, h: number) =>
  `<svg xmlns="http://www.w3.org/2000/svg" width="${w}" height="${h}"><rect width="${w}" height="${h}" fill="#7ab"/></svg>`;

/**
 * Records EVERY request the page or any of its frames makes. App assets pass;
 * the stand-in image proxy answers with an SVG (optionally late); everything
 * else is aborted after being recorded.
 */
export async function record(page: Page, opts: { proxyDelayMs?: number } = {}): Promise<Recorder> {
  const attempts: string[] = [];
  const rec: Recorder = {
    requests: [],
    cspBlocked: [],
    unexplained() {
      return attempts.filter(
        (u) => !rec.requests.includes(u) && !rec.cspBlocked.includes(u) && !u.startsWith('data:'),
      );
    },
    navigations: [],
    popups: 0,
    console: [],
    offending(allowProxy) {
      return rec.requests.filter((u) => {
        if (u.startsWith('data:') || u.startsWith('about:')) return false;
        const o = new URL(u).origin;
        if (o === APP_ORIGIN) return false;
        if (allowProxy && o === PROXY_ORIGIN) return false;
        return true;
      });
    },
  };
  await page.context().route('**/*', async (route) => {
    const url = route.request().url();
    rec.requests.push(url);
    const origin = new URL(url).origin;
    if (origin === APP_ORIGIN) return route.continue();
    if (origin === PROXY_ORIGIN) {
      if (opts.proxyDelayMs) await new Promise((r) => setTimeout(r, opts.proxyDelayMs));
      const late = url.includes(encodeURIComponent('late.png'));
      return route.fulfill({
        status: 200,
        contentType: 'image/svg+xml',
        body: late ? svg(600, 400) : svg(10, 10),
      });
    }
    return route.abort();
  });
  // Playwright reports a request event even for a load the renderer then
  // refuses on CSP (requestfailed: "csp"). Those never reach the network; they
  // are recorded apart so a test can say WHICH layer stopped a vector, and any
  // attempt that is neither routed nor CSP-refused fails the test.
  page.on('request', (r: Request) => { attempts.push(r.url()); });
  page.on('requestfailed', (r: Request) => {
    // "csp": refused by a Content-Security-Policy. "origin": refused by the
    // renderer's own same-origin rule (SVG <use> external references).
    const why = r.failure()?.errorText;
    if (why === 'csp' || why === 'origin') rec.cspBlocked.push(r.url());
  });
  page.on('framenavigated', (f) => rec.navigations.push(f.url()));
  page.on('popup', () => { rec.popups++; });
  page.on('console', (m) => rec.console.push(m.text()));
  return rec;
}

export async function waitForFrame(page: Page) {
  await page.waitForFunction(() => {
    const f = document.querySelector('iframe');
    return !!f && Number(f.getAttribute('data-height')) > 0;
  });
}

/** Height of the frame's content as the PARENT reads it (no script runs in the frame). */
export async function frameGeometry(page: Page, index = 0) {
  return page.evaluate((i) => {
    const f = document.querySelectorAll('iframe')[i] as HTMLIFrameElement;
    const d = f.contentDocument!;
    const body = d.body.getBoundingClientRect();
    const all = Array.from(d.body.querySelectorAll('*')).filter((e) => e.getClientRects().length > 0);
    const bottom = Math.max(0, ...all.map((e) => e.getBoundingClientRect().bottom));
    return { frame: f.clientHeight, body: body.height, contentBottom: bottom };
  }, index);
}
