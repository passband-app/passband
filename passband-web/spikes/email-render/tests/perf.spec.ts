import { test } from '@playwright/test';
import { writeFileSync } from 'node:fs';
import { APP_ORIGIN } from './harness';

// Numbers, not assertions: these feed FINDINGS.md. One worker so runs don't
// contend with each other for CPU.
test.describe.configure({ mode: 'serial' });

const CASES = [
  { mail: 'newsletter', thread: 1 },
  { mail: 'long', thread: 1 },
  { mail: 'long', thread: 20 },
  { mail: 'gmail-reply', thread: 50 },
  { mail: 'newsletter', thread: 10 },
];
const results: unknown[] = [];

for (const c of CASES) {
  test(`perf ${c.mail} x${c.thread}`, async ({ page }) => {
    const cdp = await page.context().newCDPSession(page);
    await cdp.send('Performance.enable');
    const qs = new URLSearchParams({ mail: c.mail, thread: String(c.thread) });
    const runs: { allReadyMs: number; firstReadyMs: number; prepareMsTotal: number }[] = [];
    for (let i = 0; i < 3; i++) {
      await page.goto(`${APP_ORIGIN}/index.html?${qs}`);
      await page.waitForFunction((n) => window.__spike.ready.length >= n, c.thread, { timeout: 30_000 });
      runs.push(await page.evaluate(() => ({
        allReadyMs: Math.round(Math.max(...window.__spike.ready)),
        firstReadyMs: Math.round(Math.min(...window.__spike.ready)),
        prepareMsTotal: Math.round(window.__spike.prepare.reduce((a, b) => a + b, 0)),
      })));
    }
    await page.evaluate(() => (globalThis as { gc?: () => void }).gc?.());
    const { metrics } = await cdp.send('Performance.getMetrics');
    const m = Object.fromEntries(metrics.map((x) => [x.name, x.value]));
    const median = (k: keyof (typeof runs)[0]) => runs.map((r) => r[k]).sort((a, b) => a - b)[1];
    const r = {
      case: `${c.mail} x${c.thread}`,
      htmlKB: Math.round((await page.evaluate(() => document.querySelector('iframe')!.srcdoc.length)) / 1024),
      firstReadyMs: median('firstReadyMs'),
      allReadyMs: median('allReadyMs'),
      prepareMsTotal: median('prepareMsTotal'),
      frames: m.Frames,
      documents: m.Documents,
      nodes: m.Nodes,
      jsHeapMB: Math.round((m.JSHeapUsedSize / 1048576) * 10) / 10,
    };
    results.push(r);
    console.log(JSON.stringify(r));
    writeFileSync('test-results/perf.json', JSON.stringify(results, null, 2));
  });
}
