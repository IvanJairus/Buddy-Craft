/**
 * T-043 acceptance specs for entity interpolation: 100 ms delay, nearest pair,
 * constant velocity smoothness and recovery from three lost snapshots without
 * a per-frame teleport.
 */

import { expect, test } from "@playwright/test";
import { Interpolator } from "../src/game/interp";
import { SnapshotDecoder } from "../src/game/net/snapshot";

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

function keyframe(seq: number, tick: number, xRaw: number): Uint8Array {
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
    0,
    0,
    0,
    100,
    1,
    0,
    0,
    0,
    0,
    0,
  ]);
}

const STEP_RAW = 14_746; // 0.225 m per 50 ms tick, 4.5 m/s

function harness(): { decoder: SnapshotDecoder; interp: Interpolator } {
  const decoder = new SnapshotDecoder();
  return { decoder, interp: new Interpolator(decoder.playerTeam, decoder.buddyRole) };
}

test("interpolation_is_smooth at constant velocity", () => {
  const { decoder, interp } = harness();
  let nextTick = 0;
  let nextArrival = 0;
  let previous = 0;
  let maxStep = 0;
  let started = false;

  for (let now = 0; now <= 400; now += 16.7) {
    while (nextTick <= 3 && nextArrival <= now) {
      decoder.decode(keyframe(nextTick + 1, nextTick, nextTick * STEP_RAW));
      interp.push(decoder.latestFrame!, nextArrival);
      nextTick += 1;
      nextArrival = nextTick * 50;
    }
    interp.sample(now);
    expect(interp.playerCount).toBe(1);
    const x = interp.playerX[0];
    if (started) {
      expect(x).toBeGreaterThanOrEqual(previous - 1e-6);
      maxStep = Math.max(maxStep, x - previous);
    }
    previous = x;
    started = true;
  }

  expect(maxStep).toBeLessThan(0.13);
  expect(interp.poolGeneration).toBe(1);
});

test("losing three snapshots does not teleport the body", () => {
  const { decoder, interp } = harness();
  const arrivals = [0, 50, 100, 300, 350, 400];
  const ticks = [0, 1, 2, 6, 7, 8];
  let next = 0;
  let previous = 0;
  let maxStep = 0;
  let started = false;

  for (let now = 0; now <= 500; now += 16.7) {
    while (next < arrivals.length && arrivals[next] <= now) {
      decoder.decode(keyframe(ticks[next] + 1, ticks[next], ticks[next] * STEP_RAW));
      interp.push(decoder.latestFrame!, arrivals[next]);
      next += 1;
    }
    interp.sample(now);
    const x = interp.playerX[0];
    if (started) {
      expect(x).toBeGreaterThanOrEqual(previous - 1e-6);
      maxStep = Math.max(maxStep, x - previous);
    }
    previous = x;
    started = true;
  }

  expect(maxStep).toBeLessThan(0.13);
  expect(interp.playerCount).toBe(1);
});

test("a long outage freezes after 100 ms of extrapolation", () => {
  const { decoder, interp } = harness();
  const arrivals = [0, 50, 100, 500, 550, 600];
  const ticks = [0, 1, 2, 12, 13, 14];
  let next = 0;
  let at350 = 0;
  let at450 = 0;
  let previous = 0;
  let maxStep = 0;
  let started = false;

  for (let now = 0; now <= 700; now += 16.7) {
    while (next < arrivals.length && arrivals[next] <= now) {
      decoder.decode(keyframe(ticks[next] + 1, ticks[next], ticks[next] * STEP_RAW));
      interp.push(decoder.latestFrame!, arrivals[next]);
      next += 1;
    }
    interp.sample(now);
    const x = interp.playerX[0];
    if (started) {
      expect(x).toBeGreaterThanOrEqual(previous - 1e-6);
      maxStep = Math.max(maxStep, x - previous);
    }
    previous = x;
    started = true;
    if (at350 === 0 && now >= 350) at350 = x;
    if (at450 === 0 && now >= 450) at450 = x;
  }

  expect(at450).toBeCloseTo(at350, 6);
  expect(maxStep).toBeLessThan(0.13);
});
