/**
 * T-060: the four economy metrics are a pure fold over snapshot events.
 */

import { expect, test } from "@playwright/test";
import {
  EconomyTracker,
  EVENT_CRAFT,
  EVENT_PICKUP,
} from "../src/telemetry/economy";

test("economy tracker counts pickups, recipes, buddies and first buddy time", () => {
  const tracker = new EconomyTracker(6);
  tracker.observe(EVENT_PICKUP, 0, 4, 0, 20);
  tracker.observe(EVENT_PICKUP, 1, 6, 0, 40);
  tracker.observe(EVENT_CRAFT, 0, 0, 0, 100);
  tracker.observe(EVENT_CRAFT, 0, 3, 1, 240);
  tracker.observe(EVENT_CRAFT, 1, 3, 1, 400);

  const summary = tracker.summary(1200);
  expect(summary.cratesPicked).toBe(10);
  expect(summary.cratesPerMinute).toBe(10);
  expect(summary.cratesPerMinutePerPlayer).toBeCloseTo(10 / 6, 5);
  expect(summary.recipesCompleted).toBe(3);
  expect(summary.buddiesSpawned).toBe(2);
  expect(summary.firstBuddyTick).toBe(240);
  expect(summary.timeToFirstBuddySeconds).toBe(12);
  expect(summary.matchSeconds).toBe(60);
  expect(summary.playerCount).toBe(6);
});

test("economy tracker ignores unknown events and empty matches", () => {
  const tracker = new EconomyTracker(6);
  tracker.observe(99, 1, 2, 3, 20);
  const summary = tracker.summary(0);
  expect(summary.cratesPicked).toBe(0);
  expect(summary.recipesCompleted).toBe(0);
  expect(summary.buddiesSpawned).toBe(0);
  expect(summary.timeToFirstBuddySeconds).toBeNull();
  expect(summary.matchSeconds).toBe(0);
});
