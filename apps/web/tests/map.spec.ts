/**
 * Map and objective HUD acceptance: the offline worldgen bases reach the
 * client, the compass rows point at a real pad and enemy, and M/Escape toggle
 * the overlay without opening the pause menu.
 */

import { expect, test, type Page } from "@playwright/test";

interface BaseRow {
  x: number;
  z: number;
  padX: number;
  padZ: number;
  y: number;
}

interface TestWindow {
  __cbReady?: boolean;
  __cbTest?: {
    bases(): BaseRow[];
    map(): {
      open: boolean;
      bases: number;
      yourTeam: number;
      player: [number, number];
      drop: [number, number] | null;
    };
    objectives(): Array<{ key: string; label: string; distance: number; angle: number }>;
    beacons(): Array<{ x: number; z: number; visible: boolean }>;
  };
}

async function boot(page: Page): Promise<void> {
  await page.goto("/?quality=low");
  await page.waitForFunction(() => (window as unknown as TestWindow).__cbReady === true, null, {
    timeout: 30_000,
  });
  await page.getByRole("button", { name: "CONTINUE" }).click();
  await page.getByRole("button", { name: "OFFLINE" }).click();
  await page.waitForFunction(
    () => (window as unknown as TestWindow).__cbTest?.bases().length === 2,
    null,
    { timeout: 15_000 },
  );
  await page.waitForTimeout(300);
}

test("the offline bases reach the map and the compass", async ({ page }: { page: Page }) => {
  await boot(page);
  const bases = await page.evaluate(() => (window as unknown as TestWindow).__cbTest!.bases());
  expect(bases).toHaveLength(2);
  const map = await page.evaluate(() => (window as unknown as TestWindow).__cbTest!.map());
  expect(map.bases).toBe(2);
  expect(map.yourTeam).toBe(0);

  const rows = await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.objectives(),
  );
  const pad = rows.find((row) => row.key === "pad");
  const enemy = rows.find((row) => row.key === "enemy");
  expect(pad?.label).toBe("PAD");
  expect(pad?.distance).toBeLessThan(12);
  expect(enemy?.label).toBe("ENEMY");
  expect(enemy?.distance).toBeGreaterThan(60);
  expect(pad?.angle).not.toBeNaN();
  expect(enemy?.angle).not.toBeNaN();

  // The own beam is within 1 m and must not cover the camera; the enemy beam
  // is still available for navigation.
  const beacons = await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.beacons(),
  );
  expect(beacons.length).toBe(2);
  expect(beacons[0].visible).toBe(false);
  expect(beacons[1].visible).toBe(true);
});

test("M opens the map, Escape closes it and the match stays live", async ({
  page,
}: {
  page: Page;
}) => {
  await boot(page);
  await page.keyboard.press("m");
  await expect(page.locator(".cb-map")).toBeVisible();
  expect(await page.evaluate(() => (window as unknown as TestWindow).__cbTest!.map().open)).toBe(
    true,
  );
  await expect(page.locator(".cb-map-legend")).toContainText("ENEMY");
  expect(await page.evaluate(() => document.body.dataset.screen)).toBe("match");

  await page.keyboard.press("Escape");
  await expect(page.locator(".cb-map")).toBeHidden();
  expect(await page.evaluate(() => (window as unknown as TestWindow).__cbTest!.map().open)).toBe(
    false,
  );
  expect(await page.evaluate(() => document.body.dataset.screen)).toBe("match");

  await page.locator(".match-map").click();
  await expect(page.locator(".cb-map")).toBeVisible();
  await page.locator(".cb-map-close").click();
  await expect(page.locator(".cb-map")).toBeHidden();
});
