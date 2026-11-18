// The web client end to end, in a production-shaped build (shipping CSP and
// Trusted Types), against a real daemon serving the invented `--rich` inbox:
//
//   spike_seed <db> --rich
//   SPIKE_SEED=<target/debug/examples/spike_seed> SPIKE_DB=<db> npm run e2e
//
// with `squelch-api` serving that db on 127.0.0.1:8849 and
// SQUELCH_PAIR_ORIGINS=http://localhost:5191. A local squelchd already holds
// 8849 on a dev machine: serve elsewhere (SQUELCH_API_HTTP=127.0.0.1:8852) and
// set E2E_DAEMON=http://127.0.0.1:8852. Control is stubbed: the test answers
// its sign-in URL with the redirect control would send.
import { execFileSync } from "node:child_process";
import { expect, test, type Page } from "@playwright/test";

const DAEMON = process.env.E2E_DAEMON ?? "http://127.0.0.1:8849";
const CONTROL = "http://127.0.0.1:5177/app/auth";

function mintCode(): string {
  const out = execFileSync(process.env.SPIKE_SEED!, [process.env.SPIKE_DB!, "--pair-code"]);
  return out.toString().trim().split("\n").pop()!;
}

/** Fail the test on any CSP or Trusted Types violation in the app. */
async function watchViolations(page: Page): Promise<string[]> {
  const seen: string[] = [];
  page.on("console", (m) => {
    const t = m.text();
    if (/Content Security Policy|TrustedHTML|Trusted Type/i.test(t)) seen.push(t);
  });
  await page.addInitScript(() => {
    document.addEventListener("securitypolicyviolation", (e) =>
      console.error(`Content Security Policy violation: ${e.violatedDirective} ${e.blockedURI}`),
    );
  });
  return seen;
}

async function signIn(page: Page) {
  const code = mintCode();
  await page.route(`${CONTROL}*`, (route) => {
    const state = new URL(route.request().url()).searchParams.get("state") ?? "";
    const frag = new URLSearchParams({ url: DAEMON, code, state });
    const to = `http://localhost:5191/connect#${frag}`.replace(/&/g, "&amp;");
    // Control answers with a 302. WebKit cannot fulfill a redirect status, so
    // the stub lands the browser on the same URL with a refresh instead.
    return route.fulfill({
      status: 200,
      contentType: "text/html",
      body: `<meta http-equiv="refresh" content="0;url=${to}">`,
    });
  });
  await page.goto("/");
  await page.getByRole("button", { name: "Continue with Google" }).click();
  await expect(page.getByRole("region", { name: "For your eyes" })).toBeVisible();
  return code;
}

test("sign in with nothing typed: the sitrep, a thread, done and undo, sign out", async ({ page }) => {
  const violations = await watchViolations(page);
  const code = await signIn(page);
  expect(page.url()).toBe("http://localhost:5191/");

  // The code was claimed once and is spent.
  const again = await page.request.post(`${DAEMON}/client/pair`, {
    data: { code, device_name: "probe" },
  });
  expect(again.status()).toBe(401);

  // The sitrep: a greeting with the account's name, and every zone drawn.
  await expect(page.getByText(/^Good (morning|afternoon|evening), Sam$/)).toBeVisible();
  const eyes = page.getByRole("region", { name: "For your eyes" });
  await expect(eyes.locator("li.eye")).toHaveCount(5);
  await expect(eyes.getByText(/past due/i)).toBeVisible();
  await expect(page.getByRole("region", { name: "Reading" }).locator(".tile")).toHaveCount(6);
  await expect(page.getByRole("region", { name: "Shipments" }).getByText("out for delivery")).toBeVisible();
  await expect(page.getByRole("region", { name: "Receipts" }).getByText("$6.50")).toBeVisible();
  for (const zone of ["Calendar", "Billing"]) {
    await expect(page.getByRole("region", { name: zone })).toBeVisible();
  }

  // The way to the app is always there, and the first-visit banner makes the case.
  await expect(page.getByRole("link", { name: /Get (Passband for|the Passband app)/ }).first()).toBeVisible();
  await expect(page.getByRole("complementary", { name: "Passband app" })).toBeVisible();

  // Open a thread from For your eyes.
  await eyes.getByText("Harborview Leasing").click();
  await expect(page.getByRole("heading", { name: "Lease renewal: signature needed" })).toBeVisible();
  expect(new URL(page.url()).pathname).toBe("/t/rich-t0");
  await page.keyboard.press("Escape");
  await expect(eyes).toBeVisible();

  // Done leaves the board; undo brings it back.
  await eyes.locator("li.eye", { hasText: "Harborview Leasing" }).hover();
  await page.keyboard.press("e");
  await expect(eyes.getByText("Harborview Leasing")).toHaveCount(0);
  // The undo is offered once the daemon has confirmed the done.
  await expect(page.getByText("done · Harborview Leasing")).toBeVisible();
  await page.keyboard.press("u");
  await expect(eyes.getByText("Harborview Leasing")).toHaveCount(1);

  // The full list is one click away.
  await page.getByRole("button", { name: "Mail", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Mail" })).toBeVisible();
  await expect(page.getByText("Your keyboard is out for delivery today.")).toBeVisible();

  // Sign out goes back to sign-in and keeps nobody signed in on reload.
  await page.getByRole("button", { name: "Sign out" }).click();
  await expect(page.getByRole("button", { name: "Continue with Google" })).toBeVisible();
  await page.reload();
  await expect(page.getByRole("button", { name: "Continue with Google" })).toBeVisible();

  expect(violations).toEqual([]);
});

test("a reply undone inside the hold is never sent", async ({ page }) => {
  const violations = await watchViolations(page);
  await signIn(page);
  const sends: string[] = [];
  page.on("request", (r) => {
    if (r.url().includes("/client/actions/send")) sends.push(r.method());
  });
  await page.getByRole("region", { name: "For your eyes" }).getByText("Mom").click();
  await expect(page.getByRole("heading", { name: "Photos from the weekend" })).toBeVisible();
  await page.keyboard.press("r");
  await page.getByLabel(/Reply to/).fill("Lovely, thank you!");
  await page.keyboard.press("Control+Enter");
  await expect(page.getByText("sending…")).toBeVisible();
  await page.getByRole("button", { name: /undo/ }).click();
  await expect(page.getByLabel(/Reply to/)).toHaveValue("Lovely, thank you!");
  await page.waitForTimeout(5_500);
  expect(sends).toEqual([]);
  expect(violations).toEqual([]);
});

test("a forged sign-in link is refused and nothing is sent", async ({ page }) => {
  const requests: string[] = [];
  page.on("request", (r) => requests.push(r.url()));
  const code = mintCode();
  await page.goto(`/connect#url=${encodeURIComponent(DAEMON)}&code=${code}&state=forged`);
  await expect(page.getByRole("status")).toHaveText(/wasn't started here/);
  expect(requests.filter((u) => u.startsWith(DAEMON))).toEqual([]);
  expect(page.url()).toBe("http://localhost:5191/connect");
});

test("flow mode: Space walks For your eyes, E and S decide, the board celebrates", async ({ page }) => {
  const violations = await watchViolations(page);
  await signIn(page);
  const eyes = page.getByRole("region", { name: "For your eyes" });
  const total = await eyes.locator("li.eye").count();
  const sealed = await eyes.locator("li.eye .pill.lock").count();
  const walkable = total - sealed;
  expect(walkable).toBeGreaterThan(2);

  await page.keyboard.press(" ");
  const bar = page.getByLabel("Flow mode");
  await expect(bar).toContainText(`1 of ${walkable}`);
  const firstPath = new URL(page.url()).pathname;

  // E: done, and straight on to the next one.
  await page.keyboard.press("e");
  await expect(bar).toContainText(`2 of ${walkable}`);
  expect(new URL(page.url()).pathname).not.toBe(firstPath);

  // u: the done comes back, and the walk steps back onto it.
  await expect(page.getByText(/^done · /)).toBeVisible();
  await page.keyboard.press("u");
  await expect(bar).toContainText(`1 of ${walkable}`);
  expect(new URL(page.url()).pathname).toBe(firstPath);

  // Arrows move without deciding.
  await page.keyboard.press("ArrowRight");
  await expect(bar).toContainText(`2 of ${walkable}`);
  await page.keyboard.press("ArrowLeft");
  await expect(bar).toContainText(`1 of ${walkable}`);

  // S on every email: nothing is changed, and the last one ends the walk.
  for (let i = 0; i < walkable; i++) await page.keyboard.press("s");
  const done = page.getByRole("complementary", { name: "Flow complete" });
  await expect(done).toContainText(`${walkable} saved for later`);
  expect(new URL(page.url()).pathname).toBe("/");
  await expect(eyes.locator("li.eye")).toHaveCount(total);

  // Back from a walk leaves flow mode rather than stepping through it.
  await page.keyboard.press(" ");
  await expect(bar).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(eyes).toBeVisible();
  expect(violations).toEqual([]);
});
