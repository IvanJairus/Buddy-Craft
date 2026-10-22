/**
 * §12.5 offline-match.spec: offline FFA, crate pickup raises the HUD carry
 * count, and a deposited 3 x 2 shape completes the sword recipe.
 *
 * The offline worker is advanced in batches so the test does not wait through
 * a real 15-second drop or a three-minute walk. Every tick, craft and event is
 * still produced by `room::Match` inside wasm.
 */

import { expect, test } from "@playwright/test";
import {
  KIND_CRAFT,
  advance,
  boot,
  events,
  feed,
  grant,
  hudCrates,
  placeRecipe,
  startOffline,
  waitForEvent,
} from "./helpers";

test("offline FFA: pickup raises carry and a 3 x 2 deposit crafts the sword", async ({
  page,
}) => {
  await boot(page);
  await startOffline(page, 0, 6);

  await grant(page, 0, 6);
  await advance(page, 20);
  expect(await hudCrates(page)).toBe(6);
  expect((await feed(page)).join(" ")).toContain("+6 crates");

  await placeRecipe(page, 0, 0);
  await advance(page, 40);

  const craft = await waitForEvent(page, KIND_CRAFT);
  expect(craft.b).toBe(0);
  const swordCrafts = (await events(page)).filter(
    (event) => event.kind === KIND_CRAFT && event.b === 0,
  );
  expect(swordCrafts.length).toBeGreaterThanOrEqual(1);
  expect((await feed(page)).join(" ")).toContain("R01_SWORD crafted");
});
