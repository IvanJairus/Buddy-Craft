/**
 * T-042 page acceptance: a wire chunk diff changes the real wasm column, the
 * streamed mesh is rebuilt, and a later diff undoes it. The unit specs cover
 * ordering and version rules; this one proves the data reaches the renderer.
 */

import { expect, test, type Page } from "@playwright/test";
import type { ChunkInfo } from "../src/render/chunkManager";

interface TestWindow {
  __cbReady?: boolean;
  __cbTest?: {
    stream(x: number, z: number, budget?: number): ChunkInfo | null;
    chunks(): ChunkInfo | null;
    surface(x: number, z: number): number;
    applyChunkDiff(frame: number[]): {
      ok: boolean;
      applied: number;
      refreshed: number;
      chunks: ChunkInfo | null;
    };
  };
}

function varint(value: number): number[] {
  const out: number[] = [];
  let rest = value >>> 0;
  do {
    let byte = rest & 0x7f;
    rest >>>= 7;
    if (rest !== 0) byte |= 0x80;
    out.push(byte);
  } while (rest !== 0);
  return out;
}

function diffFrame(
  cx: number,
  cz: number,
  version: number,
  x: number,
  y: number,
  z: number,
  block: number,
): number[] {
  const index = x + 16 * (z + 16 * y);
  return [
    0x87,
    ...varint(cx * 2),
    ...varint(cz * 2),
    ...varint(version),
    0x00,
    ...varint(1),
    ...varint(index),
    block,
    ...varint(0),
  ];
}

test("a wire diff remeshes the real column and the next version undoes it", async ({
  page,
}: {
  page: Page;
}) => {
  await page.goto("/?screen=match");
  await page.waitForFunction(() => (window as unknown as TestWindow).__cbReady === true, null, {
    timeout: 30_000,
  });
  await page.evaluate(() => (window as unknown as TestWindow).__cbTest!.stream(136, 136, 1024));

  const before = await page.evaluate(() => (window as unknown as TestWindow).__cbTest!.surface(136, 136));
  expect(before).toBeGreaterThan(0);
  expect(before).toBeLessThan(96);

  const place = diffFrame(8, 8, 1, 8, before, 8, 11);
  const placed = await page.evaluate(
    (frame) => (window as unknown as TestWindow).__cbTest!.applyChunkDiff(frame),
    place,
  );
  expect(placed.ok).toBe(true);
  expect(placed.applied).toBe(1);
  expect(placed.refreshed).toBe(5);
  expect(placed.chunks?.loaded).toBe(169);
  expect(placed.chunks?.drawCalls).toBe(169);
  const surfaceAfter = await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.surface(136, 136),
  );
  expect(surfaceAfter).toBe(before + 1);

  const undo = diffFrame(8, 8, 2, 8, before, 8, 0);
  const removed = await page.evaluate(
    (frame) => (window as unknown as TestWindow).__cbTest!.applyChunkDiff(frame),
    undo,
  );
  expect(removed.ok).toBe(true);
  expect(removed.applied).toBe(1);
  const surfaceBack = await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.surface(136, 136),
  );
  expect(surfaceBack).toBe(before);
});
