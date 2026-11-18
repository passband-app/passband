// The v1 path end to end, in a production-shaped build (shipping CSP and
// Trusted Types), against a real daemon:
//
//   SPIKE_SEED=<target/debug/examples/spike_seed> SPIKE_DB=<db> npm run e2e
//
// with `squelch-api` serving that db on 127.0.0.1:8849 and
// SQUELCH_PAIR_ORIGINS=http://localhost:5191. Control is stubbed: the test
// answers its sign-in URL with the redirect control would send.
import { execFileSync } from "node:child_process";
import { expect, test, type Page } from "@playwright/test";

const DAEMON = "http://127.0.0.1:8849";
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
    return route.fulfill({
      status: 302,
      headers: { location: `http://localhost:5191/connect#${frag}` },
    });
  });
  await page.goto("/");
  await page.getByRole("button", { name: "Continue with Google" }).click();
  await expect(page.getByRole("heading", { name: "Needs you" })).toBeVisible();
  return code;
}

test("sign in with nothing typed, read, done and undo, sign out", async ({ page }) => {
  const violations = await watchViolations(page);
  const code = await signIn(page);
  expect(page.url()).toBe("http://localhost:5191/");

  // The code was claimed once and is spent.
  const again = await page.request.post(`${DAEMON}/client/pair`, {
    data: { code, device_name: "probe" },
  });
  expect(again.status()).toBe(401);

  // The way to the app is always there, and the first-visit banner makes the case.
  await expect(page.getByRole("link", { name: /Get (Passband for|the Passband app)/ }).first()).toBeVisible();
  await expect(page.getByRole("complementary", { name: "Passband app" })).toBeVisible();

  // Open a thread.
  await page.getByRole("button", { name: /Your order shipped/ }).click();
  await expect(page.getByRole("heading", { name: "Your order shipped" })).toBeVisible();
  expect(new URL(page.url()).pathname).toBe("/t/t-shop");

  // Done leaves the inbox; undo brings it back.
  await page.keyboard.press("e");
  await expect(page.getByRole("heading", { name: "Needs you" })).toBeVisible();
  await expect(page.getByRole("button", { name: /Your order shipped/ })).toHaveCount(0);
  await page.keyboard.press("u");
  await expect(page.getByRole("button", { name: /Your order shipped/ })).toHaveCount(1);

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
  await page.getByRole("button", { name: /Lunch on Thursday/ }).click();
  await page.keyboard.press("r");
  await page.getByLabel(/Reply to/).fill("Yes, see you then.");
  await page.keyboard.press("Control+Enter");
  await expect(page.getByText("sending…")).toBeVisible();
  await page.getByRole("button", { name: /undo/ }).click();
  await expect(page.getByLabel(/Reply to/)).toHaveValue("Yes, see you then.");
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
