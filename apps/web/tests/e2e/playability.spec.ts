/**
 * The local view must let a player aim: the camera sits at eye height, the
 * local body does not cover the crosshair, and a desktop reticle marks it.
 */

import { expect, test } from "@playwright/test";
import { boot, startOffline } from "./helpers";

const EYE_HEIGHT = 1.62;

test("offline view is first-person, hides the local body and shows a reticle", async ({
  page,
}, testInfo) => {
  await boot(page);
  await startOffline(page, 0, 6);
  await page.waitForTimeout(800);

  const view = await page.evaluate(() => {
    const api = (
      window as unknown as {
        __cbTest: {
          offlineStats(): { x: number; y: number; z: number };
          camera(): { x: number; y: number; z: number };
          entities(): { hiddenLocal: number } | null;
        };
      }
    ).__cbTest;
    return {
      player: api.offlineStats(),
      camera: api.camera(),
      entities: api.entities(),
    };
  });

  expect(view.entities?.hiddenLocal).toBe(0);
  expect(Math.abs(view.camera.y - (view.player.y + EYE_HEIGHT))).toBeLessThan(0.12);
  expect(
    Math.hypot(view.camera.x - view.player.x, view.camera.z - view.player.z),
  ).toBeLessThan(0.12);

  if (testInfo.project.name.startsWith("mobile")) {
    await expect(page.locator('[data-hud="reticle"]')).toBeHidden();
    return;
  }

  const reticle = page.locator('[data-hud="reticle"]');
  await expect(reticle).toBeVisible();
  const box = await reticle.boundingBox();
  expect(box).not.toBeNull();
  const viewport = page.viewportSize();
  expect(viewport).not.toBeNull();
  expect(Math.abs(box!.x + box!.width / 2 - viewport!.width / 2)).toBeLessThan(0.5);
  expect(Math.abs(box!.y + box!.height / 2 - viewport!.height / 2)).toBeLessThan(0.5);
  expect(await reticle.evaluate((node) => getComputedStyle(node).pointerEvents)).toBe(
    "none",
  );
});
