import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: './tests',
  timeout: 30_000,
  fullyParallel: true,
  reporter: [['list']],
  use: { baseURL: 'http://localhost:5182', trace: 'off', viewport: { width: 900, height: 900 } },
  webServer: {
    command: 'npx vite build && npx vite preview --port 5182 --strictPort',
    url: 'http://localhost:5182/index.html',
    reuseExistingServer: false,
    timeout: 60_000,
  },
});
