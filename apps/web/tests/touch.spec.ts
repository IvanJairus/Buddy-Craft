/**
 * T-036 acceptance specs: two-finger move and look, explicit mode taps,
 * hold-to-deposit at 6/s, the stance cycle and 44 px targets.
 *
 * A mobile viewport with touch is used, and the two-finger gesture is sent
 * through CDP `Input.dispatchTouchEvent`: Playwright's `touchscreen.tap` is
 * single-touch and cannot prove that the stick and look run together.
 */

import { expect, test, type Page } from "@playwright/test";
import type { TouchState } from "../src/game/inputTouch";

test.use({ viewport: { width: 430, height: 900 }, hasTouch: true, isMobile: true });

interface TestWindow {
  __cbReady?: boolean;
  __cbTest?: {
    touch(): TouchState | null;
  };
}

async function boot(page: Page): Promise<void> {
  await page.goto("/?quality=high&scene=clear&touch=1&screen=match");
  await page.waitForFunction(() => (window as unknown as TestWindow).__cbReady === true, null, {
    timeout: 30_000,
  });
  await page.waitForFunction(
    () => ((window as unknown as TestWindow).__cbTest?.touch()?.polls ?? 0) > 0,
    null,
    { timeout: 5_000 },
  );
}

async function state(page: Page): Promise<TouchState> {
  const value = await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.touch(),
  );
  expect(value).not.toBeNull();
  return value!;
}

test("stick_and_look_are_independent", async ({ page }) => {
  await boot(page);
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("Input.dispatchTouchEvent", {
    type: "touchStart",
    touchPoints: [
      { x: 100, y: 400, id: 1, radiusX: 1, radiusY: 1, force: 1 },
      { x: 330, y: 300, id: 2, radiusX: 1, radiusY: 1, force: 1 },
    ],
  });
  await page.waitForTimeout(100);
  let touch = await state(page);
  expect(touch.stickActive).toBe(true);
  expect(touch.lookActive).toBe(true);

  await cdp.send("Input.dispatchTouchEvent", {
    type: "touchMove",
    touchPoints: [
      { x: 160, y: 350, id: 1, radiusX: 1, radiusY: 1, force: 1 },
      { x: 380, y: 350, id: 2, radiusX: 1, radiusY: 1, force: 1 },
    ],
  });
  await page.waitForTimeout(100);
  touch = await state(page);
  expect(touch.moveX).toBeGreaterThan(0);
  expect(touch.moveY).toBeGreaterThan(0);
  expect(touch.yaw).not.toBe(0);
  const yawDelta = ((touch.yaw + 384) % 256) - 128;
  expect(yawDelta).toBeLessThan(0);
  expect(touch.frame[3] >> 4).toBeGreaterThan(0);
  expect(touch.frame[3] & 0x0f).toBeGreaterThanOrEqual(8);

  await cdp.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
  await page.waitForTimeout(100);
  touch = await state(page);
  expect(touch.stickActive).toBe(false);
  expect(touch.lookActive).toBe(false);
  expect(touch.moveX).toBe(0);
  expect(touch.moveY).toBe(0);
});

test("mode-based taps switch between break and place", async ({ page }) => {
  await boot(page);
  expect((await state(page)).mode).toBe("break");
  expect(await page.locator(".cb-touch-crosshair").getAttribute("data-mode")).toBe("break");

  await page.touchscreen.tap(330, 300);
  await page.waitForTimeout(100);
  expect((await state(page)).breaks).toBe(1);

  await page.locator('.cb-touch-slot[data-slot="1"]').tap();
  await page.waitForTimeout(100);
  const placeMode = await state(page);
  expect(placeMode.mode).toBe("place");
  expect(placeMode.selectedBlock).toBe(1);
  expect(await page.locator(".cb-touch-crosshair").getAttribute("data-mode")).toBe("place");

  await page.touchscreen.tap(330, 300);
  await page.waitForTimeout(100);
  expect((await state(page)).places).toBe(1);
  expect((await state(page)).breaks).toBe(1);
});

test("holding in place mode deposits at six per second", async ({ page }) => {
  await boot(page);
  await page.locator('.cb-touch-slot[data-slot="1"]').tap();
  await page.waitForTimeout(100);
  const before = (await state(page)).places;

  const cdp = await page.context().newCDPSession(page);
  await cdp.send("Input.dispatchTouchEvent", {
    type: "touchStart",
    touchPoints: [{ x: 330, y: 300, id: 3, radiusX: 1, radiusY: 1, force: 1 }],
  });
  await page.waitForTimeout(750);
  await cdp.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
  await page.waitForTimeout(100);

  const placed = (await state(page)).places - before;
  expect(placed).toBeGreaterThanOrEqual(3);
  expect(placed).toBeLessThanOrEqual(6);
});

test("stance cycles, crosshair and controls meet the size gate", async ({ page }) => {
  await boot(page);
  const stanceButton = page.locator(".cb-touch-stance");
  expect(await stanceButton.getAttribute("data-stance")).toBe("follow");
  expect((await stanceButton.textContent())?.trim()).toBe("FOLLOW");

  await stanceButton.tap();
  expect(await stanceButton.getAttribute("data-stance")).toBe("hold");
  expect((await stanceButton.textContent())?.trim()).toBe("HOLD");
  await stanceButton.tap();
  expect(await stanceButton.getAttribute("data-stance")).toBe("assault");
  await stanceButton.tap();
  expect(await stanceButton.getAttribute("data-stance")).toBe("follow");

  for (const selector of [".cb-touch-jump", ".cb-touch-stance"]) {
    const box = await page.locator(selector).boundingBox();
    expect(box).not.toBeNull();
    expect(box!.width).toBeGreaterThanOrEqual(44);
    expect(box!.height).toBeGreaterThanOrEqual(44);
  }
  const slots = page.locator(".cb-touch-slot");
  await expect(slots).toHaveCount(9);
  for (let index = 0; index < 9; index += 1) {
    const box = await slots.nth(index).boundingBox();
    expect(box).not.toBeNull();
    expect(box!.width).toBeGreaterThanOrEqual(44);
    expect(box!.height).toBeGreaterThanOrEqual(44);
  }
});
