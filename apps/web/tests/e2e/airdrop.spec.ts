/**
 * T-054 / §12.5 airdrop.spec: the full attention loop reaches the client.
 *
 * The first drop is scheduled 10 s in, with a 5 s telegraph. The worker is
 * advanced in batches, then a granted pickup and a deposited 3 x 2 recipe
 * complete the chain. The events are read from the same S2C snapshots the HUD
 * consumes, not from a test-side simulation.
 */

import { expect, test } from "@playwright/test";
import {
  KIND_AIRDROP,
  KIND_CRAFT,
  KIND_DEPOSIT,
  KIND_TELEGRAPH,
  advance,
  boot,
  feed,
  grant,
  hudCrates,
  placeRecipe,
  startOffline,
  waitForEvent,
} from "./helpers";

test("telegraph, landing, pickup, deposit and craft all reach the HUD", async ({
  page,
}) => {
  await boot(page);
  await startOffline(page, 0, 6);

  await advance(page, 130);
  await waitForEvent(page, KIND_TELEGRAPH);
  expect((await feed(page)).join(" ")).toContain("Airdrop incoming");

  await advance(page, 110);
  await waitForEvent(page, KIND_AIRDROP);

  await grant(page, 0, 3);
  await advance(page, 20);
  await page.waitForFunction(
    () =>
      (window as never as {
        __cbTest: {
          sessionEvents(): Array<{ kind: number; a: number; b: number }>;
        };
      }).__cbTest
        .sessionEvents()
        .some((event) => event.kind === 6 && event.a === 0 && event.b === 3),
    null,
    { timeout: 15_000 },
  );
  expect(await hudCrates(page)).toBe(3);

  await placeRecipe(page, 0, 0);
  await advance(page, 40);
  await waitForEvent(page, KIND_DEPOSIT);
  await waitForEvent(page, KIND_CRAFT);
  expect((await feed(page)).join(" ")).toContain("R01_SWORD crafted");
});
