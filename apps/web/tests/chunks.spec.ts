/**
 * T-032 acceptance specs: column streaming, the 6/7 hysteresis, mesh handoff,
 * draw-call budget, `memory.grow` recovery and the no-view-leak soak.
 *
 * The streaming test is a pure Node test with a fake mesh source; it is about
 * set membership and disposal, not WebGL. The page tests use the real wasm
 * mesher, so the `.slice()` ownership path and the detach guard are exercised
 * with the artifact the client ships.
 */

import * as THREE from "three";
import { expect, test, type Page } from "@playwright/test";
import { ChunkManager } from "../src/render/chunkManager";
import type { ChunkInfo } from "../src/render/chunkManager";
import type { RendererInfo } from "../src/render/renderer";

interface TestWindow {
  __cbReady?: boolean;
  __cbTest?: {
    info(): RendererInfo;
    chunks(): ChunkInfo | null;
    stream(x: number, z: number, budget?: number): ChunkInfo | null;
    growWasm(pages: number): { previousPages: number; bytes: number; viewsAlive: boolean };
    leakCycle(cycles: number): void;
    wasm(): { viewsAlive: boolean; memoryBytes: number };
  };
}

function quadPayload(): Uint8Array {
  const vertices = new Uint8Array(48);
  const corners = [
    [0, 0, 0],
    [16, 0, 0],
    [16, 0, 16],
    [0, 0, 16],
  ];
  for (let i = 0; i < corners.length; i += 1) {
    vertices.set(corners[i], i * 12);
  }
  const out = new Uint8Array(12 + 48 + 24 + 256);
  const view = new DataView(out.buffer);
  view.setUint32(0, 48, true);
  view.setUint32(4, 24, true);
  view.setUint32(8, 256, true);
  out.set(vertices, 12);
  [0, 1, 2, 0, 2, 3].forEach((index, i) => view.setUint32(12 + 48 + i * 4, index, true));
  return out;
}

async function boot(page: Page, url: string): Promise<void> {
  await page.goto(url);
  await page.waitForFunction(() => (window as unknown as TestWindow).__cbReady === true, null, {
    timeout: 30_000,
  });
}

async function info(page: Page): Promise<RendererInfo> {
  return page.evaluate(() => (window as unknown as TestWindow).__cbTest!.info());
}

async function chunks(page: Page): Promise<ChunkInfo | null> {
  return page.evaluate(() => (window as unknown as TestWindow).__cbTest!.chunks());
}

async function waitForChunks(page: Page, loaded: number): Promise<void> {
  await page.waitForFunction(
    (count) => (window as unknown as TestWindow).__cbTest?.chunks()?.loaded === count,
    loaded,
    { timeout: 30_000 },
  );
}

test("streams radius 6 and keeps radius 7 before unloading", () => {
  const parent = new THREE.Group();
  const manager = new ChunkManager(
    parent,
    { meshColumn: () => quadPayload() },
    { loadRadius: 2, keepRadius: 3 },
  );

  manager.update(8 * 16 + 8, 8 * 16 + 8, 1000);
  expect(manager.info().loaded).toBe(25);
  expect(manager.info().drawCalls).toBe(25);
  expect(manager.hasColumn(10, 8)).toBe(true);
  expect(manager.hasColumn(11, 8)).toBe(false);

  // Move two columns east. The old column at distance 3 is kept by the
  // hysteresis ring; the old one at distance 4 is unloaded; the new columns at
  // distance 2 are loaded.
  manager.update(10 * 16 + 8, 8 * 16 + 8, 1000);
  expect(manager.info().loaded).toBe(30);
  expect(manager.hasColumn(7, 8)).toBe(true);
  expect(manager.hasColumn(6, 8)).toBe(false);
  expect(manager.hasColumn(12, 8)).toBe(true);
  expect(manager.hasColumn(13, 8)).toBe(false);

  manager.dispose();
  expect(manager.info().drawCalls).toBe(0);
  expect(parent.children.length).toBe(0);
});

test("the default policy loads 6 and keeps 7", () => {
  const parent = new THREE.Group();
  const manager = new ChunkManager(parent, { meshColumn: () => quadPayload() });

  manager.update(8 * 16 + 8, 8 * 16 + 8, 1000);
  expect(manager.info().loaded).toBe(169);
  expect(manager.hasColumn(14, 8)).toBe(true);
  expect(manager.hasColumn(15, 8)).toBe(false);

  // From (10, 10), column (2, 8) is distance 8 and must go; (3, 8) is distance
  // 7 and is kept by the ring even though it is not in the load set.
  manager.update(10 * 16 + 8, 10 * 16 + 8, 1000);
  expect(manager.hasColumn(2, 8)).toBe(false);
  expect(manager.hasColumn(3, 8)).toBe(true);
  expect(manager.hasColumn(15, 15)).toBe(true);
  manager.dispose();
});

test("one mesh per call unless the caller asks for more", () => {
  const parent = new THREE.Group();
  const manager = new ChunkManager(parent, { meshColumn: () => quadPayload() });
  manager.update(8 * 16 + 8, 8 * 16 + 8, 1);
  expect(manager.info().loaded).toBe(1);
  expect(manager.info().pending).toBe(168);
  manager.update(8 * 16 + 8, 8 * 16 + 8, 1000);
  expect(manager.info().loaded).toBe(169);
  expect(manager.info().pending).toBe(0);
  manager.dispose();
});

test("169 columns stay under the 120 draw-call gate", async ({ page }) => {
  await boot(page, "/?quality=high");
  await waitForChunks(page, 169);
  await page.waitForTimeout(100);

  const state = await info(page);
  const chunkInfo = await chunks(page);
  expect(chunkInfo?.meshed).toBeGreaterThanOrEqual(169);
  expect(state.drawCalls).toBeLessThanOrEqual(120);
  expect(state.triangles).toBeGreaterThan(0);
});

test("geometry survives a forced 64 MB memory.grow", async ({ page }) => {
  await boot(page, "/?quality=high");
  await waitForChunks(page, 169);
  const before = await chunks(page);
  const beforeMemory = await page.evaluate(
    () => (window as unknown as TestWindow).__cbTest!.wasm().memoryBytes,
  );
  expect(before).not.toBeNull();

  const grown = await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.growWasm(1024),
  );
  expect(grown.bytes - beforeMemory).toBeGreaterThanOrEqual(64 * 1024 * 1024);
  expect(grown.viewsAlive).toBe(true);

  await page.waitForTimeout(200);
  const after = await info(page);
  expect(after.drawCalls).toBeGreaterThan(0);

  // Meshing after the grow must still work: this allocates from the wasm arena
  // and rebuilds the views the loader holds.
  const streamed = await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.stream(10 * 16 + 8, 10 * 16 + 8, 64),
  );
  expect(streamed?.meshed).toBeGreaterThan(before!.meshed);
  expect(
    await page.evaluate(() => (window as unknown as TestWindow).__cbTest!.wasm().viewsAlive),
  ).toBe(true);
});

test("200 load/unload cycles leak under 5 MB", async ({ page }) => {
  await boot(page, "/?quality=high");
  await waitForChunks(page, 169);

  const session = await page.context().newCDPSession(page);
  await session.send("HeapProfiler.collectGarbage");
  const start = (await session.send("Runtime.getHeapUsage")) as { usedSize: number };

  await page.evaluate(() => (window as unknown as TestWindow).__cbTest!.leakCycle(200));

  await session.send("HeapProfiler.collectGarbage");
  const end = (await session.send("Runtime.getHeapUsage")) as { usedSize: number };
  expect(end.usedSize - start.usedSize).toBeLessThan(5 * 1024 * 1024);
  expect(
    await page.evaluate(() => (window as unknown as TestWindow).__cbTest!.wasm().viewsAlive),
  ).toBe(true);
});
