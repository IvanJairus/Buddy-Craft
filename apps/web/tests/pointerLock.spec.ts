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
    offlineStats(): { state: string };
    offlineGrant(player: number, crates: number): void;
    hudState(): { crates: number };
    viewModel(): { active: boolean; swings: number; frames: number };
    particles(): { drawCalls: number } | null;
    interactionVisuals(): {
      crackVisible: boolean;
      crackStage: number;
      breakable: boolean;
      placeable: boolean;
      ghost: [number, number, number] | null;
    } | null;
    blockAt(x: number, y: number, z: number): number;
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

test("terrain refuses mining and placing a crate spends one", async ({ page }) => {
  await page.goto("/?quality=low");
  await page.waitForFunction(() => (window as unknown as TestWindow).__cbReady === true, null, {
    timeout: 30_000,
  });
  await page.getByRole("button", { name: "CONTINUE" }).click();
  await page.getByRole("button", { name: "OFFLINE" }).click();
  await page.waitForFunction(
    () => (window as unknown as TestWindow).__cbTest!.offlineStats().state === "open",
    null,
    { timeout: 15_000 },
  );
  await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.offlineGrant(0, 6),
  );
  await page.waitForFunction(
    () => (window as unknown as TestWindow).__cbTest!.hudState().crates >= 6,
    null,
    { timeout: 5_000 },
  );

  let locked = false;
  for (let attempt = 0; attempt < 3; attempt += 1) {
    await page.locator("#game").click({ position: { x: 640, y: 360 } });
    await page.waitForTimeout(250);
    locked = await page.evaluate(
      () => (window as unknown as TestWindow).__cbTest!.input().pointerLocked,
    );
    if (locked) break;
    await page.bringToFront();
    await page.waitForTimeout(150);
  }
  expect(locked).toBe(true);

  // Look down at the ground so the crosshair targets a block within reach.
  await page.mouse.move(640, 470, { steps: 4 });
  await page.waitForTimeout(250);

  const before = await page.evaluate(
    () => (window as unknown as TestWindow).__cbTest!.viewModel().swings,
  );
  // Terrain is indestructible: a full hold must not swing or crack.
  await page.mouse.down({ button: "left" });
  await page.waitForTimeout(500);
  await page.mouse.up({ button: "left" });
  expect(
    await page.evaluate(() => (window as unknown as TestWindow).__cbTest!.viewModel().swings),
  ).toBe(before);

  let ghost: [number, number, number] | null = null;
  let placed = false;
  for (let attempt = 0; attempt < 3 && !placed; attempt += 1) {
    if (
      !(await page.evaluate(
        () => (window as unknown as TestWindow).__cbTest!.input().pointerLocked,
      ))
    ) {
      await page.bringToFront();
      await page.locator("#game").click({ position: { x: 640, y: 360 } });
      await page.waitForTimeout(250);
    }
    const sight = await page.evaluate(
      () => (window as unknown as TestWindow).__cbTest!.interactionVisuals(),
    );
    expect(sight?.breakable).toBe(false);
    if (sight?.placeable === true && sight.ghost !== null) {
      ghost = sight.ghost;
    }
    await page.mouse.down({ button: "right" });
    await page.waitForTimeout(150);
    await page.mouse.up({ button: "right" });
    await page.waitForTimeout(600);
    placed =
      (await page.evaluate(
        () => (window as unknown as TestWindow).__cbTest!.viewModel().swings,
      )) > before;
    if (!placed) {
      await page.mouse.move(640, 450 + attempt * 15, { steps: 3 });
      await page.waitForTimeout(150);
    }
  }
  expect(placed).toBe(true);
  expect(ghost).not.toBeNull();
  const placedBurst = await page.evaluate(
    () => (window as unknown as TestWindow).__cbTest!.particles(),
  );
  expect(placedBurst?.drawCalls).toBe(1);
  await page.waitForFunction(
    ([x, y, z]) =>
      (window as unknown as TestWindow).__cbTest!.blockAt(x, y, z) === 11,
    [Math.floor(ghost![0]), Math.floor(ghost![1]), Math.floor(ghost![2])],
    { timeout: 5_000 },
  );
  expect(
    await page.evaluate(() => (window as unknown as TestWindow).__cbTest!.hudState().crates),
  ).toBe(5);
  expect(
    await page.evaluate(() => (window as unknown as TestWindow).__cbTest!.viewModel().frames),
  ).toBeGreaterThan(0);
});
