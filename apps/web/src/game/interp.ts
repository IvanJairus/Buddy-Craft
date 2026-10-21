/**
 * Entity interpolation (task T-043, plan §6.5).
 *
 * Remote entities render 100 ms in the past. This class keeps the last four
 * snapshots by reference and samples at `server time - 2 ticks`, which is the
 * two-snapshot delay the plan asks for. If snapshots stop, the render clock is
 * clamped to `newest + 2 ticks` and the entity freezes. On recovery the clock
 * catches up at 1.35x real time instead of snapping, so three lost snapshots do
 * not teleport a body.
 *
 * The output pools are typed arrays allocated in the constructor. `push` and
 * `sample` create no per-entity objects, which is what keeps a 20 Hz snapshot
 * stream out of the young generation.
 */

import {
  MAX_SNAPSHOT_BUDDIES,
  MAX_SNAPSHOT_PLAYERS,
  type SnapshotFrame,
} from "./net/snapshot";

const HISTORY = 4;
const TICK_MS = 50;
const DELAY_TICKS = 2;
const MAX_EXTRAPOLATION_TICKS = 2;
const CATCH_UP = 1.35;
const TAU = Math.PI * 2;

export class Interpolator {
  readonly poolGeneration = 1;
  readonly playerIds = new Uint8Array(MAX_SNAPSHOT_PLAYERS);
  readonly playerX = new Float32Array(MAX_SNAPSHOT_PLAYERS);
  readonly playerY = new Float32Array(MAX_SNAPSHOT_PLAYERS);
  readonly playerZ = new Float32Array(MAX_SNAPSHOT_PLAYERS);
  readonly playerYaw = new Float32Array(MAX_SNAPSHOT_PLAYERS);
  readonly playerFlags = new Uint8Array(MAX_SNAPSHOT_PLAYERS);
  readonly playerHp = new Uint8Array(MAX_SNAPSHOT_PLAYERS);
  readonly playerAnim = new Float32Array(MAX_SNAPSHOT_PLAYERS);
  readonly playerTeam = new Uint8Array(MAX_SNAPSHOT_PLAYERS);
  readonly buddyIds = new Uint8Array(MAX_SNAPSHOT_BUDDIES);
  readonly buddyX = new Float32Array(MAX_SNAPSHOT_BUDDIES);
  readonly buddyY = new Float32Array(MAX_SNAPSHOT_BUDDIES);
  readonly buddyZ = new Float32Array(MAX_SNAPSHOT_BUDDIES);
  readonly buddyHp = new Uint8Array(MAX_SNAPSHOT_BUDDIES);
  readonly buddyRole = new Uint8Array(MAX_SNAPSHOT_BUDDIES);
  playerCount = 0;
  buddyCount = 0;
  renderTick = 0;

  private readonly history: Array<SnapshotFrame | null> = [null, null, null, null];
  private readonly teams: Uint8Array;
  private readonly roles: Uint8Array;
  private next = 0;
  private count = 0;
  private latest: SnapshotFrame | null = null;
  private latestArrival = 0;
  private renderClock = 0;
  private lastNow = 0;
  private started = false;

  constructor(teams: Uint8Array, roles: Uint8Array) {
    this.teams = teams;
    this.roles = roles;
  }

  /** Add a decoded snapshot and the client time it arrived. */
  push(frame: SnapshotFrame, nowMs: number): void {
    if (!frame.valid) return;
    this.history[this.next] = frame;
    this.next = (this.next + 1) % HISTORY;
    if (this.count < HISTORY) this.count += 1;
    this.latest = frame;
    this.latestArrival = nowMs;
    if (!this.started) {
      this.started = true;
      this.lastNow = nowMs;
      this.renderClock = frame.tick - DELAY_TICKS;
    }
  }

  /** Sample the buffer for this client frame. Writes the output pools. */
  sample(nowMs: number): void {
    if (this.latest === null) {
      this.playerCount = 0;
      this.buddyCount = 0;
      return;
    }
    const dt = Math.min(Math.max(nowMs - this.lastNow, 0), 4 * TICK_MS);
    this.lastNow = nowMs;

    const oldest = this.oldest();
    const estimate = this.latest.tick + (nowMs - this.latestArrival) / TICK_MS;
    let desired = estimate - DELAY_TICKS;
    const cap = this.latest.tick + MAX_EXTRAPOLATION_TICKS;
    desired = Math.min(Math.max(desired, oldest.tick), cap);
    const maxStep = Math.max(0.05, (dt / TICK_MS) * CATCH_UP);
    const step = Math.min(Math.max(desired - this.renderClock, -maxStep), maxStep);
    this.renderClock = Math.min(Math.max(this.renderClock + step, oldest.tick), cap);
    this.renderTick = this.renderClock;
    this.sampleAt(this.renderClock);
  }

  private oldest(): SnapshotFrame {
    const start = (this.next - this.count + HISTORY) % HISTORY;
    return this.history[start]!;
  }

  private sampleAt(tick: number): void {
    const start = (this.next - this.count + HISTORY) % HISTORY;
    const first = this.history[start]!;
    const last = this.history[(start + this.count - 1) % HISTORY]!;
    let older = first;
    let newer = first;
    let bracketed = false;
    for (let k = 1; k < this.count; k += 1) {
      const frame = this.history[(start + k) % HISTORY]!;
      if (frame.tick <= tick) {
        older = frame;
      } else {
        newer = frame;
        bracketed = true;
        break;
      }
    }
    if (!bracketed) {
      if (tick > last.tick) {
        newer = last;
        older = this.count > 1
          ? this.history[(start + this.count - 2) % HISTORY]!
          : last;
      } else {
        older = last;
        newer = last;
      }
    }
    if (tick <= first.tick) {
      older = first;
      newer = first;
    }
    const span = newer.tick - older.tick;
    const alpha = span > 0 ? (tick - older.tick) / span : 0;

    this.playerCount = newer.playerCount;
    for (let i = 0; i < newer.playerCount; i += 1) {
      const id = newer.playerIds[i];
      let at = -1;
      for (let j = 0; j < older.playerCount; j += 1) {
        if (older.playerIds[j] === id) {
          at = j;
          break;
        }
      }
      this.playerIds[i] = id;
      this.playerX[i] = lerpRaw(older.playerX, newer.playerX, at, i, alpha);
      this.playerY[i] = lerpRaw(older.playerY, newer.playerY, at, i, alpha);
      this.playerZ[i] = lerpRaw(older.playerZ, newer.playerZ, at, i, alpha);
      this.playerYaw[i] = lerpYaw(older, newer, at, i, alpha);
      this.playerHp[i] = newer.playerHp[i];
      this.playerFlags[i] = newer.playerFlags[i];
      this.playerAnim[i] = newer.playerAnim[i] / 255;
      this.playerTeam[i] = this.teams[id];
    }

    this.buddyCount = newer.buddyCount;
    for (let i = 0; i < newer.buddyCount; i += 1) {
      const id = newer.buddyIds[i];
      let at = -1;
      for (let j = 0; j < older.buddyCount; j += 1) {
        if (older.buddyIds[j] === id) {
          at = j;
          break;
        }
      }
      this.buddyIds[i] = id;
      this.buddyX[i] = lerpRaw(older.buddyX, newer.buddyX, at, i, alpha);
      this.buddyY[i] = lerpRaw(older.buddyY, newer.buddyY, at, i, alpha);
      this.buddyZ[i] = lerpRaw(older.buddyZ, newer.buddyZ, at, i, alpha);
      this.buddyHp[i] = newer.buddyHp[i];
      this.buddyRole[i] = this.roles[id];
    }
  }
}

function lerpRaw(
  older: Int32Array,
  newer: Int32Array,
  at: number,
  index: number,
  alpha: number,
): number {
  const from = at >= 0 ? older[at] : newer[index];
  return (from + (newer[index] - from) * alpha) / 65_536;
}

function lerpYaw(
  older: SnapshotFrame,
  newer: SnapshotFrame,
  at: number,
  index: number,
  alpha: number,
): number {
  const from = at >= 0 ? older.playerYaw[at] : newer.playerYaw[index];
  const delta = ((newer.playerYaw[index] - from + 384) & 0xff) - 128;
  const turn = (((from + delta * alpha) % 256) + 256) % 256;
  return (turn * TAU) / 256;
}
