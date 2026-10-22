/**
 * §12.5 base-destroy.spec: two humans in one online room see the same
 * elimination, with a victory screen for the survivor and a defeat screen for
 * the eliminated team. There is no spectator camera.
 */

import { expect, test } from "@playwright/test";
import {
  boot,
  hostCommand,
  onlineStats,
  roomCode,
  screen,
  startOnline,
} from "./helpers";

test("the winner and the loser get opposite result screens", async ({ browser }) => {
  const code = roomCode();
  const firstContext = await browser.newContext();
  const secondContext = await browser.newContext();
  const first = await firstContext.newPage();
  const second = await secondContext.newPage();
  try {
    for (const page of [first, second]) {
      await boot(page, "/?quality=low");
      await startOnline(page, code);
    }
    for (const page of [first, second]) {
      await page.waitForFunction(
        () => {
          const stats = (window as never as {
            __cbTest: { onlineStats(): { players: Array<{ id: number }> } };
          }).__cbTest.onlineStats();
          return (
            stats.players.some((player) => player.id === 0) &&
            stats.players.some((player) => player.id === 1)
          );
        },
        null,
        { timeout: 20_000 },
      );
    }

    for (const team of [1, 2, 3, 4, 5]) {
      await hostCommand({ room: code, action: "damage-core", team, amount: 100_000 });
    }
    for (const page of [first, second]) {
      await page.waitForFunction(
        () =>
          (window as never as {
            __cbTest: { screen(): string };
          }).__cbTest.screen() === "results",
        null,
        { timeout: 20_000 },
      );
    }

    await expect(first.locator('[data-results="title"]')).toHaveText("VICTORY");
    await expect(second.locator('[data-results="title"]')).toHaveText("DEFEAT");
    expect(await screen(first)).toBe("results");
    expect(await screen(second)).toBe("results");
    expect(await first.locator("#game").isVisible()).toBe(false);
    expect(await second.locator("#game").isVisible()).toBe(false);
    expect(await onlineStats(second)).toMatchObject({ active: false });
  } finally {
    await firstContext.close();
    await secondContext.close();
  }
});
