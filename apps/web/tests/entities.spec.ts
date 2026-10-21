/**
 * T-033 acceptance specs: instanced entities, the animation contract, the CPU
 * budget and the GPU-lifetime particle pool.
 *
 * The entity and particle tests run in Node with a plain `THREE.Group`; no
 * WebGL context is needed to prove draw-call counts, matrix writes or the
 * attribute-upload behaviour. One page test boots the real renderer and catches
 * shader-compile errors, because the instance-alpha injection is the only part
 * of this task that needs a GPU.
 */

import * as THREE from "three";
import { expect, test } from "@playwright/test";
import {
  EntityRenderer,
  FLAG_ALIVE,
  FLAG_ATTACKING,
  attackScale,
  bobOffset,
  dissolveAlpha,
  tiltAngle,
  type EntityInfo,
  type EntityValue,
} from "../src/render/entities";
import { ParticleSystem, type ParticleInfo } from "../src/render/particles";

interface TestWindow {
  __cbReady?: boolean;
  __cbTest?: {
    info(): { frames: number };
    entityBenchmark(frames?: number): (EntityInfo & { averageMs: number }) | null;
    particleBenchmark(): { spawned: ParticleInfo; after: ParticleInfo } | null;
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

test("18 entities fit in 27 draw calls", () => {
  const parent = new THREE.Group();
  const renderer = new EntityRenderer(parent);
  const players = Array.from({ length: 6 }, (_, i) => entity(i));
  const buddies = Array.from({ length: 12 }, (_, i) =>
    entity(20 + i, { role: i % 6, team: i % 2 }),
  );

  const info = renderer.updateFromValues(players, buddies, true);
  expect(info.players + info.buddies).toBe(18);
  expect(info.drawCalls).toBe(21);
  expect(info.drawCalls).toBeLessThanOrEqual(27);
  renderer.dispose();
  expect(parent.children.length).toBe(0);
});

test("animation comes from the RenderDesc floats", () => {
  expect(bobOffset(0)).toBeCloseTo(0);
  expect(bobOffset(0.25)).toBeCloseTo(0.06);
  expect(bobOffset(0.75)).toBeCloseTo(-0.06);

  expect(tiltAngle(0.4)).toBeCloseTo(0.25);
  expect(tiltAngle(-0.4)).toBeCloseTo(-0.25);

  expect(attackScale(0, 0.25)).toBe(1);
  expect(attackScale(FLAG_ATTACKING, 0.25)).toBeCloseTo(1.35);

  expect(dissolveAlpha(FLAG_ALIVE, 100, 0.5)).toBe(1);
  expect(dissolveAlpha(0, 0, 0.5)).toBeCloseTo(0.5);
  expect(dissolveAlpha(0, 0, 1.5)).toBe(0);
  expect(dissolveAlpha(0, 0, -1)).toBe(1);
});

test("18 entities update in under 3 ms of CPU", () => {
  const parent = new THREE.Group();
  const renderer = new EntityRenderer(parent);
  const players = Array.from({ length: 6 }, (_, i) =>
    entity(i, { anim: i / 6, pitch: i * 0.1 }),
  );
  const buddies = Array.from({ length: 12 }, (_, i) =>
    entity(20 + i, { role: i % 6, anim: i / 12, flags: i === 11 ? 0 : FLAG_ALIVE }),
  );

  for (let i = 0; i < 10; i += 1) {
    renderer.updateFromValues(players, buddies, true);
  }
  let total = 0;
  const frames = 120;
  for (let i = 0; i < frames; i += 1) {
    total += renderer.updateFromValues(players, buddies, true).lastUpdateMs;
  }
  const average = total / frames;
  expect(average).toBeLessThan(3);
  renderer.dispose();
});

test("256 particles are one draw and age on the GPU", () => {
  const parent = new THREE.Group();
  const particles = new ParticleSystem(parent, 256);
  particles.update(0);
  const origin = new THREE.Vector3(0, 1, 0);
  const velocity = new THREE.Vector3(0, 2, 0);
  for (let i = 0; i < 256; i += 1) {
    particles.spawn(origin, velocity, 1, 0.12, 0xffffff);
  }

  const spawned = particles.info();
  expect(spawned.capacity).toBe(256);
  expect(spawned.drawCalls).toBe(1);
  expect(spawned.attributeWrites).toBe(256);

  // Frame updates only move a uniform. The attribute write count staying at 256
  // is the proof that lifetime is not being recomputed on the CPU.
  for (let frame = 0; frame < 120; frame += 1) {
    particles.update(frame / 60);
  }
  const after = particles.info();
  expect(after.attributeWrites).toBe(256);
  expect(after.drawCalls).toBe(1);

  particles.setCapacity(64);
  expect(particles.info().capacity).toBe(64);
  expect(particles.info().drawCalls).toBe(1);
  particles.setCapacity(0);
  expect(particles.info().drawCalls).toBe(0);
  particles.dispose();
});

test("the real renderer compiles the entity and particle shaders", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("console", (message) => {
    if (message.type() === "error") {
      errors.push(message.text());
    }
  });

  await page.goto("/?quality=high&screen=match");
  await page.waitForFunction(() => (window as unknown as TestWindow).__cbReady === true, null, {
    timeout: 30_000,
  });

  const entities = await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.entityBenchmark(60),
  );
  const particles = await page.evaluate(() =>
    (window as unknown as TestWindow).__cbTest!.particleBenchmark(),
  );

  expect(entities).not.toBeNull();
  expect(entities!.drawCalls).toBeLessThanOrEqual(27);
  expect(entities!.averageMs).toBeLessThan(3);
  expect(particles).not.toBeNull();
  expect(particles!.spawned.capacity).toBe(256);
  expect(particles!.spawned.drawCalls).toBe(1);
  expect(particles!.after.attributeWrites).toBe(256);

  const before = await page.evaluate(() => (window as unknown as TestWindow).__cbTest!.info().frames);
  await page.waitForTimeout(500);
  const after = await page.evaluate(() => (window as unknown as TestWindow).__cbTest!.info().frames);
  expect(after - before).toBeGreaterThanOrEqual(25);
  expect(errors).toEqual([]);
});
