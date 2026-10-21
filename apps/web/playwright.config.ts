import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./tests",
  timeout: 30_000,
  fullyParallel: false,
  workers: 1,
  use: {
    baseURL: "http://127.0.0.1:4173",
    headless: true,
    viewport: { width: 1280, height: 720 },
  },
  projects: [
    {
      name: "headless",
      testIgnore: /pointerLock\.spec\.ts$/,
    },
    {
      // Chromium's headless shell does not grant pointer lock. CI runs this
      // project under Xvfb; a local run opens a short-lived window.
      name: "headed",
      testMatch: /pointerLock\.spec\.ts$/,
      use: { headless: false },
    },
  ],
  webServer: {
    command: "npm run build && npm run preview -- --host 127.0.0.1 --port 4173 --strictPort",
    url: "http://127.0.0.1:4173",
    reuseExistingServer: true,
    timeout: 180_000,
  },
});
