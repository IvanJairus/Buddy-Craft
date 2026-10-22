/**
 * §12.5 reconnect.spec: drop the socket, reconnect within 30 s, control
 * resumes and the entity count does not grow.
 */

import { expect, test } from "@playwright/test";
import {
  boot,
  moveOnline,
  onlineDrop,
  onlineStats,
  roomCode,
  startOnline,
} from "./helpers";

test("a dropped socket reconnects without duplicating the player", async ({ page }) => {
  await boot(page);
  const code = roomCode();
  await startOnline(page, code);
  const before = await onlineStats(page);
  expect(before.state).toBe("open");

  await onlineDrop(page);
  await page.waitForFunction(
    (tick) => {
      const stats = (window as never as {
        __cbTest: { onlineStats(): { state: string; snapshot: boolean; tick: number } };
      }).__cbTest.onlineStats();
      return stats.state === "open" && stats.snapshot && stats.tick > tick;
    },
    before.tick,
    { timeout: 15_000 },
  );

  await page.waitForFunction(
    (count) => {
      const stats = (window as never as {
        __cbTest: { onlineStats(): { players: unknown[] } };
      }).__cbTest.onlineStats();
      return stats.players.length === count;
    },
    before.players.length,
    { timeout: 15_000 },
  );
  const reconnected = await onlineStats(page);
  expect(reconnected.state).toBe("open");
  expect(reconnected.playerId).toBe(before.playerId);
  expect(reconnected.players.length).toBe(before.players.length);

  const start = reconnected.players.find((player) => player.id === reconnected.playerId)!;
  await moveOnline(page, 900);
  await page.waitForTimeout(300);
  const after = await onlineStats(page);
  const end = after.players.find((player) => player.id === after.playerId)!;
  expect(Math.hypot(end.x - start.x, end.z - start.z)).toBeGreaterThan(0.05);
});
