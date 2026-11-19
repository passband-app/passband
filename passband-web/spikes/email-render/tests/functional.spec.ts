import { expect, test } from '@playwright/test';
import { APP_ORIGIN, frameGeometry, record, waitForFrame } from './harness';

const open = (mail: string, extra: Record<string, string> = {}) =>
  `${APP_ORIGIN}/index.html?${new URLSearchParams({ mail, ...extra })}`;

test('the frame sandbox is exactly allow-same-origin', async ({ page }) => {
  await page.goto(open('long'));
  await waitForFrame(page);
  expect(await page.locator('iframe').getAttribute('sandbox')).toBe('allow-same-origin');
  const csp = await page.locator('iframe').getAttribute('data-csp');
  expect(csp).toBe("default-src 'none'; style-src 'unsafe-inline'; img-src data:; base-uri 'none'; form-action 'none'");
});

test('a long mail is sized to its content', async ({ page }) => {
  await page.goto(open('long'));
  await waitForFrame(page);
  await page.waitForTimeout(200);
  const g = await frameGeometry(page);
  expect(g.frame).toBeGreaterThan(1500);
  expect(Math.abs(g.frame - g.body)).toBeLessThanOrEqual(2);
  expect(g.contentBottom).toBeLessThanOrEqual(g.frame + 1);
});

test('the frame grows when a late image lands', async ({ page }) => {
  await record(page, { proxyDelayMs: 800 });
  await page.goto(open('late-image', { images: '1' }));
  await waitForFrame(page);
  const before = (await frameGeometry(page)).frame;
  await page.waitForFunction(
    (b) => Number(document.querySelector('iframe')!.getAttribute('data-height')) > b + 300,
    before,
    { timeout: 5000 },
  );
  const g = await frameGeometry(page);
  expect(g.frame - before).toBeGreaterThanOrEqual(380);
  expect(Math.abs(g.frame - g.body)).toBeLessThanOrEqual(2);
});

for (const mail of ['gmail-reply', 'wrote-reply']) {
  test(`quoted history collapses: ${mail}`, async ({ page }) => {
    await page.goto(open(mail));
    await waitForFrame(page);
    const toggle = page.locator('.quoted-toggle');
    await expect(toggle).toHaveText('show quoted text');
    const frame = page.frameLocator('iframe');
    await expect(frame.locator('blockquote').first()).toBeHidden();
    await expect(frame.getByText(/wrote:/)).toBeHidden();
    const collapsed = (await frameGeometry(page)).frame;

    await toggle.click();
    await expect(frame.locator('blockquote').first()).toBeVisible();
    await page.waitForFunction(
      (c) => Number(document.querySelector('iframe')!.getAttribute('data-height')) > c,
      collapsed,
    );
    await toggle.click();
    await expect(frame.locator('blockquote').first()).toBeHidden();
  });
}

test('a bottom-posted reply keeps its quote', async ({ page }) => {
  await page.goto(open('bottom-posted'));
  await waitForFrame(page);
  await expect(page.locator('.quoted-toggle')).toHaveCount(0);
  await expect(page.frameLocator('iframe').locator('blockquote')).toBeVisible();
});

test('links reach the callback and never navigate', async ({ page }) => {
  const rec = await record(page);
  const url = open('links');
  await page.goto(url);
  await waitForFrame(page);
  const frame = page.frameLocator('iframe');
  for (const id of ['https', 'http', 'mailto', 'ftp', 'rel', 'hash', 'inner']) {
    await frame.locator(`#${id}`).click();
  }
  await frame.locator('#https').click({ button: 'middle' });
  await frame.locator('#https').click({ modifiers: ['ControlOrMeta'] });
  await page.waitForTimeout(300);
  const links = await page.evaluate(() => window.__spike.links);
  expect(links.slice(0, 4)).toEqual([
    'https://links.example/a',
    'http://links.example/b',
    'mailto:dee@mail.example',
    'https://links.example/nested',
  ]);
  expect(page.url()).toBe(url);
  expect(page.frames()[1].url()).toBe('about:srcdoc');
  expect(rec.popups).toBe(0);
  expect(rec.offending(false)).toEqual([]);
});

test("mail CSS cannot style the parent page", async ({ page }) => {
  await page.goto(open('style-parent', { raw: '1' }));
  await waitForFrame(page);
  const marker = page.locator('#parent-marker');
  await expect(marker).toBeVisible();
  expect(await marker.evaluate((e) => getComputedStyle(e).backgroundColor)).toBe('rgb(240, 240, 240)');
  expect(await page.evaluate(() => getComputedStyle(document.body).backgroundColor)).toBe('rgba(0, 0, 0, 0)');
});

test('the frame cannot reach the parent; the parent can reach the frame', async ({ page }) => {
  await page.goto(open('long'));
  await waitForFrame(page);
  const r = await page.evaluate(() => {
    const f = document.querySelector('iframe')!;
    // Parent → frame: works (that is how we measure).
    const parentReads = !!f.contentDocument?.body;
    // The frame's realm cannot run script, so its own window cannot be used
    // to evaluate anything: a javascript: navigation and a handler attribute
    // set from outside both stay inert.
    const d = f.contentDocument!;
    const p = d.createElement('p');
    p.setAttribute('onclick', 'top.__pwned="injected-handler"');
    d.body.appendChild(p);
    p.click();
    const s = d.createElement('script');
    s.textContent = 'top.__pwned="injected-script"';
    d.body.appendChild(s);
    return { parentReads, pwned: (window as Window).__pwned ?? null };
  });
  expect(r.parentReads).toBe(true);
  expect(r.pwned).toBeNull();
});

test('trackers are stripped and remote images wait for opt-in', async ({ page }) => {
  const rec = await record(page);
  await page.goto(open('trackers'));
  await waitForFrame(page);
  expect(await page.locator('iframe').getAttribute('data-trackers')).toBe('3');

  await page.goto(open('remote-img'));
  await waitForFrame(page);
  await page.waitForTimeout(300);
  expect(await page.locator('iframe').getAttribute('data-remote')).toBe('1');
  expect(rec.requests.filter((u) => !u.startsWith(APP_ORIGIN))).toEqual([]);

  await page.goto(open('remote-img', { images: '1' }));
  await waitForFrame(page);
  await page.waitForTimeout(300);
  expect(rec.requests.filter((u) => !u.startsWith(APP_ORIGIN))).toEqual([
    'https://proxy.passband.test/client/image?u=' + encodeURIComponent('https://images.example/hero.png'),
  ]);
});

test('the frame takes no input until the click capture is attached', async ({ page }) => {
  // Watch the attribute from the first frame the iframe exists.
  await page.addInitScript(() => {
    (window as unknown as { __states: string[] }).__states = [];
    new MutationObserver(() => {
      const f = document.querySelector('iframe');
      if (f) (window as unknown as { __states: string[] }).__states.push(
        `${f.getAttribute('data-interactive')}:${f.style.pointerEvents}:${f.style.visibility}`);
    }).observe(document, { subtree: true, attributes: true, childList: true });
  });
  await page.goto(open('links'));
  await waitForFrame(page);
  const states = await page.evaluate(() => (window as unknown as { __states: string[] }).__states);
  expect(states[0]).toBe('0:none:hidden');
  expect(states.at(-1)).toBe('1:auto:visible');
  // Never interactive while the capture flag says otherwise.
  for (const s of states) expect(['0:none:hidden', '1:auto:visible']).toContain(s);
});

for (const variant of [
  { page: 'index.html', appCsp: true },
  { page: 'nocsp.html', appCsp: false },
]) {
  test(`a frame that navigates away is reset (capture off, app CSP ${variant.appCsp ? 'on' : 'off'})`, async ({ page }) => {
    const rec = await record(page);
    const url = `${APP_ORIGIN}/${variant.page}?mail=links&nolinkcapture=1`;
    await page.goto(url);
    await waitForFrame(page);
    await page.frameLocator('iframe').locator('#https').click();
    // The guard sees the frame leave its srcdoc document and replaces it.
    await page.waitForFunction(() => window.__spike.escaped.length > 0, null, { timeout: 5000 });
    // The remount lands a render later than the escape is reported.
    await expect.poll(() => page.frames()[1]?.url(), { timeout: 5000 }).toBe('about:srcdoc');
    await page.waitForFunction(() => document.querySelector('iframe')?.getAttribute('data-interactive') === '1');
    expect(page.url()).toBe(url);
    expect(rec.popups).toBe(0);
    if (variant.appCsp) {
      // The app page's frame-src 'self' refuses the frame's self-navigation:
      // the capture is not the only thing standing here.
      expect(rec.offending(false)).toEqual([]);
    } else {
      // Without it, the request leaves. This is the path the capture, the
      // input gate and the app's frame-src exist to close.
      expect(rec.requests).toContain('https://links.example/a');
    }
  });
}
