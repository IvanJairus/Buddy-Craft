/**
 * §12.5 touch-controls.spec: two-finger stick/look, tap-to-break and
 * tap-to-place on a mobile viewport. The synthetic touches are dispatched
 * directly because Playwright's touchscreen API is single-touch.
 */

import { expect, test, type Page } from "@playwright/test";
import {
  boot,
  startOffline,
  touchState,
  type Stats,
} from "./helpers";

interface TouchPoint {
  id: number;
  x: number;
  y: number;
}

async function dispatchTouch(page: Page, type: string, points: TouchPoint[]): Promise<void> {
  await page.evaluate(
    ([kind, raw]) => {
      const root = document.querySelector(".cb-touch-root");
      if (!(root instanceof HTMLElement)) {
        throw new Error("touch root is missing");
      }
      const changed = (raw as Array<{ id: number; x: number; y: number }>).map((point) => ({
        identifier: point.id,
        clientX: point.x,
        clientY: point.y,
        pageX: point.x,
        pageY: point.y,
      }));
      const active = kind === "touchend" ? [] : changed;
      const event = new Event(kind as string, { bubbles: true, cancelable: true });
      for (const [name, value] of [
        ["touches", active],
        ["targetTouches", active],
        ["changedTouches", changed],
      ] as const) {
        Object.defineProperty(event, name, { value });
      }
      root.dispatchEvent(event);
    },
    [type, points] as const,
  );
}

function displacement(before: Stats, after: Stats): number {
  return Math.hypot(after.x - before.x, after.z - before.z);
}

test("stick moves, look turns, tap breaks and place mode deposits", async ({
  browser,
}) => {
  const context = await browser.newContext({
    viewport: { width: 430, height: 900 },
    hasTouch: true,
    isMobile: true,
  });
  const page = await context.newPage();
  try {
    await boot(page, "/?quality=low&touch=1&screen=match");
    await startOffline(page, 0, 6);
    const before = await page.evaluate(
      () => (window as never as {
        __cbTest: { offlineStats(): Stats };
      }).__cbTest.offlineStats(),
    );

    await dispatchTouch(page, "touchstart", [
      { id: 1, x: 100, y: 400 },
      { id: 2, x: 330, y: 300 },
    ]);
    await page.waitForTimeout(120);
    const held = await touchState(page);
    expect(held.stickActive).toBe(true);
    expect(held.lookActive).toBe(true);

    await dispatchTouch(page, "touchmove", [
      { id: 1, x: 160, y: 350 },
      { id: 2, x: 380, y: 350 },
    ]);
    await page.waitForTimeout(900);
    const moved = await touchState(page);
    expect(moved.moveX).toBeGreaterThan(0);
    expect(moved.moveY).toBeGreaterThan(0);
    expect(moved.frame[3] >> 4).toBeGreaterThan(0);
    await dispatchTouch(page, "touchend", [
      { id: 1, x: 160, y: 350 },
      { id: 2, x: 380, y: 350 },
    ]);
    await page.waitForTimeout(300);

    const after = await page.evaluate(
      () => (window as never as {
        __cbTest: { offlineStats(): Stats };
      }).__cbTest.offlineStats(),
    );
    // The spawn is not at the origin, so distance-from-origin can shrink on a
    // valid move; displacement is the property the stick actually promises.
    expect(displacement(before, after)).toBeGreaterThan(0.05);

    const breaks = (await touchState(page)).breaks;
    await page.touchscreen.tap(330, 300);
    await page.waitForTimeout(150);
    expect((await touchState(page)).breaks).toBe(breaks + 1);

    await page.locator('.cb-touch-slot[data-slot="1"]').tap();
    await page.waitForTimeout(100);
    const placeMode = await touchState(page);
    expect(placeMode.mode).toBe("place");
    const places = placeMode.places;
    await page.touchscreen.tap(330, 300);
    await page.waitForTimeout(150);
    expect((await touchState(page)).places).toBe(places + 1);
  } finally {
    await context.close();
  }
});
