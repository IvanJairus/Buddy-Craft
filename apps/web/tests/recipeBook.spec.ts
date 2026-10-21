/**
 * T-038 acceptance specs for the recipe UI.
 *
 * The 500-cluster parity test lives in `sim::recipe::tests::near_miss_never_lies`
 * because the prediction is Rust logic. These specs check that every card is
 * built from the wasm table, locked rows stay visible with their unlock time,
 * the banner carries Rust's answer, and the ghost is one instanced placement.
 */

import { expect, test, type Page } from "@playwright/test";
import type { RecipeRow } from "../src/wasm/loader";

interface RecipeBookInfo {
  open: boolean;
  cards: number;
  locked: number;
  unlocked: number;
  nearMiss: string | null;
}

interface TestWindow {
  __cbReady?: boolean;
  __cbTest?: {
    recipeRows(): RecipeRow[];
    recipesInfo(): RecipeBookInfo;
    toggleRecipes(elapsed?: number): void;
    showNearMiss(
      blocks: Array<[number, number, number]>,
      elapsed: number,
    ): { recipeId: number; name: string; missing: number } | null;
    ghostShow(
      recipeId: number,
      origin: [number, number, number],
    ): { visible: boolean; count: number; blocks: Array<[number, number, number]> } | null;
    ghostHide(): { visible: boolean; count: number; blocks: Array<[number, number, number]> };
  };
}

async function boot(page: Page): Promise<void> {
  await page.goto("/?quality=high&scene=clear");
  await page.waitForFunction(() => (window as unknown as TestWindow).__cbReady === true, null, {
    timeout: 30_000,
  });
}

function displayName(name: string): string {
  return name.replace(/^R\d+_/, "").replace(/_/g, " ");
}

test("every recipe card comes from the wasm table", async ({ page }) => {
  await boot(page);
  const rows = await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.recipeRows(),
  );
  expect(rows).toHaveLength(14);
  for (const row of rows) {
    expect(row.blocks).toHaveLength(row.cost);
  }

  await page.locator(".recipe-toggle").click();
  const result = await page.evaluate(() => {
    const api = (window as unknown as TestWindow).__cbTest!;
    const info = api.recipesInfo();
    const cards = [...document.querySelectorAll<HTMLElement>(".recipe-card")];
    const rows = api.recipeRows();
    const sword = rows.find((row) => row.name === "R01_SWORD")!;
    const maxX = Math.max(...sword.blocks.map((block) => block[0]));
    const maxZ = Math.max(...sword.blocks.map((block) => block[2]));
    const card = document.querySelector(`[data-recipe="${sword.id}"]`)!;
    const grid = card.querySelector<HTMLElement>(".recipe-grid")!;
    const cells = grid.querySelectorAll(".recipe-cell");
    const filled = grid.querySelectorAll('.recipe-cell[data-filled="true"]');
    const lockTexts = cards
      .filter((element) => element.dataset.locked === "true")
      .map((element) => element.querySelector(".recipe-lock")?.textContent ?? "");
    return {
      info,
      cardCount: cards.length,
      lockedCards: cards.filter((element) => element.dataset.locked === "true").length,
      lockTexts,
      gridCells: cells.length,
      gridFilled: filled.length,
      expectedCells: (maxX + 1) * (maxZ + 1),
      cols: grid.style.getPropertyValue("--cols"),
      expectedCols: String(maxX + 1),
      open: document.querySelector(".recipe-book")?.classList.contains("open") ?? false,
    };
  });

  expect(result.open).toBe(true);
  expect(result.info.cards).toBe(14);
  expect(result.cardCount).toBe(14);
  expect(result.info.locked).toBeGreaterThan(0);
  expect(result.lockedCards).toBe(result.info.locked);
  expect(result.lockTexts.every((text) => /^Unlocks \d+:\d\d$/.test(text))).toBe(true);
  expect(result.gridFilled).toBeGreaterThan(0);
  expect(result.gridCells).toBe(result.expectedCells);
  expect(result.cols).toBe(result.expectedCols);
  await page.locator(".recipe-close").click();
});

test("near-miss uses Rust's answer and the banner names the recipe", async ({ page }) => {
  await boot(page);
  const rows = await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.recipeRows(),
  );
  const row = rows.find((candidate) => candidate.name === "R01_SWORD")!;

  const near = await page.evaluate((source) => {
    const api = (window as unknown as TestWindow).__cbTest!;
    for (let removed = 0; removed < source.blocks.length; removed += 1) {
      const cluster = source.blocks.filter((_, index) => index !== removed);
      const result = api.showNearMiss(cluster, 999_999);
      if (result !== null) {
        const banner = document.querySelector(".recipe-near");
        return {
          result,
          banner: banner?.textContent ?? "",
          visible: banner?.classList.contains("visible") ?? false,
          bannerRecipe: (banner as HTMLElement | null)?.dataset.recipe ?? null,
        };
      }
    }
    return null;
  }, row);

  expect(near).not.toBeNull();
  expect(near!.result.missing).toBe(1);
  expect(near!.visible).toBe(true);
  expect(near!.bannerRecipe).toBe(String(near!.result.recipeId));
  expect(near!.banner).toBe(
    `${displayName(near!.result.name)}: needs 1 more crate`,
  );

  const exact = await page.evaluate((source) => {
    const api = (window as unknown as TestWindow).__cbTest!;
    const result = api.showNearMiss(source.blocks, 999_999);
    const banner = document.querySelector(".recipe-near");
    return { result, visible: banner?.classList.contains("visible") ?? false };
  }, row);
  expect(exact.result).toBeNull();
  expect(exact.visible).toBe(false);
});

test("the ghost is one translucent copy of the recipe", async ({ page }) => {
  await boot(page);
  const rows = await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.recipeRows(),
  );
  const row = rows.find((candidate) => candidate.name === "R01_SWORD")!;

  const shown = await page.evaluate(
    ({ id }) => (window as unknown as TestWindow).__cbTest!.ghostShow(id, [0, 0, 0]),
    { id: row.id },
  );
  expect(shown).not.toBeNull();
  expect(shown!.visible).toBe(true);
  expect(shown!.count).toBe(row.cost);
  expect(shown!.blocks).toHaveLength(row.cost);

  const hidden = await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.ghostHide(),
  );
  expect(hidden.visible).toBe(false);
  expect(hidden.count).toBe(0);
});
