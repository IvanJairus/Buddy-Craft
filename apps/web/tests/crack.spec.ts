/**
 * Crate-only mining: terrain refuses the crack, a placed crate is the only
 * breakable block, and the five crack stages advance on a controlled clock.
 */

import { expect, test, type Page } from "@playwright/test";
import { BLOCK_CRATE } from "../src/game/blocks";

interface VisualState {
  breakable: boolean;
  placeable: boolean;
  crackVisible: boolean;
  crackStage: number;
  highlight: [number, number, number] | null;
}

interface TestWindow {
  __cbReady?: boolean;
  __cbTest?: {
    bases(): Array<{ x: number; z: number; padX: number; padZ: number; y: number }>;
    offlineGrant(player: number, crates: number): void;
    hudState(): { crates: number };
    blockAt(x: number, y: number, z: number): number;
    aim(
      origin: [number, number, number],
      direction: [number, number, number],
    ): { hit: unknown; visuals: VisualState };
    placeBlock(block: number): number;
    breakBlock(): number;
    interactionMine(
      held: boolean,
      nowMs: number,
    ): { progress: number; visuals: VisualState | null };
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
  // Placing on your own pad spends a carried crate; the first drop is at 0:10.
  await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.offlineGrant(0, 6),
  );
  await page.waitForFunction(
    () => (window as unknown as TestWindow).__cbTest!.hudState().crates >= 6,
    null,
    { timeout: 5_000 },
  );
  await page.waitForTimeout(300);
}

test("terrain is not breakable and a placed crate cracks in five stages", async ({
  page,
}: {
  page: Page;
}) => {
  await boot(page);
  const base = await page.evaluate(
    () => (window as unknown as TestWindow).__cbTest!.bases()[0],
  );
  // The pad centre is the core's protected volume; build on the last pad column.
  const padX = Math.round(base.padX) + 3;
  const padZ = Math.round(base.padZ);

  const terrain = await page.evaluate(
    ([x, y, z]) =>
      (window as unknown as TestWindow).__cbTest!.aim(
        [x + 0.5, y + 3, z + 0.5],
        [0, -1, 0],
      ).visuals,
    [padX, base.y, padZ],
  );
  expect(terrain.breakable).toBe(false);
  expect(terrain.placeable).toBe(true);

  const placed = await page.evaluate(
    (block) => (window as unknown as TestWindow).__cbTest!.placeBlock(block),
    BLOCK_CRATE,
  );
  expect(placed).toBeGreaterThan(0);

  await page.waitForFunction(
    ([x, z]) =>
      (window as unknown as TestWindow).__cbTest!.blockAt(x, 41, z) === 11,
    [padX, padZ],
    { timeout: 5_000 },
  );

  const cracks = await page.evaluate(
    ([x, z]) => {
      const api = (window as unknown as TestWindow).__cbTest!;
      // Look straight down at the crate now occupying the placement cell.
      api.aim([x + 0.5, 44, z + 0.5], [0, -1, 0]);
      const first = api.interactionMine(true, 1_000);
      const half = api.interactionMine(true, 1_200);
      const full = api.interactionMine(true, 1_400);
      const broken = api.breakBlock();
      return { first, half, full, broken };
    },
    [padX, padZ],
  );

  expect(cracks.full.visuals?.breakable).toBe(true);
  expect(cracks.first.progress).toBe(0);
  expect(cracks.half.progress).toBeCloseTo(0.5, 4);
  expect(cracks.half.visuals?.crackVisible).toBe(true);
  expect(cracks.half.visuals?.crackStage).toBe(2);
  expect(cracks.full.progress).toBe(1);
  expect(cracks.broken).toBeGreaterThan(0);
});
