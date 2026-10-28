// Headless UI tests for the Per-app tone screen, run against an isolated
// harness with Tauri IPC mocked. Kept apart from playwright.config.ts (which
// targets the full app on port 1420): `bunx playwright test -c playwright.flow.config.ts`
import { defineConfig, devices } from "@playwright/test";

const port = 1520;
const baseURL = `http://localhost:${port}`;

export default defineConfig({
  testDir: "./tests/flow",
  testMatch: /.*\.flow\.ts$/,
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 2 : 0,
  reporter: "list",
  outputDir: "test-results/flow",
  use: {
    baseURL,
    headless: true,
    trace: "retain-on-failure",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
  webServer: {
    command: `bunx vite --config tests/flow/vite.config.ts --port ${port} --strictPort`,
    url: `${baseURL}/index.html`,
    reuseExistingServer: false,
    timeout: 60000,
  },
});
