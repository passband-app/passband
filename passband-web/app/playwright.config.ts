import { defineConfig } from "@playwright/test";

// e2e runs against a PRODUCTION-SHAPED build (`--mode e2e`: the shipping CSP
// and Trusted Types, plus loopback daemons allowed) served by `vite preview`.
export default defineConfig({
  testDir: "./e2e",
  // The mail suite needs no daemon and runs under its own config.
  testIgnore: /mail\.spec\.ts/,
  timeout: 30_000,
  // One test at a time: each one mints a pairing code, and minting supersedes
  // the account's previous code, so two tests signing in at once spend each
  // other's.
  fullyParallel: false,
  workers: 1,
  reporter: [["list"]],
  use: { baseURL: "http://localhost:5191", trace: "off" },
  webServer: {
    command: "npx vite build --mode e2e && npx vite preview --mode e2e",
    url: "http://localhost:5191/",
    reuseExistingServer: false,
    timeout: 120_000,
  },
});
