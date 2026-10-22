/**
 * §12.5 recipe-negative.spec: seven connected crates that are not a recipe
 * never craft, and the HUD's near-miss banner comes from Rust.
 *
 * The plan's wording says "HUD shows needs 6"; five of the six sword blocks
 * are the shape that actually produces a one-crate miss, so that is the banner
 * fixture. The seven-block placement is the negative case: it has an extra
 * crate, which is deliberately not reported as a near miss.
 */

import { expect, test } from "@playwright/test";
import {
  KIND_CRAFT,
  advance,
  boot,
  events,
  nearMiss,
  placeBlocks,
  startOffline,
} from "./helpers";

const SWORD: Array<[number, number, number]> = [
  [0, 0, 0],
  [1, 0, 0],
  [2, 0, 0],
  [0, 0, 1],
  [1, 0, 1],
  [2, 0, 1],
];

test("seven crates do not craft and the near-miss banner names the sword", async ({
  page,
}) => {
  await boot(page);
  await startOffline(page, 0, 6);

  await placeBlocks(page, 0, [...SWORD, [3, 0, 0]]);
  await advance(page, 40);
  expect(
    (await events(page)).filter(
      (event) => event.kind === KIND_CRAFT && event.b === 0,
    ),
  ).toEqual([]);

  const near = await nearMiss(page, SWORD.slice(0, 5));
  expect(near).not.toBeNull();
  expect(near!.recipeId).toBe(0);
  expect(near!.missing).toBe(1);
  await expect(page.locator(".recipe-near")).toHaveText(
    "SWORD: needs 1 more crate",
  );
});
