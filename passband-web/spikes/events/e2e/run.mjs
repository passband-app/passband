// End-to-end proof in Chromium against a running spike daemon.
//   SPIKE_DB=<path to spike.db> node e2e/run.mjs        (after `npm run build`)
// Env: DAEMON (default http://127.0.0.1:8849), TOKEN (default spike-token),
//      LONG=1 to also run the 35s hidden-tab ping check.
import { createRequire } from "node:module";
// The preinstalled Playwright (do not install another: it would not find the browser).
const { chromium } = createRequire(import.meta.url)(process.env.PLAYWRIGHT_PATH ?? "/opt/node-tools/node_modules/playwright");
import { execFileSync } from "node:child_process";
import { serve } from "./server.mjs";

const DAEMON = process.env.DAEMON ?? "http://127.0.0.1:8849";
const TOKEN = process.env.TOKEN ?? "spike-token";
const DB = process.env.SPIKE_DB;
if (!DB) throw new Error("set SPIKE_DB");
const PAGE = "http://localhost:5174/";
const tool = new URL("../tools/add_events.py", import.meta.url).pathname;
const addEvents = (n) => execFileSync("python3", ["-I", tool, DB, String(n)]).toString().trim().split("\n").map(Number);
const latestId = () => Number(execFileSync("python3", ["-I", "-c",
  "import sqlite3,sys;print(sqlite3.connect(sys.argv[1]).execute('select coalesce(max(id),0) from events').fetchone()[0])", DB]).toString());
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function until(fn, ms = 10_000, step = 50) {
  const end = Date.now() + ms;
  for (;;) {
    const v = await fn();
    if (v) return v;
    if (Date.now() > end) throw new Error("timed out waiting");
    await sleep(step);
  }
}

let failures = 0;
const results = [];
async function check(name, fn) {
  try {
    const note = await fn();
    results.push(`PASS ${name}${note ? ` — ${note}` : ""}`);
  } catch (e) {
    failures++;
    results.push(`FAIL ${name} — ${e.message}`);
  }
}
const assert = (c, m) => { if (!c) throw new Error(m); };

const srv = await serve();
const browser = await chromium.launch();
const ctx = await browser.newContext();
const page = await ctx.newPage();
const cdp = await ctx.newCDPSession(page);
await cdp.send("Network.enable");
const net = [];
cdp.on("Network.requestWillBeSent", (e) => net.push({ kind: "req", id: e.requestId, url: e.request.url, method: e.request.method, type: e.type, headers: e.request.headers }));
cdp.on("Network.responseReceived", (e) => net.push({ kind: "res", id: e.requestId, url: e.response.url, status: e.response.status, type: e.type, headers: e.response.headers }));
await page.goto(PAGE);

const start = (opts = {}) => page.evaluate((o) => window.spike.start(o), { serverUrl: DAEMON, token: TOKEN, ...opts });
const stop = () => page.evaluate(() => window.spike.stop());
const frames = () => page.evaluate(() => [...window.spike.log.frames]);
const clearLog = () => page.evaluate(() => { const l = window.spike.log; l.frames.length = 0; l.statuses.length = 0; l.consumed.length = 0; });

// 1 ─ cross-origin fetch stream with a bearer; what the preflight answered.
await check("1. cross-origin stream opens with Authorization: Bearer (preflight passes)", async () => {
  const base = latestId();
  await page.evaluate((v) => window.spike.setCursor("spike", v), base);
  await start();
  await until(() => page.evaluate(() => window.spike.log.statuses.some((s) => s.s === "open")));
  const pre = net.find((n) => n.kind === "res" && n.url.includes("/client/events") && (n.type === "Preflight" || n.status === 204 || n.headers?.["access-control-allow-headers"]) && !n.headers?.["content-type"]?.includes("event-stream"));
  const main = net.find((n) => n.kind === "res" && n.url.includes("/client/events") && n.headers?.["content-type"]?.includes("event-stream"));
  assert(main && main.status === 200, "no 200 event-stream response");
  const req = net.find((n) => n.kind === "req" && n.id === main.id);
  assert(!req.url.includes(TOKEN), "token leaked into URL");
  assert(!main.headers["content-encoding"], `stream was encoded: ${main.headers["content-encoding"]}`);
  await stop();
  return `preflight ${pre ? JSON.stringify({ status: pre.status, acao: pre.headers["access-control-allow-origin"], acah: pre.headers["access-control-allow-headers"], acam: pre.headers["access-control-allow-methods"] }) : "not surfaced by CDP (see curl in FINDINGS)"}; stream ACAO=${main.headers["access-control-allow-origin"]}, no content-encoding`;
});

// 2 ─ replay, cursor, abort + reconnect: nothing replayed, nothing lost.
await check("2. events parse, cursor advances, reconnect with after= neither replays nor loses", async () => {
  await clearLog();
  const base = latestId();
  await page.evaluate((v) => window.spike.setCursor("spike", v), base);
  const a = addEvents(3);
  await start();
  await until(async () => (await frames()).length >= 3);
  assert(JSON.stringify(await frames()) === JSON.stringify(a), `first batch ${await frames()} != ${a}`);
  assert((await page.evaluate(() => window.spike.cursor())) === a[2], "cursor did not reach last id");
  const stored = await page.evaluate(() => localStorage.getItem("passband.events.lastSeen.spike"));
  assert(stored === String(a[2]), `persisted cursor ${stored}`);
  await stop(); // abort mid-stream
  const b = addEvents(2);
  await clearLog();
  await start(); // a fresh EventStream reads the persisted cursor
  await until(async () => (await frames()).length >= 2);
  await sleep(500);
  const got = await frames();
  assert(JSON.stringify(got) === JSON.stringify(b), `second batch ${got} != ${b} (replayed or lost)`);
  await stop();
  return `batch1 ${a.join(",")} → batch2 ${b.join(",")}`;
});

// 2b ─ cursorless first run is live-only.
await check("2b. first run with no stored cursor receives no backlog", async () => {
  await clearLog();
  await page.evaluate(() => window.spike.setCursor("fresh", null));
  addEvents(1);
  await start({ account: "fresh" });
  await until(() => page.evaluate(() => window.spike.log.statuses.some((s) => s.s === "open")));
  await sleep(600);
  assert((await frames()).length === 0, "backlog delivered on a cursorless connect");
  const url = net.filter((n) => n.kind === "req" && n.url.includes("/client/events")).at(-1).url;
  assert(!url.includes("after="), `cursorless connect sent ${url}`);
  await stop();
});

// 3 ─ unreachable / aborted → backoff, then catch-up via after=.
await check("3. daemon unreachable → widening backoff; recovery catches up via after=", async () => {
  await clearLog();
  await page.evaluate(() => { window.spike.timing.backoffBase = 0.1; window.spike.timing.backoffCap = 0.8; });
  const base = latestId();
  await page.evaluate((v) => window.spike.setCursor("spike", v), base);
  let aborts = 0;
  await page.route("**/client/events**", (route) => { aborts++; return route.abort("connectionrefused"); });
  await start();
  await until(() => page.evaluate(() => window.spike.log.statuses.filter((s) => s.s === "waiting").length >= 4), 15_000);
  const waits = await page.evaluate(() => window.spike.log.statuses.filter((s) => s.s === "waiting").map((s) => s.d));
  const missed = addEvents(2);
  await page.unroute("**/client/events**");
  await until(async () => (await frames()).length >= 2, 15_000);
  assert(JSON.stringify(await frames()) === JSON.stringify(missed), "did not catch up exactly");
  await stop();
  await page.evaluate(() => { window.spike.timing.backoffBase = 1; window.spike.timing.backoffCap = 60; });
  return `${aborts} refused attempts, naps ${waits.slice(0, 5).join(" → ")}`;
});

await check("3b. dead port (no route mocking) backs off without throwing", async () => {
  await clearLog();
  await page.evaluate(() => { window.spike.timing.backoffBase = 0.1; window.spike.timing.backoffCap = 0.4; });
  await start({ serverUrl: "http://127.0.0.1:9" });
  await until(() => page.evaluate(() => window.spike.log.statuses.filter((s) => s.s === "waiting").length >= 3), 15_000);
  await stop();
  await page.evaluate(() => { window.spike.timing.backoffBase = 1; window.spike.timing.backoffCap = 60; });
});

// 4 ─ background tab and frozen page.
// Headless Chromium here keeps every page `visible`, and CDP
// Page.setWebLifecycleState does not freeze a visible page (timers keep
// running), so real hide/freeze is a MANUAL check (FINDINGS.md). What these
// prove is the client's half: how it reacts to the lifecycle signals and to a
// stream that goes silent.
const fire = (what) => page.evaluate((w) => {
  if (w === "hidden" || w === "visible") {
    Object.defineProperty(document, "visibilityState", { configurable: true, get: () => w });
    document.dispatchEvent(new Event("visibilitychange"));
  } else {
    document.dispatchEvent(new Event(w)); // freeze / resume
  }
}, what);

await check("4a. tab hidden then shown: an open stream is left alone (no reconnect)", async () => {
  await clearLog();
  await page.evaluate((v) => window.spike.setCursor("spike", v), latestId());
  await start();
  await until(() => page.evaluate(() => window.spike.log.statuses.some((s) => s.s === "open")));
  const before = await page.evaluate(() => window.spike.connections());
  await fire("hidden");
  await sleep(500);
  await fire("visible");
  await sleep(500);
  const after = await page.evaluate(() => window.spike.connections());
  assert(after === before, `reconnected on visibility (${before}→${after})`);
  await stop();
});

await check("4b. resume after freeze: reconnects at once with after= and catches up exactly", async () => {
  await clearLog();
  await page.evaluate((v) => window.spike.setCursor("spike", v), latestId());
  await start();
  await until(() => page.evaluate(() => window.spike.log.statuses.some((s) => s.s === "open")));
  const before = await page.evaluate(() => window.spike.connections());
  await fire("freeze");
  // Out-of-process appends never wake a live stream (the daemon wakes on an
  // in-process notifier), so these arrive ONLY through the reconnect.
  const missed = addEvents(2);
  await fire("resume");
  await until(async () => (await frames()).length >= 2, 10_000);
  const after = await page.evaluate(() => window.spike.connections());
  assert(JSON.stringify(await frames()) === JSON.stringify(missed), `got ${await frames()} want ${missed}`);
  assert(after === before + 1, `expected exactly one reconnect, ${before}→${after}`);
  const waited = await page.evaluate(() => window.spike.log.statuses.some((s) => s.s === "waiting"));
  assert(!waited, "resume napped through a backoff instead of reconnecting at once");
  await stop();
  return `one reconnect, no nap, got ${missed.join(",")}`;
});

await check("4c. a stream that goes silent is caught by the inactivity watchdog", async () => {
  await clearLog();
  // The daemon pings every 15s; a 2s watchdog makes every connection "silent".
  await page.evaluate(() => { window.spike.timing.inactivityTimeout = 2; window.spike.timing.backoffBase = 0.1; });
  await page.evaluate((v) => window.spike.setCursor("spike", v), latestId());
  await start();
  await until(() => page.evaluate(() => window.spike.connections() >= 3), 15_000);
  const missed = addEvents(1);
  await until(async () => (await frames()).length >= 1, 10_000);
  assert(JSON.stringify(await frames()) === JSON.stringify(missed), "watchdog reconnect lost or replayed");
  await stop();
  await page.evaluate(() => { window.spike.timing.inactivityTimeout = 60; window.spike.timing.backoffBase = 1; });
});

// 5 ─ keep-alive cadence as the browser sees it.
await check("5. keep-alive: an idle stream delivers bytes every ~15s", async () => {
  if (!process.env.LONG) return "skipped (LONG=1 runs it)";
  const ticks = await page.evaluate(async ({ daemon, token }) => {
    const t0 = performance.now();
    const ctl = new AbortController();
    const res = await fetch(daemon + "/client/events", { headers: { Authorization: "Bearer " + token }, signal: ctl.signal });
    const rd = res.body.getReader();
    const out = [];
    setTimeout(() => ctl.abort(), 33_000);
    try { for (;;) { const { done } = await rd.read(); if (done) break; out.push(Math.round((performance.now() - t0) / 1000)); } } catch {}
    return out;
  }, { daemon: DAEMON, token: TOKEN });
  assert(ticks.length >= 2, `only ${ticks.length} chunks in 33s`);
  return `chunks at ${ticks.join("s, ")}s`;
});

await browser.close();
srv.close();
console.log(results.join("\n"));
console.log(`\n${results.length - failures}/${results.length} passed`);
process.exit(failures ? 1 : 0);
