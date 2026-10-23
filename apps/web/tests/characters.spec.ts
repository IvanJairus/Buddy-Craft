/**
 * T-051 follow-up: the nine GLBs are actually parsed and drawn. The box model
 * stays as the fallback, so this spec also pins the instanced model path.
 */

import { readFileSync } from "node:fs";
import { join } from "node:path";
import * as THREE from "three";
import { expect, test } from "@playwright/test";
import {
  BUDDY_MODEL_FILES,
  PLAYER_MODEL_FILES,
  parseGlb,
  type CharacterModels,
} from "../src/render/characters";
import { EntityRenderer, FLAG_ALIVE, type EntityValue } from "../src/render/entities";
import type { EntityInfo } from "../src/render/entities";

const MODELS_DIR = join(process.cwd(), "public/assets/models");

function glb(name: string): THREE.BufferGeometry {
  const bytes = readFileSync(join(MODELS_DIR, name));
  const buffer = bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) as ArrayBuffer;
  return parseGlb(buffer);
}

function loadModels(): CharacterModels {
  return {
    players: PLAYER_MODEL_FILES.map(glb),
    buddies: BUDDY_MODEL_FILES.map(glb),
  };
}

function entity(index: number, overrides: Partial<EntityValue> = {}): EntityValue {
  return {
    id: index,
    team: 0,
    flags: FLAG_ALIVE,
    x: index,
    y: 40,
    z: 0,
    yaw: 0,
    pitch: 0,
    anim: 0,
    hp: 100,
    role: 0,
    ...overrides,
  };
}

test("every shipped GLB parses with positions, colours and fewer than 400 triangles", () => {
  for (const name of [...PLAYER_MODEL_FILES, ...BUDDY_MODEL_FILES]) {
    const geometry = glb(name);
    expect(geometry.getAttribute("position"), name).toBeDefined();
    expect(geometry.getAttribute("color"), name).toBeDefined();
    const position = geometry.getAttribute("position");
    const rig = geometry.getAttribute("rig");
    expect(rig, name).toBeDefined();
    expect(rig.count, name).toBe(position.count);
    const limbs = new Set<number>();
    for (let i = 0; i < rig.count; i += 1) {
      const id = rig.getX(i);
      expect(id, name).toBeGreaterThanOrEqual(0);
      expect(id, name).toBeLessThanOrEqual(5);
      limbs.add(id);
    }
    expect([...limbs].sort(), name).toEqual([0, 1, 2, 3, 4, 5]);
    const triangles =
      (geometry.getIndex()?.count ?? geometry.getAttribute("position").count) / 3;
    expect(triangles, name).toBeLessThan(400);
    expect(geometry.boundingSphere?.radius ?? 0, name).toBeGreaterThan(0);
  }
});

test("the model path draws one instanced mesh per used role", () => {
  const parent = new THREE.Group();
  const renderer = new EntityRenderer(parent, loadModels());
  const players = Array.from({ length: 6 }, (_, i) => entity(i));
  const buddies = Array.from({ length: 12 }, (_, i) =>
    entity(20 + i, { role: i % 6, team: i % 2 }),
  );

  const info: EntityInfo = renderer.updateFromValues(players, buddies, true);
  expect(info.players + info.buddies).toBe(18);
  expect(info.drawCalls).toBe(7);
  expect(info.drawCalls).toBeLessThanOrEqual(9);
  renderer.dispose();
  expect(parent.children.length).toBe(0);
});

interface TestWindow {
  __cbReady?: boolean;
  __cbTest?: {
    characters(): { loaded: boolean; players: number; buddies: number };
    entityBenchmark(frames?: number): (EntityInfo & { averageMs: number }) | null;
  };
}

test("a real match boots with all nine character meshes", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("console", (message) => {
    if (message.type() === "error" || message.text().includes("character models unavailable")) {
      errors.push(message.text());
    }
  });

  await page.goto("/?quality=low&screen=match");
  await page.waitForFunction(() => (window as unknown as TestWindow).__cbReady === true, null, {
    timeout: 30_000,
  });
  const characters = await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.characters(),
  );
  expect(characters).toEqual({ loaded: true, players: 3, buddies: 6 });

  const benchmark = await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.entityBenchmark(30),
  );
  expect(benchmark).not.toBeNull();
  expect(benchmark!.drawCalls).toBeLessThanOrEqual(9);
  expect(benchmark!.averageMs).toBeLessThan(3);
  expect(errors).toEqual([]);
});
