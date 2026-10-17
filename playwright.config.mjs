// Browser UI tests against the Vite dev server in mock mode (no Tauri). Run: bun run test:ui
// Playwright starts the server on its own port and stops it after the run.
import { defineConfig } from "@playwright/test";

const PORT = 1499;
export default defineConfig({
  testDir: "tests",
  testMatch: /.*\.spec\.mjs/,
  timeout: 30_000,
  expect: { timeout: 5_000 },
  fullyParallel: true,
  workers: 4,
  reporter: [["list"]],
  use: {
    baseURL: `http://127.0.0.1:${PORT}`,
    headless: true,
    viewport: { width: 1100, height: 760 },
    permissions: ["clipboard-read", "clipboard-write"],
    acceptDownloads: true,
    trace: "retain-on-failure",
  },
  webServer: {
    command: `bunx vite --port ${PORT} --strictPort --host 127.0.0.1`,
    url: `http://127.0.0.1:${PORT}`,
    reuseExistingServer: false,
    timeout: 30_000,
  },
});
