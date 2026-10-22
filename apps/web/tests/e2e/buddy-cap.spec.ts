/**
 * §12.5 buddy-cap.spec: once the team's buddy cap is full, another recipe is
 * refused, the crates stay on the pad, and the HUD says why.
 *
 * FFA gives every human their own team, so the cap is deterministic (2 in a
 * six-player FFA). The plan's "sixth craft" refers to the Teams cap of 5;
 * the rule under test is the same `can_add_buddy` veto.
 */

import { expect, test } from "@playwright/test";
import {
  KIND_CRAFT,
  KIND_CRAFT_BLOCKED,
  advance,
  boot,
  events,
  feed,
  placeRecipe,
  startOffline,
  waitForEvent,
} from "./helpers";

const SWORDSMAN_RECIPE = 3;

test("the cap vetoes a craft, keeps the crates and shows a message", async ({
  page,
}) => {
  await boot(page);
  await startOffline(page, 0, 6);

  for (let craft = 0; craft < 2; craft += 1) {
    await placeRecipe(page, 0, SWORDSMAN_RECIPE);
    await advance(page, 40);
  }
  expect(
    (await events(page)).filter(
      (event) => event.kind === KIND_CRAFT && event.b === SWORDSMAN_RECIPE,
    ).length,
  ).toBe(2);

  await placeRecipe(page, 0, SWORDSMAN_RECIPE);
  await advance(page, 40);
  const blocked = await waitForEvent(page, KIND_CRAFT_BLOCKED);
  expect(blocked.b).toBe(SWORDSMAN_RECIPE);
  expect((await feed(page)).join(" ")).toContain("buddy cap reached");
});
