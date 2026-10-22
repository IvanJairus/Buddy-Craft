/** Shared E2E helpers (task T-054). */

import { expect, type Page } from "@playwright/test";

export const HOST_URL = "http://127.0.0.1:18080";
export const SERVER = "ws://127.0.0.1:18080";
export const OFFLINE_FFA = 0;
export const OFFLINE_TEAMS = 1;
export const KIND_CRAFT = 0;
export const KIND_ELIMINATION = 3;
export const KIND_AIRDROP = 4;
export const KIND_PICKUP = 6;
export const KIND_DEPOSIT = 7;
export const KIND_TELEGRAPH = 8;
export const KIND_CRAFT_BLOCKED = 10;

export interface SessionEvent {
  kind: number;
  a: number;
  b: number;
  c: number;
  tick: number;
}

export interface Stats {
  active: boolean;
  state: string;
  snapshot: boolean;
  decoded: number;
  tick: number;
  x: number;
  y: number;
  z: number;
  players?: Array<{ id: number; x: number; y: number; z: number }>;
  playerId?: number;
  frames?: { snapshots: number; chunkDiffs: number };
  ended?: boolean;
}

interface TestWindow {
  __cbReady?: boolean;
  __cbTest?: {
    screen(): string;
    offlineStart(mode?: number, teams?: number): boolean;
    offlineStats(): Stats;
    offlineAdvance(ticks: number, post?: boolean): void;
    offlineGrant(player: number, crates: number): void;
    offlineDamageCore(team: number, amount: number): void;
    offlinePlaceRecipe(team: number, recipe: number): void;
    offlinePlaceBlocks(team: number, blocks: Array<[number, number, number]>): void;
    onlineStart(roomCode: string, server?: string): boolean;
    onlineStats(): Stats & {
      playerId: number;
      players: Array<{ id: number; x: number; y: number; z: number }>;
      rtt: number | null;
      frames: { snapshots: number; chunkDiffs: number };
      ended: boolean;
    };
    onlineInput(frame: number[]): void;
    onlineHold(frame: number[] | null): void;
    onlineDrop(): void;
    onlineStop(): void;
    hudState(): { crates: number };
    hudFeed(): string[];
    showNearMiss(
      blocks: Array<[number, number, number]>,
      elapsed: number,
    ): { recipeId: number; name: string; missing: number } | null;
    sessionEvents(): SessionEvent[];
    screen(): string;
    touch(): {
      stickActive: boolean;
      lookActive: boolean;
      moveX: number;
      moveY: number;
      frame: number[];
      breaks: number;
      places: number;
      mode: string;
    } | null;
  };
}

export async function boot(page: Page, url = "/?quality=low"): Promise<void> {
  await page.goto(url);
  await page.waitForFunction(
    () => (window as unknown as TestWindow).__cbReady === true,
    null,
    { timeout: 30_000 },
  );
}

export async function startOffline(
  page: Page,
  mode = OFFLINE_FFA,
  teams = 6,
): Promise<void> {
  await page.evaluate(
    ([nextMode, nextTeams]) =>
      (window as unknown as TestWindow).__cbTest!.offlineStart(nextMode, nextTeams),
    [mode, teams],
  );
  await page.waitForFunction(
    () => {
      const stats = (window as unknown as TestWindow).__cbTest!.offlineStats();
      return stats.state === "open" && stats.decoded > 0;
    },
    null,
    { timeout: 15_000 },
  );
}

export async function startOnline(page: Page, room: string, server = SERVER): Promise<void> {
  await page.evaluate(
    ([nextRoom, nextServer]) =>
      (window as unknown as TestWindow).__cbTest!.onlineStart(nextRoom, nextServer),
    [room, server],
  );
  await page.waitForFunction(
    () => {
      const stats = (window as unknown as TestWindow).__cbTest!.onlineStats();
      return stats.state === "open" && stats.snapshot;
    },
    null,
    { timeout: 20_000 },
  );
}

export async function advance(page: Page, ticks: number): Promise<void> {
  const before = await page.evaluate(
    () => (window as unknown as TestWindow).__cbTest!.offlineStats().tick,
  );
  await page.evaluate(
    (count) => (window as unknown as TestWindow).__cbTest!.offlineAdvance(count, true),
    ticks,
  );
  await page.waitForFunction(
    (target) => {
      const api = (window as unknown as TestWindow).__cbTest!;
      const stats = api.offlineStats();
      return stats.tick >= target || stats.ended === true || api.screen() === "results";
    },
    before + ticks - 1,
    { timeout: 15_000 },
  );
  await page.waitForTimeout(100);
}

export async function feed(page: Page): Promise<string[]> {
  return page.evaluate(() => (window as unknown as TestWindow).__cbTest!.hudFeed());
}

export async function events(page: Page): Promise<SessionEvent[]> {
  return page.evaluate(() => (window as unknown as TestWindow).__cbTest!.sessionEvents());
}

export async function waitForEvent(page: Page, kind: number): Promise<SessionEvent> {
  await page.waitForFunction(
    (wanted) =>
      (window as unknown as TestWindow).__cbTest!.sessionEvents().some(
        (event) => event.kind === wanted,
      ),
    kind,
    { timeout: 15_000 },
  );
  const found = await page.evaluate(
    (wanted) =>
      (window as unknown as TestWindow).__cbTest!.sessionEvents().find(
        (event) => event.kind === wanted,
      ) ?? null,
    kind,
  );
  expect(found).not.toBeNull();
  return found as SessionEvent;
}

export async function hostCommand(body: Record<string, unknown>): Promise<void> {
  const response = await fetch(`${HOST_URL}/debug/command`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  expect(response.status).toBe(202);
}

export async function grant(page: Page, player: number, crates: number): Promise<void> {
  await page.evaluate(
    ([seat, count]) =>
      (window as unknown as TestWindow).__cbTest!.offlineGrant(seat, count),
    [player, crates],
  );
}

export async function placeRecipe(
  page: Page,
  team: number,
  recipe: number,
): Promise<void> {
  await page.evaluate(
    ([nextTeam, nextRecipe]) =>
      (window as unknown as TestWindow).__cbTest!.offlinePlaceRecipe(nextTeam, nextRecipe),
    [team, recipe],
  );
}

export async function placeBlocks(
  page: Page,
  team: number,
  blocks: Array<[number, number, number]>,
): Promise<void> {
  await page.evaluate(
    ([nextTeam, nextBlocks]) =>
      (window as unknown as TestWindow).__cbTest!.offlinePlaceBlocks(
        nextTeam as number,
        nextBlocks as Array<[number, number, number]>,
      ),
    [team, blocks] as const,
  );
}

export async function hudCrates(page: Page): Promise<number> {
  return page.evaluate(
    () => (window as unknown as TestWindow).__cbTest!.hudState().crates,
  );
}

export async function onlineStats(page: Page): Promise<Stats & {
  playerId: number;
  players: Array<{ id: number; x: number; y: number; z: number }>;
  rtt: number | null;
  frames: { snapshots: number; chunkDiffs: number };
  ended: boolean;
}> {
  return page.evaluate(() => (window as unknown as TestWindow).__cbTest!.onlineStats());
}

export async function moveOnline(
  page: Page,
  ms = 900,
  frame: [number, number, number, number] = [0, 0, 0, 0x50],
): Promise<void> {
  await page.evaluate(
    (bytes) => (window as unknown as TestWindow).__cbTest!.onlineHold(bytes),
    frame,
  );
  await page.waitForTimeout(ms);
  await page.evaluate(() => (window as unknown as TestWindow).__cbTest!.onlineHold(null));
}

export async function onlineDrop(page: Page): Promise<void> {
  await page.evaluate(() => (window as unknown as TestWindow).__cbTest!.onlineDrop());
}

export async function touchState(page: Page): Promise<NonNullable<ReturnType<NonNullable<TestWindow["__cbTest"]>["touch"]>>> {
  const value = await page.evaluate(
    () => (window as unknown as TestWindow).__cbTest!.touch(),
  );
  expect(value).not.toBeNull();
  return value as NonNullable<typeof value>;
}

export async function nearMiss(
  page: Page,
  blocks: Array<[number, number, number]>,
  elapsed = 0,
): Promise<{ recipeId: number; name: string; missing: number } | null> {
  return page.evaluate(
    ([nextBlocks, seconds]) =>
      (window as unknown as TestWindow).__cbTest!.showNearMiss(
        nextBlocks as Array<[number, number, number]>,
        seconds as number,
      ),
    [blocks, elapsed] as const,
  );
}

export function roomCode(): string {
  const raw = Date.now().toString(36).toUpperCase().replace(/[^0-9A-Z]/g, "");
  return `T${raw.slice(-5).padStart(5, "0")}`;
}

export async function screen(page: Page): Promise<string> {
  return page.evaluate(() => (window as unknown as TestWindow).__cbTest!.screen());
}
