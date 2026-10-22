import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./tests",
  timeout: 45_000,
  fullyParallel: false,
  workers: 1,
  globalSetup: "./tests/e2e/global-setup.ts",
  use: {
    baseURL: "http://127.0.0.1:4173",
    headless: true,
    viewport: { width: 1280, height: 720 },
  },
  projects: [
    {
      name: "headless",
      testIgnore: /(pointerLock\.spec\.ts|e2e\/)/,
    },
    {
      // Chromium's headless shell does not grant pointer lock. CI runs this
      // project under Xvfb; a local run opens a short-lived window.
      name: "headed",
      testMatch: /pointerLock\.spec\.ts$/,
      use: { headless: false },
    },
    {
      name: "chromium-e2e",
      testMatch: /e2e\/.*\.spec\.ts$/,
      use: { browserName: "chromium" },
    },
    {
      name: "firefox-e2e",
      testMatch: /e2e\/.*\.spec\.ts$/,
      use: { browserName: "firefox" },
    },
    {
      name: "webkit-e2e",
      testMatch: /e2e\/.*\.spec\.ts$/,
      use: { browserName: "webkit" },
    },
    {
      name: "mobile-e2e",
      testMatch: /e2e\/.*\.spec\.ts$/,
      use: {
        browserName: "chromium",
        viewport: { width: 430, height: 900 },
        hasTouch: true,
        isMobile: true,
      },
    },
  ],
  webServer: {
    command: "npm run build && npm run preview -- --host 127.0.0.1 --port 4173 --strictPort",
    url: "http://127.0.0.1:4173",
    reuseExistingServer: true,
    timeout: 180_000,
  },
});
