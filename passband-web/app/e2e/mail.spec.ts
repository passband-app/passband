// The reading frame, under the app's shipping CSP and Trusted Types
// (`vite build --mode e2e`), mounted by e2e/mail-harness.html. Run with
//   npx playwright test --trace off -c e2e/mail.playwright.config.ts
// Ported from passband-web/spikes/email-render/tests. No daemon needed.
import { expect, test, type Page, type Request } from "@playwright/test";
import { HOSTILE } from "./mail-corpus";

/** The app origin of whichever config runs this suite (mail or app). */
const app = () => new URL(test.info().project.use.baseURL ?? "http://localhost:5192").origin;
const harness = (mail: string, extra: Record<string, string> = {}) =>
  `${app()}/e2e/mail-harness.html?${new URLSearchParams({ mail, ...extra })}`;

interface Recorder {
  /** Every request that reached the network layer. */
  requests: string[];
  popups: number;
  /** CSP / Trusted Types violations in OUR page (not inside the mail frame). */
  violations: string[];
  /** Requests to any origin but the app's own. */
  offending(): string[];
}

async function record(page: Page): Promise<Recorder> {
  const rec: Recorder = {
    requests: [],
    popups: 0,
    violations: [],
    offending() {
      return rec.requests.filter((u) => {
        if (/^(data|blob|about):/.test(u)) return false;
        return new URL(u).origin !== app();
      });
    },
  };
  await page.context().route("**/*", (route) => {
    const url = route.request().url();
    rec.requests.push(url);
    return new URL(url).origin === app() ? route.continue() : route.abort();
  });
  page.on("request", (r: Request) => {
    if (!rec.requests.includes(r.url()) && !/^(data|blob):/.test(r.url())) {
      // A request that never reached the route was refused by the renderer
      // (CSP inside the mail frame). Recorded only so a test can see it.
    }
  });
  page.on("popup", () => {
    rec.popups++;
  });
  page.on("console", (m) => {
    const t = m.text();
    if (/^PB-VIOLATION|TrustedHTML|Trusted Type/.test(t)) rec.violations.push(t);
  });
  await page.addInitScript(() => {
    if (window !== window.top) return;
    document.addEventListener("securitypolicyviolation", (e) =>
      console.error(`PB-VIOLATION ${e.violatedDirective} ${e.blockedURI}`),
    );
  });
  return rec;
}

async function waitForFrame(page: Page, index = 0) {
  await page.waitForFunction((i) => {
    const f = document.querySelectorAll("iframe")[i];
    return !!f && Number(f.getAttribute("data-height")) > 0 && f.getAttribute("data-interactive") === "1";
  }, index);
}

async function frameGeometry(page: Page, index = 0) {
  return page.evaluate((i) => {
    const f = document.querySelectorAll("iframe")[i] as HTMLIFrameElement;
    const d = f.contentDocument!;
    const body = d.body.getBoundingClientRect();
    const all = Array.from(d.body.querySelectorAll("*")).filter((e) => e.getClientRects().length > 0);
    const bottom = Math.max(0, ...all.map((e) => e.getBoundingClientRect().bottom));
    return { frame: f.clientHeight, body: body.height, contentBottom: bottom };
  }, index);
}

// ---------------------------------------------------------------- hostile mail

for (const images of [false, true]) {
  test.describe(images ? "hostile, images on" : "hostile, images off", () => {
    for (const mail of HOSTILE) {
      test(mail.id, async ({ page }) => {
        const rec = await record(page);
        const url = harness(mail.id, images ? { images: "1" } : {});
        await page.goto(url);
        await waitForFrame(page);

        const frame = page.frameLocator("iframe").first();
        if (mail.hover) await frame.locator(mail.hover).hover({ timeout: 2000 }).catch(() => {});
        if (mail.click) await frame.locator(mail.click).click({ timeout: 2000 }).catch(() => {});
        await page.waitForTimeout(1200);

        // 1. No script from the mail ran anywhere.
        expect(await page.evaluate(() => window.__pwned)).toBeUndefined();
        expect(await page.title()).toBe("mail harness");

        // 2. Nothing left for any origin but the app's own: remote images go
        //    through the ImageSource (the daemon's proxy), never the network.
        expect(rec.offending()).toEqual([]);

        // 3. Neither the page nor the mail frame went anywhere; no window opened.
        expect(page.url()).toBe(url);
        expect(rec.popups).toBe(0);
        expect(page.frames()).toHaveLength(2);
        expect(page.frames()[1]!.url()).toBe("about:srcdoc");

        // 4. The secret never appears in any request or reaches the image source.
        expect(rec.requests.some((u) => u.includes("secret-device-token"))).toBe(false);
        expect(
          (await page.evaluate(() => window.__mail.remote)).some((u) => u.includes("secret-device-token")),
        ).toBe(false);

        // 5. Our own page broke none of its own rules. One refusal is expected
        //    and is the app CSP working: the pre-pass parses the mail into an
        //    inert document in OUR page, and a mail's <base href> is refused
        //    there by `base-uri 'none'` before the pass drops the element.
        expect(rec.violations.filter((v) => !/^PB-VIOLATION base-uri /.test(v))).toEqual([]);
      });
    }
  });
}

// ---------------------------------------------------------------- the frame

test("the sandbox is exactly allow-same-origin, with the frame's own policy", async ({ page }) => {
  await page.goto(harness("long"));
  await waitForFrame(page);
  const f = page.locator("iframe");
  expect(await f.getAttribute("sandbox")).toBe("allow-same-origin");
  expect(await f.getAttribute("srcdoc")).not.toBeNull();
  expect(await f.getAttribute("data-csp")).toBe(
    "default-src 'none'; style-src 'unsafe-inline'; img-src data: blob:; base-uri 'none'; form-action 'none'",
  );
  // The policy is the first element of the frame's <head>.
  const first = await page.evaluate(
    () => (document.querySelector("iframe") as HTMLIFrameElement).contentDocument!.head.firstElementChild!.outerHTML,
  );
  expect(first).toContain('http-equiv="Content-Security-Policy"');
});

test("a long mail is sized to its content", async ({ page }) => {
  const rec = await record(page);
  await page.goto(harness("long"));
  await waitForFrame(page);
  await page.waitForTimeout(200);
  const g = await frameGeometry(page);
  expect(g.frame).toBeGreaterThan(1500);
  expect(Math.abs(g.frame - g.body)).toBeLessThanOrEqual(2);
  expect(g.contentBottom).toBeLessThanOrEqual(g.frame + 1);
  expect(rec.violations).toEqual([]);
});

test("the frame grows when a late image lands", async ({ page }) => {
  await page.goto(harness("late-image", { images: "1" }));
  await waitForFrame(page);
  const before = (await frameGeometry(page)).frame;
  await page.waitForFunction(
    (b) => Number(document.querySelector("iframe")!.getAttribute("data-height")) > b + 300,
    before,
    { timeout: 5000 },
  );
  const g = await frameGeometry(page);
  expect(g.frame - before).toBeGreaterThanOrEqual(300);
  expect(Math.abs(g.frame - g.body)).toBeLessThanOrEqual(2);
});

for (const mail of ["gmail-reply", "wrote-reply"]) {
  test(`quoted history collapses: ${mail}`, async ({ page }) => {
    await page.goto(harness(mail));
    await waitForFrame(page);
    const toggle = page.locator(".quoted-toggle");
    await expect(toggle).toHaveText("show quoted text");
    const frame = page.frameLocator("iframe");
    await expect(frame.locator("blockquote").first()).toBeHidden();
    await expect(frame.getByText(/wrote:/)).toBeHidden();
    const collapsed = (await frameGeometry(page)).frame;
    await toggle.click();
    await expect(frame.locator("blockquote").first()).toBeVisible();
    await page.waitForFunction(
      (c) => Number(document.querySelector("iframe")!.getAttribute("data-height")) > c,
      collapsed,
    );
    await toggle.click();
    await expect(frame.locator("blockquote").first()).toBeHidden();
  });
}

test("a bottom-posted reply keeps its quote", async ({ page }) => {
  await page.goto(harness("bottom-posted"));
  await waitForFrame(page);
  await expect(page.locator(".quoted-toggle")).toHaveCount(0);
  await expect(page.frameLocator("iframe").locator("blockquote")).toBeVisible();
});

test("http(s) links reach the app and nothing navigates", async ({ page }) => {
  const rec = await record(page);
  const url = harness("links");
  await page.goto(url);
  await waitForFrame(page);
  const frame = page.frameLocator("iframe");
  for (const id of ["https", "http", "mailto", "ftp", "rel", "hash", "inner"]) {
    await frame.locator(`#${id}`).click();
  }
  await frame.locator("#https").click({ button: "middle" });
  await frame.locator("#https").click({ modifiers: ["ControlOrMeta"] });
  await page.waitForTimeout(300);
  const links = await page.evaluate(() => window.__mail.links);
  expect(links.slice(0, 3)).toEqual([
    "https://links.example/a",
    "http://links.example/b",
    "https://links.example/nested",
  ]);
  expect(links.every((l) => /^https?:\/\//.test(l))).toBe(true);
  expect(page.url()).toBe(url);
  expect(page.frames()[1]!.url()).toBe("about:srcdoc");
  expect(rec.popups).toBe(0);
  expect(rec.offending()).toEqual([]);
});

test("mail CSS cannot style the parent page", async ({ page }) => {
  await page.goto(harness("style-parent"));
  await waitForFrame(page);
  const marker = page.locator("#parent-marker");
  await expect(marker).toBeVisible();
  expect(await marker.evaluate((e) => getComputedStyle(e).backgroundColor)).toBe("rgb(240, 240, 240)");
  expect(await page.evaluate(() => getComputedStyle(document.body).backgroundColor)).not.toBe("rgb(255, 0, 0)");
});

test("the frame cannot run anything, even what the parent writes into it", async ({ page }) => {
  await page.goto(harness("long"));
  await waitForFrame(page);
  const r = await page.evaluate(() => {
    const f = document.querySelector("iframe")!;
    const d = f.contentDocument!;
    let refused = false;
    try {
      // Trusted Types refuses the handler string outright (the frame inherits
      // the app's policy); were it written, the sandbox would still not run it.
      const p = d.createElement("p");
      p.setAttribute("onclick", 'top.__pwned="injected-handler"');
      d.body.appendChild(p);
      p.click();
    } catch {
      refused = true;
    }
    return { parentReads: !!d.body, refused, pwned: window.__pwned ?? null };
  });
  expect(r.parentReads).toBe(true);
  expect(r.refused).toBe(true);
  expect(r.pwned).toBeNull();
});

test("the frame takes no input until the click capture is attached", async ({ page }) => {
  await page.addInitScript(() => {
    const w = window as unknown as { __states: string[] };
    w.__states = [];
    new MutationObserver(() => {
      const f = document.querySelector("iframe");
      if (f) w.__states.push(`${f.getAttribute("data-interactive")}:${f.style.pointerEvents}:${f.style.visibility}`);
    }).observe(document, { subtree: true, attributes: true, childList: true });
  });
  await page.goto(harness("links"));
  await waitForFrame(page);
  const states = await page.evaluate(() => (window as unknown as { __states: string[] }).__states);
  expect(states[0]).toBe("0:none:hidden");
  expect(states.at(-1)).toBe("1:auto:visible");
  for (const s of states) expect(["0:none:hidden", "1:auto:visible"]).toContain(s);
});

// ---------------------------------------------------------------- images

test("trackers are stripped before anything could load them", async ({ page }) => {
  await page.goto(harness("trackers", { images: "1" }));
  await waitForFrame(page);
  expect(await page.locator("iframe").getAttribute("data-trackers")).toBe("3");
  await page.waitForTimeout(300);
  expect(await page.evaluate(() => window.__mail.remote)).toEqual([]);
});

test("remote images wait for opt-in, then come only through the image source", async ({ page }) => {
  const rec = await record(page);
  await page.goto(harness("remote-mixed", { optin: "1" }));
  await waitForFrame(page);
  await page.waitForTimeout(300);
  expect(await page.evaluate(() => window.__mail.hasRemote.at(-1))).toBe(true);
  expect(await page.evaluate(() => window.__mail.remote)).toEqual([]);
  const frame = page.frameLocator("iframe");
  expect(await frame.locator("#hero").getAttribute("src")).toBeNull();
  // No network URL survived into the document at all.
  const doc = await page.evaluate(
    () => (document.querySelector("iframe") as HTMLIFrameElement).contentDocument!.documentElement.outerHTML,
  );
  expect(doc).not.toContain("images.example");

  await page.locator("#load-images").click();
  await expect.poll(() => page.evaluate(() => window.__mail.remote.length)).toBe(15);
  const asked = await page.evaluate(() => window.__mail.remote);
  expect(asked).toContain("https://images.example/hero.png");
  expect(asked).toContain("https://images.example/bg.png");
  expect(asked).toContain("https://images.example/sheet.png");

  await expect.poll(() => frame.locator("#hero").getAttribute("src")).toMatch(/^blob:/);
  await expect
    .poll(() => frame.locator("#bg").evaluate((e) => getComputedStyle(e).backgroundImage))
    .toMatch(/blob:/);
  await expect
    .poll(() => frame.locator("#sheet").evaluate((e) => getComputedStyle(e).backgroundImage))
    .toMatch(/blob:/);
  // Placeholder 1 and placeholder 12 are different images.
  const one = await frame.locator("#twelve-a").getAttribute("src");
  const twelve = await frame.locator("#twelve").getAttribute("src");
  expect(one).toMatch(/^blob:/);
  expect(twelve).toMatch(/^blob:/);
  expect(one).not.toBe(twelve);

  expect(rec.offending()).toEqual([]);
  expect(rec.violations).toEqual([]);
});

test("inline (cid:) images load without opt-in; a fake image type is refused", async ({ page }) => {
  const rec = await record(page);
  await page.goto(harness("cid"));
  await waitForFrame(page);
  const frame = page.frameLocator("iframe");
  await expect.poll(() => frame.locator("#logo").getAttribute("src")).toMatch(/^blob:/);
  await expect.poll(() => frame.locator("#logo").evaluate((e) => (e as HTMLImageElement).naturalWidth)).toBe(40);
  await page.waitForTimeout(300);
  expect(await frame.locator("#svgcid").getAttribute("src")).toBeNull();
  expect(await frame.locator("#unknown").getAttribute("src")).toBeNull();
  expect((await page.evaluate(() => window.__mail.attachments)).sort()).toEqual([41, 42]);
  expect(await page.evaluate(() => window.__mail.remote)).toEqual([]);
  expect(await page.evaluate(() => window.__mail.hasRemote.at(-1))).toBe(false);
  expect(rec.offending()).toEqual([]);
});

// ---------------------------------------------------------------- text, threads

test("plain-text mail is shown as text, never parsed", async ({ page }) => {
  await page.goto(harness("long", { plain: "1" }));
  await expect(page.locator("pre.mail-text")).toContainText("<b>not bold</b> & plain text");
  await expect(page.locator("iframe")).toHaveCount(0);
  await expect(page.locator("pre.mail-text b")).toHaveCount(0);
});

test("a long thread mounts frames as they come near, with space reserved", async ({ page }) => {
  const rec = await record(page);
  await page.setViewportSize({ width: 900, height: 700 });
  await page.goto(harness("long", { thread: "40" }));
  await waitForFrame(page);
  const mounted = await page.locator("iframe").count();
  expect(mounted).toBeGreaterThan(0);
  expect(mounted).toBeLessThan(40);
  // Placeholders hold space for the rest.
  expect(await page.locator(".mail").count()).toBe(40);
  // Scroll THROUGH the thread, as a reader does: each frame grows from its
  // reserved height as it is measured, so jumping straight to the bottom would
  // skip frames in the middle without them ever coming near the viewport.
  for (let i = 0; i < 600 && (await page.locator("iframe").count()) < 40; i++) {
    await page.evaluate(() => window.scrollBy(0, 1500));
    await page.waitForTimeout(20);
  }
  await expect(page.locator("iframe")).toHaveCount(40);
  expect(rec.violations).toEqual([]);
});
