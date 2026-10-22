/**
 * §12.5 two-client.spec: two browser contexts join one host-axum room, see
 * each other move and receive the same pad diff.
 */

import { expect, test } from "@playwright/test";
import {
  boot,
  hostCommand,
  moveOnline,
  onlineStats,
  roomCode,
  startOnline,
} from "./helpers";

test("two clients share movement and a replicated edit", async ({ browser }) => {
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

    const before = await onlineStats(second);
    const firstStart = before.players.find((player) => player.id === 0)!;
    await moveOnline(first, 1_000);
    await second.waitForFunction(
      (start) => {
        const stats = (window as never as {
          __cbTest: {
            onlineStats(): { players: Array<{ id: number; x: number; z: number }> };
          };
        }).__cbTest.onlineStats();
        const player = stats.players.find((candidate) => candidate.id === 0);
        return player !== undefined && Math.hypot(player.x - start.x, player.z - start.z) > 0.05;
      },
      firstStart,
      { timeout: 10_000 },
    );

    const beforeDiffs = await onlineStats(second);
    await hostCommand({ room: code, action: "place-crate", team: 0 });
    for (const page of [first, second]) {
      await page.waitForFunction(
        (count) => {
          const stats = (window as never as {
            __cbTest: { onlineStats(): { frames: { chunkDiffs: number } } };
          }).__cbTest.onlineStats();
          return stats.frames.chunkDiffs > count;
        },
        beforeDiffs.frames.chunkDiffs,
        { timeout: 10_000 },
      );
    }
    expect((await onlineStats(first)).playerId).toBe(0);
    expect((await onlineStats(second)).playerId).toBe(1);
  } finally {
    await firstContext.close();
    await secondContext.close();
  }
});
