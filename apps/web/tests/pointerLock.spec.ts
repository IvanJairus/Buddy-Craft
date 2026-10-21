/**
 * T-035's pointer-lock spec.
 *
 * Chromium's headless shell refuses `requestPointerLock`, even from a trusted
 * click; a headed browser grants it. This file is therefore its own Playwright
 * project with `headless: false`, and CI runs the suite under Xvfb. The rest of
 * the input contract stays in `input.spec.ts`, which is headless.
 */

import { expect, test } from "@playwright/test";
import { FIRE, USE } from "../src/game/inputDesktop";

interface InputState {
  yaw: number;
  pointerLocked: boolean;
  frame: number[];
}

interface TestWindow {
  __cbReady?: boolean;
  __cbTest?: {
    input(): InputState;
  };
}

test.use({ headless: false });

test("pointer lock on click, mouse look and fire button", async ({ page }) => {
  await page.goto("/?quality=high&scene=clear&screen=match");
  await page.waitForFunction(() => (window as unknown as TestWindow).__cbReady === true, null, {
    timeout: 30_000,
  });
  await page.bringToFront();
  for (let attempt = 0; attempt < 3; attempt += 1) {
    await page.locator("#game").click({ position: { x: 640, y: 360 } });
    await page.waitForTimeout(250);
    const locked = await page.evaluate(
      () => (window as unknown as TestWindow).__cbTest!.input().pointerLocked,
    );
    if (locked) {
      break;
    }
    await page.bringToFront();
    await page.waitForTimeout(150);
  }
  expect((await page.evaluate(() => (window as unknown as TestWindow).__cbTest!.input())).pointerLocked).toBe(true);
  expect(await page.evaluate(() => document.pointerLockElement?.id ?? null)).toBe("game");

  const yawBefore = (await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.input(),
  )).yaw;
  await page.mouse.move(760, 360, { steps: 3 });
  await page.waitForTimeout(100);
  const yawAfter = (await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.input(),
  )).yaw;
  expect(yawAfter).not.toBe(yawBefore);

  await page.mouse.down({ button: "left" });
  await page.waitForTimeout(100);
  const firing = await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.input(),
  );
  expect(firing.frame[0] & FIRE).toBe(FIRE);
  await page.mouse.up({ button: "left" });

  await page.mouse.down({ button: "right" });
  await page.waitForTimeout(100);
  const using = await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.input(),
  );
  expect(using.frame[0] & USE).toBe(USE);
  await page.mouse.up({ button: "right" });
});
