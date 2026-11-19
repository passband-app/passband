import { defineConfig } from "@playwright/test";

// The search suite: the real SearchPage in an e2e-only harness, under the
// shipping CSP and Trusted Types, against a daemon serving the --rich inbox:
//
//   SEARCH_DAEMON=http://127.0.0.1:8855 npx playwright test -c e2e/search.playwright.config.ts --trace off
export default defineConfig({
  testDir: ".",
  testMatch: /search\.spec\.ts/,
  timeout: 30_000,
  reporter: [["list"]],
  outputDir: "../node_modules/.search-e2e-results",
  use: { baseURL: "http://localhost:5196", trace: "off" },
  webServer: {
    command: "npx vite build --mode e2e --outDir node_modules/.search-e2e-dist && npx vite preview --mode e2e --outDir node_modules/.search-e2e-dist --port 5196 --strictPort",
    url: "http://localhost:5196/e2e/search-harness.html",
    reuseExistingServer: false,
    timeout: 120_000,
    cwd: "..",
  },
});
