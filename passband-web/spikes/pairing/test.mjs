// Spike 4 end-to-end: run with the spike daemon on :8851 (SQUELCH_PAIR_ORIGINS
// listing http://localhost:5176) and `node serve.mjs` up.
//   SPIKE_SEED=<path to spike_seed binary> SPIKE_DB=<db> node test.mjs
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const { chromium } = require("playwright");

const DAEMON = "http://127.0.0.1:8851";
const mint = () =>
  execFileSync(process.env.SPIKE_SEED, [process.env.SPIKE_DB, "--pair-code"], {
    env: { ...process.env },
  })
    .toString()
    .trim()
    .split("\n")
    .pop();

const browser = await chromium.launch();
let passed = 0;

async function withPage(fn) {
  const ctx = await browser.newContext();
  const page = await ctx.newPage();
  const requests = [];
  page.on("request", (r) => requests.push({ url: r.url(), method: r.method() }));
  try {
    await fn(page, requests);
  } finally {
    await ctx.close();
  }
}

async function claimStatus(code) {
  const r = await fetch(`${DAEMON}/client/pair`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ code, device_name: "probe" }),
  });
  return r.status;
}

async function test(name, fn) {
  await fn();
  passed++;
  console.log(`ok  ${name}`);
}

await test("sign-in round trip pairs with no typing and leaves no secret in the URL", () =>
  withPage(async (page, requests) => {
    const code = mint();
    await page.route("http://127.0.0.1:5177/app/auth*", (route) => {
      const u = new URL(route.request().url());
      u.searchParams.set("test_code", code);
      route.continue({ url: u.toString() });
    });
    await page.goto("http://localhost:5176/");
    await page.click("#signin");
    await page.waitForFunction(() => document.getElementById("out").textContent !== "");
    assert.match(await page.textContent("#out"), /^paired as .*; stats 200$/);
    assert.equal(page.url(), "http://localhost:5176/connect", "fragment erased");
    const pairReq = requests.find((r) => r.url === `${DAEMON}/client/pair`);
    assert.ok(pairReq, "claimed against the daemon");
    assert.ok(
      requests.every((r) => !r.url.includes(code)),
      "the code never appears in any request URL",
    );
    assert.equal(await claimStatus(code), 401, "code is one-shot");
  }));

await test("a forged link the app did not start is refused before any request", () =>
  withPage(async (page, requests) => {
    const code = mint();
    await page.goto(
      `http://localhost:5176/connect#url=${encodeURIComponent(DAEMON)}&code=${code}&state=attacker`,
    );
    await page.waitForFunction(() => document.getElementById("out").textContent !== "");
    assert.equal(await page.textContent("#out"), "refused: state");
    assert.ok(!requests.some((r) => r.url.startsWith(DAEMON)), "nothing sent to the daemon");
    assert.equal(page.url(), "http://localhost:5176/connect");
    assert.equal(await claimStatus(code), 200, "code untouched by the refusal");
  }));

await test("a pending sign-in cannot be completed with a server the app does not allow", () =>
  withPage(async (page, requests) => {
    await page.route("http://127.0.0.1:5177/app/auth*", (route) => {
      const state = new URL(route.request().url()).searchParams.get("state");
      route.fulfill({
        status: 302,
        headers: {
          location: `http://localhost:5176/connect#url=${encodeURIComponent("https://evil.example")}&code=ABCD-EFGH&state=${state}`,
        },
      });
    });
    await page.goto("http://localhost:5176/");
    await page.click("#signin");
    await page.waitForFunction(() => document.getElementById("out").textContent !== "");
    assert.equal(await page.textContent("#out"), "refused: server");
    assert.ok(!requests.some((r) => r.url.includes("evil.example")));
  }));

await test("a page on another origin cannot read a token, and cannot burn the code", () =>
  withPage(async (page, requests) => {
    const code = mint();
    await page.goto("http://127.0.0.1:5178/");
    const result = await page.evaluate(
      async ({ DAEMON, code }) => {
        const out = {};
        try {
          await fetch(`${DAEMON}/client/pair`, {
            method: "POST",
            headers: { "content-type": "application/json" },
            body: JSON.stringify({ code, device_name: "evil" }),
          });
          out.json = "read";
        } catch (e) {
          out.json = "blocked";
        }
        // The only request a page can send without a preflight: no-cors text/plain.
        await fetch(`${DAEMON}/client/pair`, {
          method: "POST",
          mode: "no-cors",
          headers: { "content-type": "text/plain" },
          body: JSON.stringify({ code, device_name: "evil" }),
        });
        out.nocors = "sent";
        return out;
      },
      { DAEMON, code },
    );
    assert.equal(result.json, "blocked", "JSON claim blocked at preflight");
    // Playwright reports a request event for the JSON POST even though Chromium
    // stops it at the failed preflight, so the client-side count proves
    // nothing. The server is the witness: had either POST been processed, the
    // valid code would now be spent.
    console.log(
      "    requests seen:",
      requests.filter((r) => r.url.startsWith(DAEMON)).map((r) => r.method).join(", "),
    );
    assert.equal(await claimStatus(code), 200, "neither attempt consumed the code");
  }));

await browser.close();
console.log(`${passed} passed`);
