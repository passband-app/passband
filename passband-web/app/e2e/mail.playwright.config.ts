import { defineConfig } from "@playwright/test";

// The reading-frame suite on its own port and build directory, so it can run
// beside the app suite (playwright.config.ts) without sharing a server:
//
//   npx playwright test --trace off -c e2e/mail.playwright.config.ts
//
// Same production-shaped build: `--mode e2e` (shipping CSP + Trusted Types).
const PORT = 5192;
const OUT = "node_modules/.mail-e2e";

export default defineConfig({
  testDir: ".",
  testMatch: "mail.spec.ts",
  timeout: 30_000,
  fullyParallel: true,
  reporter: [["list"]],
  use: { baseURL: `http://localhost:${PORT}`, trace: "off" },
  webServer: {
    command: `npx vite build --mode e2e --outDir ${OUT} --emptyOutDir && npx vite preview --mode e2e --outDir ${OUT} --port ${PORT}`,
    cwd: "..",
    url: `http://localhost:${PORT}/e2e/mail-harness.html`,
    reuseExistingServer: false,
    timeout: 120_000,
  },
});
