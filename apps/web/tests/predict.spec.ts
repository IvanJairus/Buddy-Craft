/**
 * T-041 browser half: sequence numbering and the visual residual. The replay
 * itself is covered by the Rust tests in `crates/client-wasm/src/predict.rs`.
 */

import { expect, test } from "@playwright/test";
import { PredictionGlue, SMOOTHING_SECONDS } from "../src/game/predict";

const ORIGIN = { x: 0, y: 0, z: 0 };

test("the glue numbers inputs and only moves its ack forward", () => {
  const glue = new PredictionGlue();
  for (let index = 1; index <= 128; index += 1) {
    expect(glue.beginInput()).toBe(index);
  }
  expect(glue.pending).toBe(128);

  glue.acknowledge(64);
  expect(glue.pending).toBe(64);
  glue.acknowledge(10);
  expect(glue.pending).toBe(64);
  glue.acknowledge(500);
  expect(glue.pending).toBe(0);

  glue.reset();
  expect(glue.lastSequence).toBe(0);
  expect(glue.pending).toBe(0);
});

test("a one metre correction glides within five centimetres a frame", () => {
  const glue = new PredictionGlue();
  glue.correction({ x: 1, y: 0, z: 0 }, ORIGIN);
  expect(glue.render(ORIGIN).x).toBe(1);
  expect(glue.corrections).toBe(1);

  let previous = glue.render(ORIGIN);
  let maxStep = 0;
  for (let frame = 0; frame < 60; frame += 1) {
    glue.update(1 / 60);
    const next = glue.render(ORIGIN);
    maxStep = Math.max(maxStep, Math.hypot(next.x - previous.x, next.y - previous.y, next.z - previous.z));
    previous = next;
  }
  expect(maxStep).toBeLessThanOrEqual(0.0501);
  expect(glue.residualLength).toBeLessThan(0.01);
});

test("the residual follows exp(-dt/0.15) below the clamp", () => {
  const glue = new PredictionGlue();
  glue.correction({ x: 0.01, y: 0, z: 0 }, ORIGIN);
  glue.update(1 / 60);

  const expected = 0.01 * Math.exp(-1 / 60 / SMOOTHING_SECONDS);
  expect(glue.residualLength).toBeCloseTo(expected, 6);
});
