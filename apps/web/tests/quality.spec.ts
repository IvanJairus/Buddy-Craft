/**
 * T-031 acceptance specs: WebGL2, the DPR clamp, the three tiers, dynamic
 * resolution and the persisted boot benchmark.
 *
 * The specs run against a production `vite preview` build, not the dev server:
 * the boot benchmark and the adaptive scaler are the things under test, and a
 * dev-only code path would prove nothing about the shipped client.
 */

import { expect, test } from "@playwright/test";
import type { RendererInfo } from "../src/render/renderer";
import {
  QUALITY,
  QUALITY_STORAGE_KEY,
  tierForFrameTime,
  type QualityTier,
} from "../src/render/quality";

interface TestWindow {
  __cbReady?: boolean;
  __cbTest?: {
    info(): RendererInfo;
    setFrameTime(ms: number | null): void;
    setTier(tier: QualityTier): void;
  };
}

async function boot(page: import("@playwright/test").Page, url: string): Promise<void> {
  await page.goto(url);
  await page.waitForFunction(() => (window as unknown as TestWindow).__cbReady === true, null, {
    timeout: 30_000,
  });
}

async function info(page: import("@playwright/test").Page): Promise<RendererInfo> {
  return page.evaluate(() => (window as unknown as TestWindow).__cbTest!.info());
}

test("the clear frame renders through WebGL2", async ({ page }) => {
  await boot(page, "/?quality=high");
  const before = await info(page);
  expect(before.webgl2).toBe(true);

  await page.waitForTimeout(1000);
  const after = await info(page);
  const frames = after.frames - before.frames;
  expect(frames).toBeGreaterThanOrEqual(55);
  expect(after.cameraFar).toBe(96);
});

test("device pixel ratio is clamped to 2", async ({ browser }) => {
  const context = await browser.newContext({
    deviceScaleFactor: 3,
    viewport: { width: 800, height: 600 },
  });
  const page = await context.newPage();
  await boot(page, "/?quality=high");

  expect(await page.evaluate(() => window.devicePixelRatio)).toBe(3);
  const state = await info(page);
  expect(state.basePixelRatio).toBe(2);
  expect(state.pixelRatioAtBoot).toBe(2);
  expect(state.pixelRatio).toBeLessThanOrEqual(2);
  await context.close();
});

test("each tier applies the exact §8.5 numbers", async ({ page }) => {
  await boot(page, "/?quality=high");
  const tiers: QualityTier[] = ["LOW", "MED", "HIGH"];
  for (const tier of tiers) {
    await page.evaluate((next) => (window as unknown as TestWindow).__cbTest!.setTier(next), tier);
    const state = await info(page);
    expect(state.tier).toBe(tier);
    expect(state.settings).toEqual(QUALITY[tier]);
    expect(state.fogNear).toBe(QUALITY[tier].fogStart);
    expect(state.fogFar).toBe(QUALITY[tier].fogEnd);
    expect(state.cameraFar).toBe(QUALITY[tier].fogEnd);
  }
});

test("dynamic resolution drops under 25 ms frames and recovers under 8 ms", async ({ page }) => {
  await boot(page, "/?quality=high");
  expect((await info(page)).scale).toBe(1);

  await page.evaluate(() => (window as unknown as TestWindow).__cbTest!.setFrameTime(25));
  await page.waitForTimeout(1000);
  const dropped = await info(page);
  expect(dropped.scale).toBeLessThan(1);
  expect(dropped.scale).toBeGreaterThanOrEqual(0.55);
  expect(dropped.pixelRatio).toBeLessThanOrEqual(2);

  await page.evaluate(() => (window as unknown as TestWindow).__cbTest!.setFrameTime(8));
  await page.waitForTimeout(2000);
  const recovered = await info(page);
  expect(recovered.scale).toBeGreaterThan(0.95);
  expect(recovered.scale).toBeLessThanOrEqual(1);
});

test("the scale never falls below 0.55", async ({ page }) => {
  await boot(page, "/?quality=high");
  // 200 ms is clamped to a 100 ms sample inside the scaler, which drives the
  // target to the floor. The floor is the assertion.
  await page.evaluate(() => (window as unknown as TestWindow).__cbTest!.setFrameTime(200));
  await page.waitForTimeout(1500);
  const state = await info(page);
  expect(state.scale).toBeGreaterThanOrEqual(0.55);
  expect(state.scale).toBeLessThan(0.6);
});

test("the benchmark thresholds make 60 Hz HIGH", () => {
  expect(tierForFrameTime(16.7)).toBe("HIGH");
  expect(tierForFrameTime(20)).toBe("MED");
  expect(tierForFrameTime(30)).toBe("LOW");
  expect(tierForFrameTime(Number.NaN)).toBe("MED");
});

test("the boot benchmark persists a tier", async ({ browser }) => {
  const context = await browser.newContext();
  const page = await context.newPage();
  await page.addInitScript(() => window.localStorage.removeItem("buddycraft.quality"));
  await boot(page, "/");

  const stored = await page.evaluate(
    (key) => window.localStorage.getItem(key),
    QUALITY_STORAGE_KEY,
  );
  expect(stored === "LOW" || stored === "MED" || stored === "HIGH").toBe(true);
  const state = await info(page);
  expect(state.tier).toBe(stored);
  expect(state.settings).toEqual(QUALITY[stored as QualityTier]);
  await context.close();
});
