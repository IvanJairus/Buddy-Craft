/**
 * T-043 acceptance specs for snapshot decoding. The wire vectors come from
 * `crates/protocol`, so a field-order change fails on both sides.
 */

import { expect, test, type Page } from "@playwright/test";
import { Interpolator } from "../src/game/interp";
import { SnapshotDecoder } from "../src/game/net/snapshot";
import type { EntityInfo } from "../src/render/entities";

interface TestWindow {
  __cbReady?: boolean;
  __cbTest?: {
    snapshotView(
      frame: number[],
      nowMs: number,
    ): { decoded: number; players: number; buddies: number; entity: EntityInfo | null };
  };
}

const KEYFRAME = new Uint8Array([
  0x84, 0x05, 0x01, 0x00, 0x01, 0x01, 0x02, 0x1f, 0x02, 0x00, 0x00, 0x40, 0x64, 0x01, 0x00,
  0x00, 0x00, 0x00, 0x00,
]);
const DELTA = new Uint8Array([
  0x84, 0x06, 0x02, 0x01, 0x00, 0x01, 0x02, 0x01, 0x80, 0x80, 0x08, 0x00, 0x00, 0x00, 0x00,
  0x00, 0x00,
]);
const EXTRAS = new Uint8Array([
  0x84, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x01, 0x01, 0x02, 0x01, 0x00, 0x01, 0xff, 0x00,
  0x01, 0x01, 0x32, 0x04, 0x00, 0x03, 0x01, 0x05, 0x02, 0x01, 0x00,
]);

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

function zigzag(value: number): number[] {
  return varint(value >= 0 ? value * 2 : -value * 2 - 1);
}

function keyframe(
  seq: number,
  tick: number,
  xRaw: number,
  yaw = 0,
  yRaw = 0,
  zRaw = 0,
): Uint8Array {
  return Uint8Array.from([
    0x84,
    ...varint(tick),
    ...varint(seq),
    0,
    1,
    1,
    2,
    0x1f,
    ...zigzag(xRaw),
    ...zigzag(yRaw),
    ...zigzag(zRaw),
    yaw,
    100,
    1,
    0,
    0,
    0,
    0,
    0,
  ]);
}

function delta(seq: number, baseline: number, dx: number): Uint8Array {
  return Uint8Array.from([
    0x84,
    ...varint(seq),
    ...varint(seq),
    ...varint(baseline),
    0,
    1,
    2,
    1,
    ...zigzag(dx),
    0,
    0,
    0,
    0,
    0,
    0,
  ]);
}

test("a keyframe and its delta reconstruct absolute state", () => {
  const decoder = new SnapshotDecoder();
  const first = decoder.decode(KEYFRAME);
  expect(first).not.toBeNull();
  expect(first!.seq).toBe(1);
  expect(first!.playerCount).toBe(1);
  expect(first!.playerIds[0]).toBe(2);
  expect(first!.playerX[0]).toBe(1);
  expect(first!.playerHp[0]).toBe(100);
  expect(first!.playerYaw[0]).toBe(64);
  expect(first!.playerX[0] / 65_536).toBeCloseTo(1 / 65_536, 8);

  const second = decoder.decode(DELTA);
  expect(second).not.toBeNull();
  expect(second!.seq).toBe(2);
  expect(second!.playerX[0]).toBe(65_537);
  expect(decoder.lastSeq).toBe(2);
  expect(decoder.needsKeyframe).toBe(0);
});

test("a delta without its baseline asks for a keyframe instead of guessing", () => {
  const decoder = new SnapshotDecoder();
  expect(decoder.decode(DELTA)).toBeNull();
  expect(decoder.needsKeyframe).toBe(1);
  expect(decoder.lastSeq).toBe(0);
});

test("projectiles, structures and events advance the cursor correctly", () => {
  const decoder = new SnapshotDecoder();
  const frame = decoder.decode(EXTRAS);
  expect(frame).not.toBeNull();
  expect(frame!.playerCount).toBe(0);
  expect(frame!.buddyCount).toBe(0);
  expect(decoder.decoded).toBe(1);
});

test("no_gc_in_hot_path: 600 decodes reuse the same pools", () => {
  const decoder = new SnapshotDecoder();
  const interp = new Interpolator(decoder.playerTeam, decoder.buddyRole);
  expect(decoder.decode(keyframe(1, 1, 0))).not.toBeNull();
  let reused = null;
  let x = 0;
  for (let seq = 2; seq <= 601; seq += 1) {
    x += 1;
    const frame = decoder.decode(delta(seq, seq - 1, 1));
    expect(frame).not.toBeNull();
    if (seq === 537) reused = frame;
    interp.push(frame!, seq * 50);
    interp.sample(seq * 50);
  }
  expect(decoder.decoded).toBe(601);
  expect(decoder.needsKeyframe).toBe(0);
  expect(decoder.poolGeneration).toBe(1);
  expect(interp.poolGeneration).toBe(1);
  expect(interp.playerCount).toBe(1);
  expect(decoder.frame(601)!.playerX[0]).toBe(600);
  // 537 and 601 share a ring slot, so the same frame object and arrays return.
  expect(reused).not.toBeNull();
  expect(decoder.frame(601)).toBe(reused);
  expect(decoder.frame(601)!.playerIds).toBe(reused!.playerIds);
});

test("a decoded snapshot reaches the entity renderer", async ({ page }: { page: Page }) => {
  await page.goto("/?screen=match");
  await page.waitForFunction(() => (window as unknown as TestWindow).__cbReady === true, null, {
    timeout: 30_000,
  });
  const frame = Array.from(keyframe(1, 10, 128 * 65_536, 0, 40 * 65_536, 128 * 65_536));
  const result = await page.evaluate(
    (bytes) => (window as unknown as TestWindow).__cbTest!.snapshotView(bytes, 1_000),
    frame,
  );
  expect(result.decoded).toBe(1);
  expect(result.players).toBe(1);
  expect(result.buddies).toBe(0);
  expect(result.entity?.players).toBe(1);
  // T-051: the local snapshot now renders through one instanced model mesh,
  // not the three-part box fallback.
  expect(result.entity?.drawCalls).toBe(1);
});
