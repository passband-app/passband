import { expect, test } from '@playwright/test';
import { HOSTILE } from '../src/corpus';
import { APP_ORIGIN, record, waitForFrame } from './harness';

// Each mail, in five configurations. The last two strip every layer but the
// frame's own (sandbox + CSP meta): no client pass, no inherited app policy.
const MODES = [
  { name: 'full', path: '/index.html', raw: false, images: false },
  { name: 'full+images', path: '/index.html', raw: false, images: true },
  { name: 'frame-only', path: '/index.html', raw: true, images: false },
  { name: 'frame-only/no-app-csp', path: '/nocsp.html', raw: true, images: false },
  { name: 'frame-only/no-app-csp+images', path: '/nocsp.html', raw: true, images: true },
];

for (const mode of MODES) {
  test.describe(mode.name, () => {
    for (const mail of HOSTILE) {
      test(mail.id, async ({ page }) => {
        const rec = await record(page);
        const qs = new URLSearchParams({ mail: mail.id });
        if (mode.raw) qs.set('raw', '1');
        if (mode.images) qs.set('images', '1');
        const url = `${APP_ORIGIN}${mode.path}?${qs}`;
        await page.goto(url);
        await waitForFrame(page);

        const frame = page.frameLocator('iframe').first();
        if (mail.hover) await frame.locator(mail.hover).hover({ timeout: 2000 }).catch(() => {});
        if (mail.click) await frame.locator(mail.click).click({ timeout: 2000 }).catch(() => {});
        // Let any deferred request or refresh fire.
        await page.waitForTimeout(1200);

        // 1. No script from the mail ran anywhere.
        expect(await page.evaluate(() => window.__pwned)).toBeUndefined();
        expect(await page.title()).toBe('email-render spike');

        // 2. Nothing left for an origin outside the allowlist. With images on,
        //    the proxy is the ONE extra origin, and only in that state.
        expect(rec.offending(mode.images)).toEqual([]);
        expect(rec.unexplained().filter((u) => !u.startsWith(APP_ORIGIN))).toEqual([]);
        expect(rec.requests.some((u) => u.includes('evil.example') && !u.startsWith('https://proxy.passband.test'))).toBe(false);

        // 3. Neither the page nor the mail frame went anywhere; no window opened.
        expect(page.url()).toBe(url);
        expect(rec.popups).toBe(0);
        // The MAIL frame stays the srcdoc document. A frame the mail itself
        // declares (frame-only mode) may exist, but only as a refused load:
        // the CSP turns it into the browser's error page, never a remote one.
        const mailFrame = page.frames()[1];
        expect(mailFrame.url()).toBe('about:srcdoc');
        for (const f of page.frames().slice(2)) {
          expect(['about:srcdoc', 'about:blank', 'chrome-error://chromewebdata/']).toContain(f.url());
        }
        if (!mode.raw) expect(page.frames().length).toBe(2);

        // 4. The secret never appears in any request.
        expect(rec.requests.some((u) => u.includes('secret-device-token'))).toBe(false);
      });
    }
  });
}
