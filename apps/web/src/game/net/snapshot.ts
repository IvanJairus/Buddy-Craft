/**
 * Snapshot decode (task T-043, plan §6.3/§7.3).
 *
 * The server sends a keyframe every three seconds and position deltas against
 * the last snapshot the client acknowledged. Every value lands in fixed-size
 * typed arrays allocated once per decoder: a frame never creates a per-entity
 * object, and the 64-frame ring lets a delta name any recent baseline.
 *
 * A delta whose baseline has been evicted is not guessed at. The decoder
 * returns null and raises `needsKeyframe`; the server sends a keyframe when the
 * client's acknowledgement falls out of the history.
 *
 * Teams and buddy roles are not on the snapshot wire (§7.3). They come from
 * the lobby and are copied into small lookup tables at construction.
 */

export const S2C_SNAPSHOT = 0x84;
export const MAX_SNAPSHOT_BYTES = 2_048;
export const MAX_SNAPSHOT_PLAYERS = 6;
export const MAX_SNAPSHOT_BUDDIES = 12;
export const MAX_SNAPSHOT_PROJECTILES = 24;
export const MAX_SNAPSHOT_STRUCTURES = 4;
export const MAX_SNAPSHOT_EVENTS = 16;
export const BASELINE_FRAMES = 64;

export const PRESENCE_POS = 1 << 0;
export const PRESENCE_YAW = 1 << 1;
export const PRESENCE_HP = 1 << 2;
export const PRESENCE_FLAGS = 1 << 3;
export const PRESENCE_ANIM = 1 << 4;
export const PRESENCE_ALL = 0x1f;

export interface SnapshotFrame {
  seq: number;
  tick: number;
  valid: boolean;
  playerCount: number;
  buddyCount: number;
  readonly playerIds: Uint8Array;
  readonly playerX: Int32Array;
  readonly playerY: Int32Array;
  readonly playerZ: Int32Array;
  readonly playerYaw: Uint8Array;
  readonly playerHp: Uint8Array;
  readonly playerFlags: Uint8Array;
  readonly playerAnim: Uint8Array;
  readonly buddyIds: Uint8Array;
  readonly buddyX: Int32Array;
  readonly buddyY: Int32Array;
  readonly buddyZ: Int32Array;
  readonly buddyHp: Uint8Array;
}

function fail(reason: string): never {
  throw new Error(`snapshot: ${reason}`);
}

const EMPTY = new Uint8Array(0);

/** A cursor over one frame. Reused by the decoder; no per-read objects. */
class Reader {
  bytes: Uint8Array = EMPTY;
  at = 0;

  reset(bytes: Uint8Array): void {
    this.bytes = bytes;
    this.at = 0;
  }

  u8(): number {
    if (this.at >= this.bytes.length) fail("truncated byte");
    const byte = this.bytes[this.at];
    this.at += 1;
    return byte;
  }

  varint(): number {
    let value = 0;
    let shift = 0;
    for (;;) {
      if (this.at >= this.bytes.length) fail("truncated varint");
      const byte = this.bytes[this.at];
      this.at += 1;
      value += (byte & 0x7f) * 2 ** shift;
      if ((byte & 0x80) === 0) return value >>> 0;
      shift += 7;
      if (shift > 35) fail("varint longer than five bytes");
    }
  }

  signed(): number {
    const raw = this.varint();
    return (raw >>> 1) ^ -(raw & 1);
  }
}

function frameOf(): SnapshotFrame {
  return {
    seq: 0,
    tick: 0,
    valid: false,
    playerCount: 0,
    buddyCount: 0,
    playerIds: new Uint8Array(MAX_SNAPSHOT_PLAYERS),
    playerX: new Int32Array(MAX_SNAPSHOT_PLAYERS),
    playerY: new Int32Array(MAX_SNAPSHOT_PLAYERS),
    playerZ: new Int32Array(MAX_SNAPSHOT_PLAYERS),
    playerYaw: new Uint8Array(MAX_SNAPSHOT_PLAYERS),
    playerHp: new Uint8Array(MAX_SNAPSHOT_PLAYERS),
    playerFlags: new Uint8Array(MAX_SNAPSHOT_PLAYERS),
    playerAnim: new Uint8Array(MAX_SNAPSHOT_PLAYERS),
    buddyIds: new Uint8Array(MAX_SNAPSHOT_BUDDIES),
    buddyX: new Int32Array(MAX_SNAPSHOT_BUDDIES),
    buddyY: new Int32Array(MAX_SNAPSHOT_BUDDIES),
    buddyZ: new Int32Array(MAX_SNAPSHOT_BUDDIES),
    buddyHp: new Uint8Array(MAX_SNAPSHOT_BUDDIES),
  };
}

export class SnapshotDecoder {
  readonly playerTeam = new Uint8Array(256);
  readonly buddyRole = new Uint8Array(256);
  readonly poolGeneration = 1;
  needsKeyframe = 0;
  decoded = 0;
  private readonly frames: SnapshotFrame[] = [];
  private readonly reader = new Reader();
  private latest: SnapshotFrame | null = null;

  constructor(
    teams: ReadonlyMap<number, number> = new Map(),
    roles: ReadonlyMap<number, number> = new Map(),
  ) {
    for (let i = 0; i < BASELINE_FRAMES; i += 1) {
      this.frames.push(frameOf());
    }
    for (const [id, team] of teams) this.playerTeam[id & 0xff] = team & 0xff;
    for (const [id, role] of roles) this.buddyRole[id & 0xff] = role & 0xff;
  }

  get lastSeq(): number {
    return this.latest?.seq ?? 0;
  }

  get latestFrame(): SnapshotFrame | null {
    return this.latest;
  }

  reset(): void {
    for (const frame of this.frames) frame.valid = false;
    this.latest = null;
    this.needsKeyframe = 0;
    this.decoded = 0;
  }

  frame(seq: number): SnapshotFrame | null {
    if (seq === 0) return null;
    const frame = this.frames[seq % BASELINE_FRAMES];
    return frame.valid && frame.seq === seq ? frame : null;
  }

  /** Decode one `S2C_SNAPSHOT` frame. Returns the reused frame object. */
  decode(bytes: Uint8Array): SnapshotFrame | null {
    if (bytes.length > MAX_SNAPSHOT_BYTES) {
      fail(`${bytes.length} bytes exceeds the ${MAX_SNAPSHOT_BYTES}-byte budget`);
    }
    if (bytes.length === 0) fail("empty frame");
    const reader = this.reader;
    reader.reset(bytes);
    if (reader.u8() !== S2C_SNAPSHOT) fail(`expected tag ${S2C_SNAPSHOT}`);
    const tick = reader.varint();
    const seq = reader.varint();
    const baselineSeq = reader.varint();
    if (seq === 0 || seq > 0xffff || baselineSeq > 0xffff) fail("sequence out of range");
    const keyframeByte = reader.u8();
    if (keyframeByte > 1) fail("keyframe is not a bool");
    const keyframe = keyframeByte === 1;
    const baseline = keyframe ? null : this.frame(baselineSeq);
    if (!keyframe && baseline === null) {
      this.needsKeyframe += 1;
      return null;
    }

    const playerCount = reader.varint();
    if (playerCount > MAX_SNAPSHOT_PLAYERS) fail("too many players");
    const target = this.frames[seq % BASELINE_FRAMES];
    if (baseline === target) {
      this.needsKeyframe += 1;
      return null;
    }
    target.valid = false;
    target.playerCount = 0;
    target.buddyCount = 0;

    for (let i = 0; i < playerCount; i += 1) {
      const id = reader.u8();
      const presence = reader.varint();
      if ((presence & ~PRESENCE_ALL) !== 0) fail("unknown player presence bit");
      for (let j = 0; j < target.playerCount; j += 1) {
        if (target.playerIds[j] === id) fail(`duplicate player ${id}`);
      }
      let base = -1;
      if (baseline !== null) {
        for (let j = 0; j < baseline.playerCount; j += 1) {
          if (baseline.playerIds[j] === id) {
            base = j;
            break;
          }
        }
      }
      let x = base >= 0 ? baseline!.playerX[base] : 0;
      let y = base >= 0 ? baseline!.playerY[base] : 0;
      let z = base >= 0 ? baseline!.playerZ[base] : 0;
      if (presence & PRESENCE_POS) {
        x = (x + reader.signed()) | 0;
        y = (y + reader.signed()) | 0;
        z = (z + reader.signed()) | 0;
      }
      let yaw = base >= 0 ? baseline!.playerYaw[base] : 0;
      let hp = base >= 0 ? baseline!.playerHp[base] : 0;
      let flags = base >= 0 ? baseline!.playerFlags[base] : 0;
      let anim = base >= 0 ? baseline!.playerAnim[base] : 0;
      if (presence & PRESENCE_YAW) yaw = reader.u8();
      if (presence & PRESENCE_HP) hp = reader.u8();
      if (presence & PRESENCE_FLAGS) flags = reader.u8();
      if (presence & PRESENCE_ANIM) anim = reader.u8();
      const index = target.playerCount;
      target.playerIds[index] = id;
      target.playerX[index] = x;
      target.playerY[index] = y;
      target.playerZ[index] = z;
      target.playerYaw[index] = yaw;
      target.playerHp[index] = hp;
      target.playerFlags[index] = flags;
      target.playerAnim[index] = anim;
      target.playerCount += 1;
    }

    const buddyCount = reader.varint();
    if (buddyCount > MAX_SNAPSHOT_BUDDIES) fail("too many buddies");
    target.buddyCount = 0;
    for (let i = 0; i < buddyCount; i += 1) {
      const id = reader.u8();
      const presence = reader.varint();
      if ((presence & ~(PRESENCE_POS | PRESENCE_HP)) !== 0) {
        fail("unknown buddy presence bit");
      }
      for (let j = 0; j < target.buddyCount; j += 1) {
        if (target.buddyIds[j] === id) fail(`duplicate buddy ${id}`);
      }
      let base = -1;
      if (baseline !== null) {
        for (let j = 0; j < baseline.buddyCount; j += 1) {
          if (baseline.buddyIds[j] === id) {
            base = j;
            break;
          }
        }
      }
      let x = base >= 0 ? baseline!.buddyX[base] : 0;
      let y = base >= 0 ? baseline!.buddyY[base] : 0;
      let z = base >= 0 ? baseline!.buddyZ[base] : 0;
      if (presence & PRESENCE_POS) {
        x = (x + reader.signed()) | 0;
        y = (y + reader.signed()) | 0;
        z = (z + reader.signed()) | 0;
      }
      let hp = base >= 0 ? baseline!.buddyHp[base] : 0;
      if (presence & PRESENCE_HP) hp = reader.u8();
      const index = target.buddyCount;
      target.buddyIds[index] = id;
      target.buddyX[index] = x;
      target.buddyY[index] = y;
      target.buddyZ[index] = z;
      target.buddyHp[index] = hp;
      target.buddyCount += 1;
    }

    const projectileCount = reader.varint();
    if (projectileCount > MAX_SNAPSHOT_PROJECTILES) fail("too many projectiles");
    for (let i = 0; i < projectileCount; i += 1) {
      reader.varint();
      for (let j = 0; j < 3; j += 1) reader.signed();
      // postcard writes i8 as one two's-complement byte, not a zigzag varint.
      for (let j = 0; j < 3; j += 1) reader.u8();
    }
    const structureCount = reader.varint();
    if (structureCount > MAX_SNAPSHOT_STRUCTURES) fail("too many structures");
    for (let i = 0; i < structureCount; i += 1) {
      reader.u8();
      reader.u8();
      for (let j = 0; j < 3; j += 1) reader.signed();
    }
    const eventCount = reader.varint();
    if (eventCount > MAX_SNAPSHOT_EVENTS) fail("too many events");
    for (let i = 0; i < eventCount; i += 1) {
      reader.u8();
      for (let j = 0; j < 3; j += 1) reader.signed();
    }
    if (reader.at !== bytes.length) fail(`${bytes.length - reader.at} trailing bytes`);

    target.seq = seq;
    target.tick = tick;
    target.valid = true;
    this.latest = target;
    this.decoded += 1;
    return target;
  }
}
