/**
 * T-054 / D-05 bot-win.spec: eliminating the only human ends the match with no
 * winner even though friendly bots are still alive on that team.
 *
 * Offline Teams 2 seats bots beside the human. Destroying the human core kills
 * the team's human; the outcome is `NoHumans`, not a bot victory.
 */

import { expect, test } from "@playwright/test";
import { advance, boot, screen, startOffline } from "./helpers";

test("the last human falling ends the match with no winner", async ({ page }) => {
  await boot(page);
  await startOffline(page, 1, 2);

  await page.evaluate(() =>
    (window as never as {
      __cbTest: { offlineDamageCore(team: number, amount: number): void };
    }).__cbTest.offlineDamageCore(0, 100_000),
  );
  await advance(page, 40);
  await page.waitForFunction(
    () =>
      (window as never as {
        __cbTest: { screen(): string };
      }).__cbTest.screen() === "results",
    null,
    { timeout: 15_000 },
  );

  expect(await screen(page)).toBe("results");
  await expect(page.locator('[data-results="title"]')).toHaveText("NO HUMAN SURVIVOR");
  await expect(page.locator('[data-results="detail"]')).toContainText("Bots do not win");
});
