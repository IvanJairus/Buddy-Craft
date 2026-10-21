/**
 * T-044 acceptance specs: hitstop is presentation-only, damage numbers are
 * capped at 32, reduced motion keeps informational feedback, and the whole juice
 * update stays under the 1.5 ms gate at 18 entities.
 */

import { expect, test, type Page } from "@playwright/test";
import { EFFECTS, type JuiceStats } from "../src/render/juice";

interface TestWindow {
  __cbReady?: boolean;
  __cbTest?: {
    input(): { ticks: number };
    juiceStats(): JuiceStats;
    juiceEvent(kind: number, a: number, b: number, c: number): JuiceStats;
    juiceHitstop(ms?: number): JuiceStats;
    juiceDamage(amount: number, x: number, y: number, z: number): JuiceStats;
    juiceBenchmark(frames?: number): { averageMs: number; stats: JuiceStats };
    serverMessage(frame: number[]): boolean;
  };
}

async function boot(page: Page): Promise<void> {
  await page.goto("/?screen=match");
  await page.waitForFunction(() => (window as unknown as TestWindow).__cbReady === true, null, {
    timeout: 30_000,
  });
}

test("every effect in §4.9 has a juice slot", () => {
  expect([...EFFECTS]).toEqual([
    "hit_flash",
    "hitstop",
    "knockback",
    "shake",
    "damage_numbers",
    "airdrop_telegraph",
    "airdrop_landing",
    "pickup",
    "deposit",
    "craft",
    "vignette",
    "buddy_death",
    "low_hp",
    "slow_motion",
  ]);
});

test("every event kind reaches a handler", async ({ page }: { page: Page }) => {
  await boot(page);
  const stats = await page.evaluate(() => {
    const api = (window as unknown as TestWindow).__cbTest!;
    let last = api.juiceStats();
    for (let kind = 0; kind <= 9; kind += 1) {
      last = api.juiceEvent(kind, 0, 20, 0);
    }
    return last;
  });
  expect(stats.eventCount).toBeGreaterThanOrEqual(10);
});

test("events from RenderDesc reach the juice layer", async ({ page }: { page: Page }) => {
  await boot(page);
  // A keyframe with no entities and one HIT event: id 0, 20 damage.
  const frame = [0x84, 0x01, 0x01, 0x00, 0x01, 0, 0, 0, 0, 0x01, 0x05, 0, 20, 0];
  const accepted = await page.evaluate(
    (bytes) => (window as unknown as TestWindow).__cbTest!.serverMessage(bytes),
    frame,
  );
  expect(accepted).toBe(true);
  await page.waitForFunction(
    () => (window as unknown as TestWindow).__cbTest!.juiceStats().eventCount >= 1,
    null,
    { timeout: 5_000 },
  );
  const stats = await page.evaluate(() => (window as unknown as TestWindow).__cbTest!.juiceStats());
  expect(stats.damageActive).toBeGreaterThan(0);
});

test("hitstop_does_not_stop_simulation", async ({ page }: { page: Page }) => {
  await boot(page);
  const before = await page.evaluate(() => {
    const api = (window as unknown as TestWindow).__cbTest!;
    const stats = api.juiceHitstop(600);
    return { ticks: api.input().ticks, presented: stats.presentationSeconds, scale: stats.timeScale };
  });
  expect(before.scale).toBe(0);
  await page.waitForTimeout(120);
  const after = await page.evaluate(() => {
    const api = (window as unknown as TestWindow).__cbTest!;
    const stats = api.juiceStats();
    return { ticks: api.input().ticks, presented: stats.presentationSeconds, scale: stats.timeScale };
  });
  expect(after.ticks).toBeGreaterThan(before.ticks);
  expect(after.presented - before.presented).toBeLessThan(0.03);
  expect(after.scale).toBe(0);
});

test("damage numbers cap at 32 and evict the oldest", async ({ page }: { page: Page }) => {
  await boot(page);
  const stats = await page.evaluate(() => {
    const api = (window as unknown as TestWindow).__cbTest!;
    let last = api.juiceStats();
    for (let i = 0; i < 40; i += 1) {
      last = api.juiceDamage(10 + i, 128 + (i % 8), 42, 128 + Math.floor(i / 8));
    }
    return last;
  });
  expect(stats.damageActive).toBe(32);
  expect(stats.damageEvicted).toBe(8);
});

test("reduced motion disables shake, vignette and slow motion only", async ({
  page,
}: {
  page: Page;
}) => {
  await boot(page);
  await page.evaluate(() => document.body.classList.add("cb-reduced-motion"));
  await page.evaluate(() => {
    const api = (window as unknown as TestWindow).__cbTest!;
    api.juiceEvent(2, 0, 0, 0);
    api.juiceEvent(5, 0, 20, 1);
    api.juiceEvent(3, 0, 0, 0);
    api.juiceDamage(20, 128, 42, 128);
  });
  await page.waitForTimeout(40);
  const reduced = await page.evaluate(() => (window as unknown as TestWindow).__cbTest!.juiceStats());
  expect(reduced.shakeActive).toBe(false);
  expect(reduced.vignette).toBe(0);
  expect(reduced.timeScale).toBe(1);
  expect(reduced.flashes).toBeGreaterThan(0);
  expect(reduced.damageActive).toBeGreaterThan(0);

  await page.evaluate(() => document.body.classList.remove("cb-reduced-motion"));
  await page.evaluate(() => {
    (window as unknown as TestWindow).__cbTest!.juiceEvent(2, 0, 0, 0);
  });
  await page.waitForTimeout(40);
  const normal = await page.evaluate(() => (window as unknown as TestWindow).__cbTest!.juiceStats());
  expect(normal.shakeActive).toBe(true);
  expect(normal.vignette).toBeGreaterThan(0);
});

test("the juice update costs at most 1.5 ms at 18 entities", async ({ page }: { page: Page }) => {
  await boot(page);
  const result = await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.juiceBenchmark(60),
  );
  expect(result.averageMs).toBeLessThanOrEqual(1.5);
});
