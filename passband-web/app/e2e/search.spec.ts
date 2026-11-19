// Search against a real daemon serving the invented `--rich` inbox. See
// search.playwright.config.ts for how to run it.
import { expect, test, type Page } from "@playwright/test";

const DAEMON = process.env.SEARCH_DAEMON ?? "http://127.0.0.1:8855";
const TOKEN = process.env.SEARCH_TOKEN ?? "spike-token";

async function open(page: Page, q?: string): Promise<string[]> {
  const violations: string[] = [];
  page.on("console", (m) => {
    if (/Content Security Policy|Trusted Type/i.test(m.text())) violations.push(m.text());
  });
  const params = new URLSearchParams({ daemon: DAEMON, token: TOKEN });
  if (q) params.set("q", q);
  await page.goto(`/e2e/search-harness.html?${params}`);
  await expect(page.getByLabel("Search your mail")).toBeFocused();
  return violations;
}

const results = (page: Page) => page.getByRole("region", { name: "Search results" });

test("results arrive as you type, with the matched words lit as elements", async ({ page }) => {
  const violations = await open(page);
  await page.keyboard.type("offs");
  const hit = results(page).locator("li.hit", { hasText: "Offsite moved to the 14th" });
  await expect(hit).toBeVisible();
  // Partial matching: "offs" found "Offsite". The light is a <mark>, never markup from the mail.
  await expect(hit.locator("mark.hl").first()).toHaveText(/^offsite$/i);
  expect(await results(page).locator(".line *:not(span):not(mark)").count()).toBe(0);
  expect(violations).toEqual([]);
});

test("arrow and Enter open a hit, and the search is remembered", async ({ page }) => {
  await open(page);
  await page.keyboard.type("lease renewal");
  await expect(results(page).locator("li.hit").first()).toBeVisible();
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("Enter");
  await expect(page.locator("#opened")).toHaveText(/^rich-t\d+$/);
  await page.reload();
  const recent = page.getByRole("region", { name: "Recent searches" });
  await expect(recent.getByRole("button", { name: "lease renewal" })).toBeVisible();
  await recent.getByRole("button", { name: "lease renewal" }).click();
  await expect(page.getByLabel("Search your mail")).toHaveValue("lease renewal");
});

test("a sender is offered above the hits, and picking one lists their mail", async ({ page }) => {
  await open(page);
  await page.keyboard.type("bright");
  const sender = results(page).locator("li.sender-row", { hasText: "Brightly" });
  await expect(sender).toBeVisible();
  await sender.click();
  await expect(page.getByLabel("Search your mail")).toHaveValue("from:deals@brightly.example ");
  const hits = results(page).locator("li.hit");
  await expect(hits.first()).toBeVisible();
  const whos = await hits.locator(".who").allTextContents();
  expect(whos.every((w) => w === "Brightly")).toBe(true);
});

test("from: with no value waits; typing into it offers the menu", async ({ page }) => {
  await open(page);
  const asked: string[] = [];
  page.on("request", (r) => {
    if (r.url().includes("/client/search")) asked.push(new URL(r.url()).searchParams.get("q") ?? "");
  });
  await page.keyboard.type("from:");
  await page.waitForTimeout(600);
  expect(asked).toEqual([]);
  await page.keyboard.type("jam");
  await expect(results(page).locator("li.sender-row", { hasText: "Jamie" })).toBeVisible();
  await expect(results(page).locator(".list-section").first()).toHaveText("From");
});

test("no match says so; Esc clears, then blurs", async ({ page }) => {
  await open(page);
  await page.keyboard.type("zzqqxx");
  await expect(page.getByText("No mail matches “zzqqxx”.")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByLabel("Search your mail")).toHaveValue("");
  await page.keyboard.press("Escape");
  await expect(page.getByLabel("Search your mail")).not.toBeFocused();
});
