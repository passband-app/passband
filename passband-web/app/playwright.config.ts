import { defineConfig } from "@playwright/test";

// e2e runs against a PRODUCTION-SHAPED build (`--mode e2e`: the shipping CSP
// and Trusted Types, plus loopback daemons allowed) served by `vite preview`.
export default defineConfig({
  testDir: "./e2e",
  timeout: 30_000,
  fullyParallel: true,
  reporter: [["list"]],
  use: { baseURL: "http://localhost:5191", trace: "off" },
  webServer: {
    command: "npx vite build --mode e2e && npx vite preview --mode e2e",
    url: "http://localhost:5191/",
    reuseExistingServer: false,
    timeout: 120_000,
  },
});
