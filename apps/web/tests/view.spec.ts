/**
 * Local first-person view: byte yaw and ±64 pitch must become the same facing
 * vector the server uses, with the eye on top of the body origin.
 */

import { expect, test } from "@playwright/test";
import { EYE_HEIGHT, eyePose } from "../src/game/view";
import type { EyePose } from "../src/game/view";

function forward(pose: EyePose): { x: number; y: number; z: number } {
  return {
    x: -Math.cos(pose.rotationX) * Math.sin(pose.rotationY),
    y: Math.sin(pose.rotationX),
    z: -Math.cos(pose.rotationX) * Math.cos(pose.rotationY),
  };
}

test("yaw 0 looks down +Z and a quarter turn looks down +X", () => {
  const north = forward(eyePose(10, 40, 20, 0, 0));
  expect(north.x).toBeCloseTo(0, 6);
  expect(north.y).toBeCloseTo(0, 6);
  expect(north.z).toBeCloseTo(1, 6);

  const east = forward(eyePose(10, 40, 20, 64, 0));
  expect(east.x).toBeCloseTo(1, 6);
  expect(east.z).toBeCloseTo(0, 6);
});

test("pitch 64 is straight up, -64 straight down and the eye is on the body", () => {
  const up = forward(eyePose(10, 40, 20, 0, 64));
  expect(up.y).toBeCloseTo(1, 6);
  const down = forward(eyePose(10, 40, 20, 0, -64));
  expect(down.y).toBeCloseTo(-1, 6);

  const pose = eyePose(10, 40, 20, 0, 0);
  expect(pose.x).toBe(10);
  expect(pose.y).toBe(40 + EYE_HEIGHT);
  expect(pose.z).toBe(20);
});

test("pitch clamps at vertical and yaw wraps like a byte", () => {
  expect(eyePose(0, 0, 0, 0, 100).rotationX).toBeCloseTo(Math.PI / 2, 6);
  expect(eyePose(0, 0, 0, 0, -100).rotationX).toBeCloseTo(-Math.PI / 2, 6);

  const before = eyePose(0, 0, 0, 255, 0).rotationY;
  const after = eyePose(0, 0, 0, -1, 0).rotationY;
  expect(after).toBeCloseTo(before, 6);
});
