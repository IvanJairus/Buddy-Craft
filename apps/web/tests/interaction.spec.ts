/**
 * T-034 acceptance specs: the 20-case DDA table, the through-wall negative
 * case, the highlight/ghost preview and the pending-edit lifecycle.
 *
 * The raycast and the visual objects run in Node. One page test casts at a real
 * chunk column, so the height-map `VoxelSource` is exercised against terrain
 * that was actually meshed, not only against a hand-built set of cells.
 */

import * as THREE from "three";
import { expect, test, type Page } from "@playwright/test";
import {
  EDIT_BREAK,
  EDIT_PLACE,
  Interaction,
  raycastVoxels,
  type RayHit,
  type Vec3Like,
  type VisualState,
  type VoxelSource,
} from "../src/game/interaction";

interface TestWindow {
  __cbReady?: boolean;
  __cbTest?: {
    chunks(): { loaded: number } | null;
    surface(x: number, z: number): number;
    aim(
      origin: [number, number, number],
      direction: [number, number, number],
    ): { hit: RayHit | null; visuals: VisualState | null };
    placeBlock(block: number): number;
    breakBlock(): number;
    pendingEdits(): Array<{ seq: number; x: number; y: number; z: number; block: number; action: number }>;
    ackEdit(seq: number, accepted: boolean): boolean;
  };
}

function world(cells: Array<[number, number, number]>): VoxelSource {
  const set = new Set(cells.map(([x, y, z]) => `${x},${y},${z}`));
  return { solid: (x, y, z) => set.has(`${x},${y},${z}`) };
}

function wallX(x: number, yMax = 2, zMax = 2): VoxelSource {
  const cells: Array<[number, number, number]> = [];
  for (let y = 0; y <= yMax; y += 1) {
    for (let z = 0; z <= zMax; z += 1) {
      cells.push([x, y, z]);
    }
  }
  return world(cells);
}

function wallZ(z: number, yMax = 2, xMax = 2): VoxelSource {
  const cells: Array<[number, number, number]> = [];
  for (let y = 0; y <= yMax; y += 1) {
    for (let x = 0; x <= xMax; x += 1) {
      cells.push([x, y, z]);
    }
  }
  return world(cells);
}

function floor(y: number): VoxelSource {
  const cells: Array<[number, number, number]> = [];
  for (let x = -2; x <= 6; x += 1) {
    for (let z = -2; z <= 6; z += 1) {
      cells.push([x, y, z]);
    }
  }
  return world(cells);
}

const origin = (x: number, y: number, z: number): Vec3Like => ({ x, y, z });
const vector = (x: number, y: number, z: number): Vec3Like => ({ x, y, z });

interface Case {
  name: string;
  from: Vec3Like;
  direction: Vec3Like;
  source: VoxelSource;
  expected: null | {
    cell: [number, number, number];
    normal: [number, number, number];
    place: [number, number, number];
    distance: number;
  };
}

const CASES: Case[] = [
  {
    name: "east wall",
    from: origin(0.5, 0.5, 0.5),
    direction: vector(1, 0, 0),
    source: wallX(3),
    expected: { cell: [3, 0, 0], normal: [-1, 0, 0], place: [2, 0, 0], distance: 2.5 },
  },
  {
    name: "west wall",
    from: origin(5.5, 0.5, 0.5),
    direction: vector(-1, 0, 0),
    source: wallX(3),
    expected: { cell: [3, 0, 0], normal: [1, 0, 0], place: [4, 0, 0], distance: 1.5 },
  },
  {
    name: "floor",
    from: origin(0.5, 2.5, 0.5),
    direction: vector(0, -1, 0),
    source: floor(0),
    expected: { cell: [0, 0, 0], normal: [0, 1, 0], place: [0, 1, 0], distance: 1.5 },
  },
  {
    name: "ceiling",
    from: origin(0.5, 2.5, 0.5),
    direction: vector(0, 1, 0),
    source: world([
      [0, 4, 0],
      [1, 4, 0],
      [0, 4, 1],
    ]),
    expected: { cell: [0, 4, 0], normal: [0, -1, 0], place: [0, 3, 0], distance: 1.5 },
  },
  {
    name: "south wall",
    from: origin(0.5, 0.5, 0.5),
    direction: vector(0, 0, 1),
    source: wallZ(3),
    expected: { cell: [0, 0, 3], normal: [0, 0, -1], place: [0, 0, 2], distance: 2.5 },
  },
  {
    name: "north wall",
    from: origin(0.5, 0.5, 5.5),
    direction: vector(0, 0, -1),
    source: wallZ(3),
    expected: { cell: [0, 0, 3], normal: [0, 0, 1], place: [0, 0, 4], distance: 1.5 },
  },
  {
    name: "diagonal east-south",
    from: origin(0.5, 0.5, 0.5),
    direction: vector(1, 0, 0.5),
    source: wallX(3, 2, 5),
    expected: { cell: [3, 0, 1], normal: [-1, 0, 0], place: [2, 0, 1], distance: 2.79508497 },
  },
  {
    name: "diagonal west-north",
    from: origin(5.5, 0.5, 5.5),
    direction: vector(-1, 0, -0.5),
    source: wallX(3, 2, 5),
    expected: { cell: [3, 0, 4], normal: [1, 0, 0], place: [4, 0, 4], distance: 1.67705098 },
  },
  {
    name: "unnormalised direction",
    from: origin(0.5, 0.5, 0.5),
    direction: vector(2, 0, 0),
    source: wallX(3),
    expected: { cell: [3, 0, 0], normal: [-1, 0, 0], place: [2, 0, 0], distance: 2.5 },
  },
  {
    name: "beyond reach",
    from: origin(0.5, 0.5, 0.5),
    direction: vector(1, 0, 0),
    source: wallX(7),
    expected: null,
  },
  {
    name: "at five point five metres",
    from: origin(0.5, 0.5, 0.5),
    direction: vector(1, 0, 0),
    source: wallX(6),
    expected: { cell: [6, 0, 0], normal: [-1, 0, 0], place: [5, 0, 0], distance: 5.5 },
  },
  {
    name: "first of two walls",
    from: origin(0.5, 0.5, 0.5),
    direction: vector(1, 0, 0),
    source: world([
      [2, 0, 0],
      [4, 0, 0],
    ]),
    expected: { cell: [2, 0, 0], normal: [-1, 0, 0], place: [1, 0, 0], distance: 1.5 },
  },
  {
    name: "empty world",
    from: origin(0.5, 0.5, 0.5),
    direction: vector(1, 0, 0),
    source: world([]),
    expected: null,
  },
  {
    name: "origin inside a block",
    from: origin(0.5, 0.5, 0.5),
    direction: vector(0, 1, 0),
    source: floor(0),
    expected: null,
  },
  {
    name: "half a metre above the floor",
    from: origin(0.5, 1.5, 0.5),
    direction: vector(0, -1, 0),
    source: floor(0),
    expected: { cell: [0, 0, 0], normal: [0, 1, 0], place: [0, 1, 0], distance: 0.5 },
  },
  {
    name: "wall behind the ray",
    from: origin(0.5, 0.5, 0.5),
    direction: vector(-1, 0, 0),
    source: wallX(3),
    expected: null,
  },
  {
    name: "offset origin",
    from: origin(0.9, 0.5, 0.5),
    direction: vector(1, 0, 0),
    source: wallX(3),
    expected: { cell: [3, 0, 0], normal: [-1, 0, 0], place: [2, 0, 0], distance: 2.1 },
  },
  {
    name: "ray misses a one-block-high wall",
    from: origin(0.5, 1.5, 0.5),
    direction: vector(1, 0, 0),
    source: world([
      [3, 0, 0],
      [3, 0, 1],
      [3, 0, 2],
    ]),
    expected: null,
  },
  {
    name: "zero direction",
    from: origin(0.5, 0.5, 0.5),
    direction: vector(0, 0, 0),
    source: wallX(3),
    expected: null,
  },
  {
    name: "descending diagonal",
    from: origin(0.5, 3.5, 0.5),
    direction: vector(1, -2, 0),
    source: floor(0),
    expected: { cell: [1, 0, 0], normal: [0, 1, 0], place: [1, 1, 0], distance: 2.79508497 },
  },
];

test("raycast_accuracy: 20 known cases", () => {
  expect(CASES).toHaveLength(20);
  for (const entry of CASES) {
    const hit = raycastVoxels(entry.from, entry.direction, 6, entry.source);
    if (entry.expected === null) {
      expect(hit, entry.name).toBeNull();
      continue;
    }
    expect(hit, entry.name).not.toBeNull();
    expect([hit!.cell.x, hit!.cell.y, hit!.cell.z], entry.name).toEqual(entry.expected.cell);
    expect([hit!.normal.x, hit!.normal.y, hit!.normal.z], entry.name).toEqual(
      entry.expected.normal,
    );
    expect([hit!.place.x, hit!.place.y, hit!.place.z], entry.name).toEqual(
      entry.expected.place,
    );
    expect(hit!.distance, entry.name).toBeCloseTo(entry.expected.distance, 3);
  }
});

test("cannot_target_through_wall", () => {
  const cells: Array<[number, number, number]> = [];
  for (let y = 0; y <= 4; y += 1) {
    for (let z = 0; z <= 4; z += 1) {
      cells.push([2, y, z]);
    }
  }
  cells.push([5, 0, 0]);
  const source = world(cells);
  const hit = raycastVoxels(origin(0.5, 0.5, 0.5), vector(1, 0, 0), 6, source);
  expect(hit).not.toBeNull();
  expect([hit!.cell.x, hit!.cell.y, hit!.cell.z]).toEqual([2, 0, 0]);
  expect(hit!.cell.x).not.toBe(5);
});

test("highlight and ghost follow the hit and edit lifecycle", () => {
  const scene = new THREE.Scene();
  const calls: Array<{ x: number; y: number; z: number; block: number; action: number }> = [];
  let nextSeq = 1;
  const edit = {
    localEdit(x: number, y: number, z: number, block: number, action: number): number {
      calls.push({ x, y, z, block, action });
      return nextSeq++;
    },
  };
  const interaction = new Interaction(scene, wallX(2), edit);
  const hit = interaction.aim(origin(0.5, 0.5, 0.5), vector(1, 0, 0));

  expect(hit).not.toBeNull();
  expect([hit!.cell.x, hit!.cell.y, hit!.cell.z]).toEqual([2, 0, 0]);
  expect([hit!.normal.x, hit!.normal.y, hit!.normal.z]).toEqual([-1, 0, 0]);
  const visuals = interaction.visuals();
  expect(visuals.highlightVisible).toBe(true);
  expect(visuals.ghostVisible).toBe(true);
  expect(visuals.highlight).toEqual([2.5, 0.5, 0.5]);
  expect(visuals.ghost).toEqual([1.5, 0.5, 0.5]);

  const placed = interaction.place(3);
  expect(placed).toBe(1);
  expect(calls[0]).toEqual({ x: 1, y: 0, z: 0, block: 3, action: EDIT_PLACE });
  expect(interaction.pending()).toHaveLength(1);

  const broken = interaction.breakBlock();
  expect(broken).toBe(2);
  expect(calls[1]).toEqual({ x: 2, y: 0, z: 0, block: 0, action: EDIT_BREAK });
  expect(interaction.pending()).toHaveLength(2);

  expect(interaction.ack(placed, true)).toBe(true);
  expect(interaction.pending()).toHaveLength(1);
  expect(interaction.ack(broken, false)).toBe(true);
  expect(interaction.pending()).toHaveLength(0);
  expect(interaction.ack(999, true)).toBe(false);

  expect(interaction.aim(origin(0.5, 0.5, 0.5), vector(0, 1, 0))).toBeNull();
  expect(interaction.visuals().highlightVisible).toBe(false);
  expect(interaction.visuals().ghostVisible).toBe(false);

  interaction.dispose();
  expect(scene.children).toHaveLength(0);
});

async function boot(page: Page): Promise<void> {
  await page.goto("/?quality=high");
  await page.waitForFunction(() => (window as unknown as TestWindow).__cbReady === true, null, {
    timeout: 30_000,
  });
  await page.waitForFunction(
    () => (window as unknown as TestWindow).__cbTest?.chunks()?.loaded === 169,
    null,
    { timeout: 30_000 },
  );
}

test("aims at a real terrain column and records a local edit", async ({ page }) => {
  await boot(page);
  const surface = await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.surface(128, 128),
  );
  expect(surface).toBeGreaterThan(0);

  const aimed = await page.evaluate(
    (height) =>
      (window as unknown as TestWindow).__cbTest!.aim(
        [128.5, height + 2, 128.5],
        [0, -1, 0],
      ),
    surface,
  );
  expect(aimed.hit).not.toBeNull();
  expect(aimed.hit!.cell).toEqual({ x: 128, y: surface - 1, z: 128 });
  expect(aimed.hit!.normal).toEqual({ x: 0, y: 1, z: 0 });
  expect(aimed.hit!.place).toEqual({ x: 128, y: surface, z: 128 });
  expect(aimed.visuals?.highlightVisible).toBe(true);
  expect(aimed.visuals?.ghostVisible).toBe(true);
  expect(aimed.visuals?.highlight).toEqual([128.5, surface - 0.5, 128.5]);
  expect(aimed.visuals?.ghost).toEqual([128.5, surface + 0.5, 128.5]);

  const placed = await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.placeBlock(3),
  );
  expect(placed).toBeGreaterThan(0);
  let pending = await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.pendingEdits(),
  );
  expect(pending).toHaveLength(1);
  expect(pending[0]).toMatchObject({
    x: 128,
    y: surface,
    z: 128,
    block: 3,
    action: EDIT_PLACE,
  });

  const broken = await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.breakBlock(),
  );
  expect(broken).toBeGreaterThan(0);
  pending = await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.pendingEdits(),
  );
  expect(pending).toHaveLength(2);

  expect(
    await page.evaluate(
      (seq) => (window as unknown as TestWindow).__cbTest!.ackEdit(seq, true),
      placed,
    ),
  ).toBe(true);
  await page.evaluate(
    (seq) => (window as unknown as TestWindow).__cbTest!.ackEdit(seq, false),
    broken,
  );
  expect(
    await page.evaluate(() =>
      (window as unknown as TestWindow).__cbTest!.pendingEdits(),
    ),
  ).toHaveLength(0);
});
