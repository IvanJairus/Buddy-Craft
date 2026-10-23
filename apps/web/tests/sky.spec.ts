/**
 * World-art smoke test: with the camera above the ground and pitched up, the
 * sky dome and the instanced cloud banks must both reach the frame. The flat
 * pre-art sky had no white cloud pixels and no height gradient.
 */

import { expect, test, type Page } from "@playwright/test";

interface TestWindow {
  __cbReady?: boolean;
  __cbTest?: {
    cameraPose(x: number, y: number, z: number, pitch: number): void;
    pixel(x: number, y: number): [number, number, number, number];
  };
}

test("looking up shows a blue gradient sky and white cloud banks", async ({ page }: { page: Page }) => {
  await page.goto("/?quality=high&screen=match");
  await page.waitForFunction(() => (window as unknown as TestWindow).__cbReady === true, null, {
    timeout: 30_000,
  });
  // Column (128, 128) has a surface of y = 54, so the camera must stay above it.
  await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.cameraPose(128, 70, 128, 0.4),
  );
  await page.waitForTimeout(200);

  const pixel = (x: number, y: number): Promise<[number, number, number, number]> =>
    page.evaluate(
      ([px, py]) => (window as unknown as TestWindow).__cbTest!.pixel(px, py),
      [x, y],
    );

  for (const [x, y] of [
    [100, 420],
    [650, 512],
    [1200, 420],
  ]) {
    const [red, green, blue] = await pixel(x, y);
    expect(red, `cloud at ${x},${y}`).toBeGreaterThan(200);
    expect(green, `cloud at ${x},${y}`).toBeGreaterThan(200);
    expect(blue, `cloud at ${x},${y}`).toBeGreaterThan(200);
  }

  const [topRed, topGreen, topBlue] = await pixel(640, 20);
  const [lowRed, lowGreen, lowBlue] = await pixel(640, 180);
  expect(topBlue).toBeGreaterThan(topRed + 15);
  expect(topGreen).toBeGreaterThan(topRed);
  expect(lowBlue).toBeGreaterThan(lowRed + 15);
  expect(lowRed, "the sky must pale toward the horizon").toBeGreaterThan(topRed + 4);
  expect(lowGreen).toBeGreaterThan(topGreen);
});
