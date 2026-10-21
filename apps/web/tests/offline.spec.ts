/**
 * T-048 acceptance specs: the offline worker starts a bot-filled room, accepts
 * input while the browser network is disabled, and can complete a match.
 */

import { expect, test, type Page } from "@playwright/test";

interface OfflineStats {
  active: boolean;
  snapshot: boolean;
  state: string;
  players: number;
  buddies: number;
  decoded: number;
  hash: number;
  ended: boolean;
  x: number;
  y: number;
  z: number;
}

interface TestWindow {
  __cbReady?: boolean;
  __cbTest?: {
    screen(): string;
    offlineStart(mode?: number, teams?: number): boolean;
    offlineStats(): OfflineStats;
    offlineAdvance(ticks: number): void;
    offlineInput(frame: number[]): void;
    offlineStop(): void;
  };
}

async function boot(page: Page): Promise<void> {
  await page.goto("/");
  await page.waitForFunction(() => (window as unknown as TestWindow).__cbReady === true, null, {
    timeout: 30_000,
  });
}

test("the offline worker starts a bot-filled match", async ({ page }: { page: Page }) => {
  await boot(page);
  await page.evaluate(() => (window as unknown as TestWindow).__cbTest!.offlineStart());
  await page.waitForFunction(
    () => {
      const stats = (window as unknown as TestWindow).__cbTest!.offlineStats();
      return stats.state === "open" && stats.decoded > 0;
    },
    null,
    { timeout: 15_000 },
  );
  const stats = await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.offlineStats(),
  );
  expect(stats.active).toBe(true);
  expect(stats.snapshot).toBe(true);
  expect(stats.players).toBeGreaterThanOrEqual(2);
  expect(await page.evaluate(() => (window as unknown as TestWindow).__cbTest!.screen())).toBe(
    "match",
  );
});

test("FFA offline also starts a bot-filled room", async ({ page }: { page: Page }) => {
  await boot(page);
  await page.evaluate(() => (window as unknown as TestWindow).__cbTest!.offlineStart(0, 6));
  await page.waitForFunction(
    () => {
      const stats = (window as unknown as TestWindow).__cbTest!.offlineStats();
      return stats.active && stats.decoded > 0;
    },
    null,
    { timeout: 15_000 },
  );
  const stats = await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.offlineStats(),
  );
  expect(stats.snapshot).toBe(true);
});

test("input still moves the player with the network disabled", async ({ page }: { page: Page }) => {
  await boot(page);
  await page.evaluate(() => (window as unknown as TestWindow).__cbTest!.offlineStart());
  await page.waitForFunction(
    () => (window as unknown as TestWindow).__cbTest!.offlineStats().decoded > 0,
    null,
    { timeout: 15_000 },
  );
  await page.context().setOffline(true);
  const before = await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.offlineStats(),
  );
  await page.keyboard.down("w");
  await page.waitForTimeout(800);
  await page.keyboard.up("w");
  const after = await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.offlineStats(),
  );
  expect(after.decoded).toBeGreaterThan(before.decoded);
  expect(Math.hypot(after.x - before.x, after.z - before.z)).toBeGreaterThan(0.05);
  await page.context().setOffline(false);
});

test("a full bot match reaches the results screen", async ({ page }: { page: Page }) => {
  await boot(page);
  test.setTimeout(45_000);
  await page.evaluate(() => (window as unknown as TestWindow).__cbTest!.offlineStart());
  await page.waitForFunction(
    () => (window as unknown as TestWindow).__cbTest!.offlineStats().decoded > 0,
    null,
    { timeout: 15_000 },
  );
  await page.evaluate(() => (window as unknown as TestWindow).__cbTest!.offlineAdvance(60_000));
  await page.waitForFunction(
    () => (window as unknown as TestWindow).__cbTest!.screen() === "results",
    null,
    { timeout: 40_000 },
  );
  const stats = await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.offlineStats(),
  );
  expect(stats.ended).toBe(true);
  expect(stats.hash).not.toBe(0);
});
