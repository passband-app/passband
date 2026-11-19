// Spike 2: authenticated images in the web client.
//
//   node --test test/                      (from passband-web/spikes/assets)
//
// Needs a spike daemon (squelch-api + spike_seed) on DAEMON, default
// http://127.0.0.1:8850, bearer TOKEN (default spike-token), and Chromium via
// the preinstalled Playwright.
import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { serveStatic, serveElsewhere } from "../server.mjs";

const require = createRequire(import.meta.url);
const { chromium } = require("playwright");

const DAEMON = process.env.DAEMON || "http://127.0.0.1:8850";
const TOKEN = process.env.TOKEN || "spike-token";
const PAGE = "http://127.0.0.1:5185";
const ELSEWHERE = "http://127.0.0.1:5186";
const REMOTE_HOST = "images.shop.example"; // the sender's image host in the seed mail

let browser, site, elsewhere;

before(async () => {
  site = await serveStatic(5185);
  elsewhere = await serveElsewhere(5186);
  browser = await chromium.launch();
});
after(async () => {
  await browser?.close();
  site?.close();
  elsewhere?.server.close();
});

/** A fresh profile (first visit: no SW yet) that records every request. */
async function visit() {
  const context = await browser.newContext();
  const requests = [];
  context.on("request", (r) =>
    requests.push({
      url: r.url(),
      fromSW: !!r.serviceWorker(),
      headers: r.headers(),
      req: r,
    }),
  );
  const page = await context.newPage();
  await page.goto(`${PAGE}/?daemon=${encodeURIComponent(DAEMON)}&token=${TOKEN}`);
  await page.waitForFunction(() => document.title === "ready");
  return { context, page, requests };
}

/** Subresource URLs only: the one document load carries the test-only query. */
function subresourceUrls(requests) {
  return requests.map((r) => r.url).filter((u) => !u.startsWith(`${PAGE}/?`));
}

function assertHygiene(requests) {
  for (const u of subresourceUrls(requests)) {
    assert.ok(!u.includes(TOKEN), `token in a URL: ${u}`);
  }
  const direct = requests.filter((r) => new URL(r.url).hostname === REMOTE_HOST);
  assert.equal(direct.length, 0, `direct request to the sender's image host: ${direct.map((r) => r.url)}`);
}

test("SW: cid and attachment images load inside a srcdoc frame (allow-same-origin, no scripts)", async () => {
  const { context, page, requests } = await visit();
  await page.evaluate(() => window.spike.swReady());
  const { imgs } = await page.evaluate(() => window.spike.thread("t-shop", "sw"));
  const cid = imgs.find((i) => i.src.includes("/client/attachments/"));
  assert.ok(cid?.ok, `cid image should load: ${JSON.stringify(imgs)}`);
  // The remote image went to the daemon proxy, not the sender's host. The
  // daemon can't reach the internet here (502), so it renders broken — the
  // point is WHERE the request went, and that it was authenticated.
  const proxied = requests.filter((r) => r.url.startsWith(`${DAEMON}/client/image?u=`));
  assert.ok(proxied.length >= 1, "remote image routed via /client/image");
  assert.ok(
    proxied.some((r) => r.fromSW && r.headers.authorization === `Bearer ${TOKEN}`),
    "the SW's outgoing proxy request carried the bearer",
  );
  // Frame subresources went through the parent's worker: the SW-initiated
  // fetches are the authed ones.
  const swAuthed = requests.filter((r) => r.fromSW && r.url.startsWith(`${DAEMON}/client/attachments/`));
  assert.ok(swAuthed.length >= 1, "frame <img> fetched via the service worker");
  assertHygiene(requests);
  await context.close();
});

test("SW: an opaque-origin frame (sandbox without allow-same-origin) is NOT controlled — images fail", async () => {
  const { context, page, requests } = await visit();
  await page.evaluate(() => window.spike.swReady());
  const { imgs } = await page.evaluate(() => window.spike.thread("t-shop", "sw", ""));
  assert.equal(imgs, null, "the parent cannot see into an opaque frame");
  // The frame's own image requests reached the daemon WITHOUT the bearer
  // (no worker in the path) and were refused.
  const frameReqs = requests.filter((r) => r.url.startsWith(`${DAEMON}/client/attachments/`));
  assert.ok(frameReqs.length >= 1, "the opaque frame did request its images");
  assert.ok(frameReqs.every((r) => !r.fromSW && !r.headers.authorization), JSON.stringify(frameReqs.map((r) => [r.url, r.fromSW])));
  for (const r of frameReqs) assert.equal((await r.req.response())?.status() ?? 0, 401);
  assertHygiene(requests);
  await context.close();
});

test("SW: first visit before the worker controls the page — images fail closed (401), token never leaks", async () => {
  const { context, page, requests } = await visit();
  // Render WITHOUT swReady: no controller yet.
  assert.equal(await page.evaluate(() => window.spike.controlled()), false);
  const { imgs } = await page.evaluate(() => window.spike.thread("t-shop", "sw"));
  assert.ok(imgs.every((i) => !i.ok), "uncontrolled first render has no auth");
  const unauthed = requests.filter((r) => r.url.startsWith(`${DAEMON}/client/attachments/`));
  assert.ok(unauthed.length >= 1 && unauthed.every((r) => !r.headers.authorization));
  assertHygiene(requests);
  await context.close();
});

test("SW: survives the worker being stopped (token restored from IndexedDB)", async () => {
  const { context, page, requests } = await visit();
  await page.evaluate(() => window.spike.swReady());
  const cdp = await context.newCDPSession(page);
  await cdp.send("ServiceWorker.enable");
  await cdp.send("ServiceWorker.stopAllWorkers");
  const { imgs } = await page.evaluate(() => window.spike.thread("t-shop", "sw"));
  assert.ok(imgs.find((i) => i.src.includes("/client/attachments/"))?.ok, JSON.stringify(imgs));
  assertHygiene(requests);
  await context.close();
});

test("SW: never adds the bearer to another origin, nor to daemon paths outside the image allowlist", async () => {
  const { context, page } = await visit();
  await page.evaluate(() => window.spike.swReady());
  elsewhere.seen.length = 0;
  // Another origin, and a path on it that mimics the guarded one.
  await page.evaluate((u) => window.spike.pageImg(u), `${ELSEWHERE}/x.png`);
  await page.evaluate((u) => window.spike.pageImg(u), `${ELSEWHERE}/client/attachments/1`);
  assert.equal(elsewhere.seen.length, 2);
  assert.ok(elsewhere.seen.every((r) => !r.headers.authorization), JSON.stringify(elsewhere.seen));
  // A daemon path outside the allowlist: fetched without the bearer -> 401 -> broken.
  const ok = await page.evaluate((u) => window.spike.pageImg(u), `${DAEMON}/client/stats`);
  assert.equal(ok, false);
  await context.close();
});

test("blob: fetch-with-bearer then blob: URLs in the frame (CSP img-src blob:)", async () => {
  const { context, page, requests } = await visit();
  const { imgs, blobs } = await page.evaluate(() => window.spike.thread("t-shop", "blob"));
  assert.ok(blobs.length >= 1);
  assert.ok(imgs.find((i) => i.src?.startsWith("blob:"))?.ok, JSON.stringify(imgs));
  // Every daemon request is the page's own authed fetch; the frame asked for nothing.
  const daemonReqs = requests.filter((r) => r.url.startsWith(DAEMON) && !r.url.startsWith(`${DAEMON}/client/thread/`));
  assert.ok(daemonReqs.length >= 1 && daemonReqs.every((r) => r.headers.authorization === `Bearer ${TOKEN}`), JSON.stringify(daemonReqs.map((r) => r.url)));
  assertHygiene(requests);
  await context.close();
});

test("perf: a 50-image newsletter, both modes", async () => {
  const out = {};
  for (const mode of ["sw", "blob"]) {
    const { context, page, requests } = await visit();
    if (mode === "sw") await page.evaluate(() => window.spike.swReady());
    const cdp = await context.newCDPSession(page);
    await cdp.send("HeapProfiler.collectGarbage");
    const before = (await cdp.send("Runtime.getHeapUsage")).usedSize;
    const r = await page.evaluate((m) => window.spike.newsletter(50, 2, m), mode);
    await cdp.send("HeapProfiler.collectGarbage");
    const afterHeap = (await cdp.send("Runtime.getHeapUsage")).usedSize;
    assert.equal(r.loaded, 50, `${mode}: all 50 load`);
    out[mode] = { ms: Math.round(r.ms), heapDeltaKB: Math.round((afterHeap - before) / 1024), blobs: r.blobs };
    assertHygiene(requests);
    await context.close();
  }
  console.log("PERF", JSON.stringify(out));
});
