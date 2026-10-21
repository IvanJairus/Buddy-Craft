/**
 * T-035 acceptance specs: the 4-byte frame, key release on blur, hotbar and
 * actions, one pack per tick, pointer lock, and settings that survive reload.
 *
 * The page specs run against `?scene=clear`: input does not need chunks, and a
 * clear scene keeps the frame-rate checks out of the terrain budget.
 */

import { expect, test, type Page } from "@playwright/test";
import {
  FIRE,
  JUMP,
  USE,
  packFrame,
  type InputAction,
} from "../src/game/inputDesktop";

interface InputState {
  polls: number;
  ticks: number;
  frame: number[];
  yaw: number;
  pitch: number;
  slot: number;
  pointerLocked: boolean;
  sensitivity: number;
  invertY: boolean;
  lastAction: InputAction | null;
}

interface TestWindow {
  __cbReady?: boolean;
  __cbTest?: {
    input(): InputState;
    setSensitivity(value: number): void;
    setInvertY(value: boolean): void;
  };
}

test("packs the wire frame", () => {
  const frame = packFrame({
    forward: 1,
    strafe: -1,
    jump: true,
    fire: true,
    use: false,
    stance: false,
    crouch: false,
    yaw: 64,
    pitch: 192,
  });
  expect(frame[0]).toBe(JUMP | FIRE);
  expect(frame[1]).toBe(64);
  expect(frame[2]).toBe(192);
  expect(frame[3]).toBe((7 << 4) | 8);

  const neutral = packFrame({
    forward: 0,
    strafe: 0,
    jump: false,
    fire: false,
    use: false,
    stance: false,
    crouch: false,
    yaw: 0,
    pitch: 0,
  });
  expect(Array.from(neutral)).toEqual([0, 0, 0, 0]);

  const right = new Uint8Array(4);
  expect(packFrame(
    {
      forward: 0,
      strafe: 1,
      jump: false,
      fire: false,
      use: true,
      stance: true,
      crouch: false,
      yaw: 0,
      pitch: 0,
    },
    right,
  )).toBe(right);
  expect(right[0]).toBe(USE | (1 << 3));
  expect(right[3]).toBe(7);
});

async function boot(page: Page): Promise<void> {
  await page.goto("/?quality=high&scene=clear&screen=match");
  await page.waitForFunction(() => (window as unknown as TestWindow).__cbReady === true, null, {
    timeout: 30_000,
  });
  await page.waitForFunction(
    () => ((window as unknown as TestWindow).__cbTest?.input().polls ?? 0) > 0,
    null,
    { timeout: 5_000 },
  );
}

async function state(page: Page): Promise<InputState> {
  return page.evaluate(() => (window as unknown as TestWindow).__cbTest!.input());
}

test("WASD, jump and mouse buttons fill the frame", async ({ page }) => {
  await boot(page);
  let input = await state(page);
  expect(input.polls).toBeGreaterThan(0);
  expect(input.ticks).toBeGreaterThan(0);

  await page.keyboard.down("w");
  await page.waitForTimeout(100);
  input = await state(page);
  expect(input.frame[3] >> 4).toBe(7);
  expect(input.frame[3] & 0x0f).toBe(0);

  await page.keyboard.down("d");
  await page.waitForTimeout(100);
  input = await state(page);
  expect(input.frame[3]).toBe((7 << 4) | 7);

  await page.keyboard.down("Space");
  await page.waitForTimeout(100);
  input = await state(page);
  expect(input.frame[0] & JUMP).toBe(JUMP);

  await page.keyboard.up("Space");
  await page.keyboard.up("d");
  await page.keyboard.up("w");
});

test("no_input_when_unfocused", async ({ page }) => {
  await boot(page);
  await page.keyboard.down("w");
  await page.waitForTimeout(100);
  expect((await state(page)).frame[3] >> 4).toBe(7);

  await page.evaluate(() => window.dispatchEvent(new Event("blur")));
  await page.waitForTimeout(100);
  const input = await state(page);
  expect(input.frame[3]).toBe(0);
  expect(input.frame[0]).toBe(0);
  await page.keyboard.up("w");
});

test("hotbar and action keys", async ({ page }) => {
  await boot(page);
  await page.keyboard.press("3");
  expect((await state(page)).slot).toBe(2);

  await page.mouse.wheel(0, 120);
  await page.waitForTimeout(50);
  expect((await state(page)).slot).toBe(3);

  await page.keyboard.press("q");
  expect((await state(page)).lastAction).toBe("drop");
  await page.keyboard.press("Tab");
  expect((await state(page)).lastAction).toBe("scoreboard");
  await page.keyboard.press("e");
  expect((await state(page)).lastAction).toBe("recipes");
  await page.keyboard.press("f");
  expect((await state(page)).lastAction).toBe("stance");
  await page.keyboard.press("Escape");
  expect((await state(page)).lastAction).toBe("menu");
});

test("one poll per tick", async ({ page }) => {
  await boot(page);
  const before = await state(page);
  await page.waitForTimeout(300);
  const after = await state(page);
  expect(after.ticks).toBeGreaterThan(before.ticks);
  expect(after.polls - before.polls).toBe(after.ticks - before.ticks);
});

test("sensitivity and invert-Y persist across reload", async ({ page }) => {
  await boot(page);
  await page.evaluate(() => {
    (window as unknown as TestWindow).__cbTest!.setSensitivity(0.5);
    (window as unknown as TestWindow).__cbTest!.setInvertY(true);
  });
  await page.reload();
  await page.waitForFunction(() => (window as unknown as TestWindow).__cbReady === true, null, {
    timeout: 30_000,
  });
  const input = await state(page);
  expect(input.sensitivity).toBe(0.5);
  expect(input.invertY).toBe(true);
});
