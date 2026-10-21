/**
 * T-049 audio specs. Chromium is used because WebKit is not installed in this
 * workspace; the WebKit unlock row is therefore documented as not verified.
 */

import { expect, test, type Page } from "@playwright/test";
import type { AudioStats } from "../src/audio/engine";

interface TestWindow {
  __cbReady?: boolean;
  __cbTest?: {
    audioStats(): AudioStats;
    audioUnlock(): Promise<boolean>;
    audioCue(cue: string, x?: number, y?: number, z?: number): number;
    audioRumble(level: number): AudioStats;
    audioPanner(): { x: number; y: number; z: number } | null;
    audioEvent(kind: number, a: number, b: number): void;
  };
}

async function boot(page: Page): Promise<void> {
  await page.goto("/?screen=match");
  await page.waitForFunction(() => (window as unknown as TestWindow).__cbReady === true, null, {
    timeout: 30_000,
  });
  await page.locator("#game").click({ position: { x: 40, y: 40 } });
}

test("the graph has the §8.8 buses, compressor and 30 cues", async ({ page }: { page: Page }) => {
  await boot(page);
  const stats = await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.audioStats(),
  );
  expect(stats.available).toBe(true);
  expect(stats.cues).toBe(30);
  expect(stats.maxVoices).toBe(24);
  expect(stats.buses).toEqual(["sfx", "ui", "music"]);
  expect(stats.compressorThreshold).toBe(-12);
  expect(stats.compressorRatio).toBe(4);
  expect(stats.pannerModel).toBe("HRTF");
});

test("the first pointerdown unlocks the context", async ({ page }: { page: Page }) => {
  await boot(page);
  const unlocked = await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.audioUnlock(),
  );
  expect(unlocked).toBe(true);
  const stats = await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.audioStats(),
  );
  expect(stats.state).toBe("running");
});

test("a left-side cue pans left and the voice cap holds at 24", async ({ page }: { page: Page }) => {
  await boot(page);
  await page.evaluate(() => {
    const api = (window as unknown as TestWindow).__cbTest!;
    api.audioCue("sword_flesh", -4, 0, 0);
  });
  const panner = await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.audioPanner(),
  );
  expect(panner?.x).toBeCloseTo(-4, 3);

  const stats = await page.evaluate(() => {
    const api = (window as unknown as TestWindow).__cbTest!;
    for (let i = 0; i < 30; i += 1) {
      api.audioCue("ui_click");
    }
    return api.audioStats();
  });
  expect(stats.voices).toBeLessThanOrEqual(24);
});

test("a source beyond forty metres is culled", async ({ page }: { page: Page }) => {
  await boot(page);
  const played = await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.audioCue("tower_fire", 100, 0, 0),
  );
  expect(played).toBe(0);
});

test("the rumble worklet loads and accepts intensity", async ({ page }: { page: Page }) => {
  await boot(page);
  const applied = await page.evaluate(() => {
    const api = (window as unknown as TestWindow).__cbTest!;
    return api.audioRumble(0.5).rumble;
  });
  expect(applied).toBeCloseTo(0.5, 2);
  await page.waitForFunction(
    () => (window as unknown as TestWindow).__cbTest!.audioStats().workletReady === true,
    null,
    { timeout: 5_000 },
  );
  const stats = await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.audioStats(),
  );
  expect(stats.workletReady).toBe(true);
});
