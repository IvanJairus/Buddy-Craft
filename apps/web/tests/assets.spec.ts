/**
 * T-051 acceptance specs: the shipped atlas is 256x256 with 24 authored
 * 16x16 slots under the 30 KB blocking budget, and a match boots with the
 * atlas loaded and the terrain shader compiled.
 */

import { expect, test, type Page } from "@playwright/test";
import { inflateSync } from "node:zlib";

const ATLAS_URL = "/assets/atlas.png";
const ATLAS_BYTES_MAX = 30 * 1024;
const TILE = 16;
const AUTHORED_TILES = 24;

interface DecodedPng {
  width: number;
  height: number;
  rgba: Uint8Array;
}

interface TestWindow {
  __cbReady?: boolean;
  __cbTest?: {
    chunks(): { loaded: number; atlasReady: boolean } | null;
    pixel(x: number, y: number): [number, number, number, number];
  };
}

function decodePng(bytes: Buffer): DecodedPng {
  expect(bytes.subarray(0, 8).toString("hex")).toBe("89504e470d0a1a0a");
  let offset = 8;
  let width = 0;
  let height = 0;
  const parts: Buffer[] = [];
  while (offset < bytes.length) {
    const length = bytes.readUInt32BE(offset);
    const type = bytes.toString("ascii", offset + 4, offset + 8);
    const data = bytes.subarray(offset + 8, offset + 8 + length);
    if (type === "IHDR") {
      width = data.readUInt32BE(0);
      height = data.readUInt32BE(4);
      expect(data[8]).toBe(8);
      expect(data[9]).toBe(6);
    } else if (type === "IDAT") {
      parts.push(data);
    }
    offset += 12 + length;
    if (type === "IEND") break;
  }
  const raw = inflateSync(Buffer.concat(parts));
  const stride = width * 4;
  const rgba = new Uint8Array(width * height * 4);
  for (let y = 0; y < height; y += 1) {
    const row = y * (stride + 1);
    expect(raw[row], "the generator writes unfiltered scanlines").toBe(0);
    rgba.set(raw.subarray(row + 1, row + 1 + stride), y * stride);
  }
  return { width, height, rgba };
}

function tileHasAlpha(atlas: DecodedPng, tile: number): boolean {
  const tx = (tile % 16) * TILE;
  const ty = Math.floor(tile / 16) * TILE;
  for (let y = 0; y < TILE; y += 1) {
    for (let x = 0; x < TILE; x += 1) {
      if (atlas.rgba[((ty + y) * atlas.width + tx + x) * 4 + 3] > 0) return true;
    }
  }
  return false;
}

async function boot(page: Page): Promise<void> {
  await page.goto("/?quality=high&screen=match");
  await page.waitForFunction(
    () =>
      (window as unknown as TestWindow).__cbReady === true &&
      (window as unknown as TestWindow).__cbTest?.chunks()?.loaded === 169,
    null,
    { timeout: 30_000 },
  );
  await page.waitForFunction(
    () => (window as unknown as TestWindow).__cbTest?.chunks()?.atlasReady === true,
    null,
    { timeout: 15_000 },
  );
  await page.waitForTimeout(200);
}

test("the atlas is 256x256, 24 slots and under 30 KB", async ({ request }) => {
  const response = await request.get(ATLAS_URL);
  expect(response.status()).toBe(200);
  expect(response.headers()["content-type"]).toContain("image/png");
  const bytes = await response.body();
  expect(bytes.length).toBeLessThanOrEqual(ATLAS_BYTES_MAX);

  const atlas = decodePng(bytes);
  expect(atlas.width).toBe(256);
  expect(atlas.height).toBe(256);

  // Slot 0 is air and stays transparent so the fragment shader discards it.
  expect(tileHasAlpha(atlas, 0)).toBe(false);
  for (let tile = 1; tile < AUTHORED_TILES; tile += 1) {
    expect(tileHasAlpha(atlas, tile), `tile ${tile} is empty`).toBe(true);
  }
  for (let tile = AUTHORED_TILES; tile < 256; tile += 1) {
    expect(tileHasAlpha(atlas, tile), `tile ${tile} should be spare`).toBe(false);
  }
});

test("a match loads the atlas and draws textured terrain", async ({ page }) => {
  const errors: string[] = [];
  const atlasStatus: number[] = [];
  page.on("console", (message) => {
    if (message.type() === "error") errors.push(message.text());
  });
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("response", (response) => {
    if (response.url().includes(ATLAS_URL)) atlasStatus.push(response.status());
  });

  await boot(page);
  await expect.poll(() => atlasStatus.length).toBeGreaterThan(0);
  expect(atlasStatus.every((status) => status === 200)).toBe(true);
  expect(errors.filter((text) => /shader|webglprogram|atlas\.png/i.test(text))).toEqual([]);

  // A wrong flip or an all-transparent sample discards every terrain fragment;
  // the pixel then falls back to the blue clear colour.
  for (const [x, y] of [
    [640, 400],
    [200, 650],
  ]) {
    const [red, green, blue, alpha] = await page.evaluate(
      ([px, py]) => (window as unknown as TestWindow).__cbTest!.pixel(px, py),
      [x, y],
    );
    expect(alpha).toBe(255);
    expect(green, `terrain pixel at ${x},${y}`).toBeGreaterThan(red);
    expect(green, `terrain pixel at ${x},${y}`).toBeGreaterThan(blue);
  }
});
