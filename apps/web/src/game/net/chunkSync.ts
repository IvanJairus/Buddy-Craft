/**
 * Chunk diff streaming (task T-042, plan §6.6/§6.7).
 *
 * The server only sends the cells that differ from generated terrain. This
 * module decodes `S_CHUNK_DIFF`, keeps one version per column, and applies a
 * frame only when its version is newer than the one already held. Diffs are
 * applied nearest-first, and the column under the player is always first, so
 * the pad you are standing next to does not wait behind a distant base.
 *
 * The decoder is defensive at the wire boundary: 1.5 KB and 500 entries are
 * hard limits from §6.7, and a keyframe must cover exactly one 16 x 96 x 16
 * column. A malformed frame throws instead of half-applying.
 *
 * Reapplying the same state is idempotent. A keyframe replaces the column;
 * a later sparse diff updates it, expanding the runs once if necessary.
 */

export const S2C_CHUNK_DIFF = 0x87;
export const MAX_DIFF_BYTES = 1_536;
export const MAX_DIFF_ENTRIES = 500;
export const COLUMN_SIZE = 16;
export const COLUMN_HEIGHT = 96;
export const COLUMN_VOLUME = COLUMN_SIZE * COLUMN_SIZE * COLUMN_HEIGHT;

export interface ChunkPatch {
  cx: number;
  cz: number;
  version: number;
  keyframe: boolean;
  entries: Array<[index: number, block: number]>;
  runs: Array<[block: number, length: number]>;
}

function fail(reason: string): never {
  throw new Error(`chunk diff: ${reason}`);
}

function readVarint(bytes: Uint8Array, at: number): { value: number; next: number } {
  let value = 0;
  let shift = 0;
  let offset = at;
  for (;;) {
    if (offset >= bytes.length) fail("truncated varint");
    const byte = bytes[offset];
    offset += 1;
    value += (byte & 0x7f) * 2 ** shift;
    if ((byte & 0x80) === 0) return { value: value >>> 0, next: offset };
    shift += 7;
    if (shift > 35) fail("varint longer than five bytes");
  }
}

function zigzag(raw: number): number {
  return (raw >>> 1) ^ -(raw & 1);
}

/** Decode one `S_CHUNK_DIFF` frame, including its tag byte. */
export function decodeChunkDiff(bytes: Uint8Array): ChunkPatch {
  if (bytes.length > MAX_DIFF_BYTES) {
    fail(`${bytes.length} bytes exceeds the ${MAX_DIFF_BYTES}-byte budget`);
  }
  if (bytes.length === 0 || bytes[0] !== S2C_CHUNK_DIFF) {
    fail(`expected tag ${S2C_CHUNK_DIFF}`);
  }
  let at = 1;
  const cx = zigzag(readVarint(bytes, at).value);
  at = readVarint(bytes, at).next;
  const cz = zigzag(readVarint(bytes, at).value);
  at = readVarint(bytes, at).next;
  const version = readVarint(bytes, at).value;
  at = readVarint(bytes, at).next;
  const keyframeByte = bytes[at];
  if (keyframeByte === undefined || keyframeByte > 1) fail("keyframe is not a bool");
  const keyframe = keyframeByte === 1;
  at += 1;

  const count = readVarint(bytes, at).value;
  at = readVarint(bytes, at).next;
  if (count > MAX_DIFF_ENTRIES) {
    fail(`${count} entries exceeds the ${MAX_DIFF_ENTRIES}-entry limit`);
  }
  const entries: Array<[number, number]> = [];
  for (let i = 0; i < count; i += 1) {
    const index = readVarint(bytes, at).value;
    at = readVarint(bytes, at).next;
    const block = bytes[at];
    if (block === undefined) fail("truncated entry block");
    at += 1;
    if (index >= COLUMN_VOLUME) fail(`cell index ${index} is outside the column`);
    entries.push([index, block]);
  }

  const runCount = readVarint(bytes, at).value;
  at = readVarint(bytes, at).next;
  if (runCount > COLUMN_VOLUME) fail(`${runCount} runs exceeds the column`);
  const runs: Array<[number, number]> = [];
  let covered = 0;
  for (let i = 0; i < runCount; i += 1) {
    const block = bytes[at];
    if (block === undefined) fail("truncated run block");
    at += 1;
    const length = readVarint(bytes, at).value;
    at = readVarint(bytes, at).next;
    if (length === 0) fail("run has zero length");
    covered += length;
    if (covered > COLUMN_VOLUME) fail("runs overrun the column");
    runs.push([block, length]);
  }
  if (keyframe && covered !== COLUMN_VOLUME) {
    fail(`keyframe covers ${covered} cells, expected ${COLUMN_VOLUME}`);
  }
  if (at !== bytes.length) fail(`${bytes.length - at} trailing bytes`);
  if (cx < -32768 || cx > 32767 || cz < -32768 || cz > 32767) fail("column out of range");
  return { cx, cz, version, keyframe, entries, runs };
}

interface ColumnState {
  version: number;
  /** Null while the column is still the compact run form of a keyframe. */
  cells: Map<number, number> | null;
  runs: Array<[number, number]> | null;
}

function keyOf(cx: number, cz: number): string {
  return `${cx},${cz}`;
}

function expand(state: ColumnState): Map<number, number> {
  if (state.cells !== null) return state.cells;
  const cells = new Map<number, number>();
  let index = 0;
  for (const [block, length] of state.runs ?? []) {
    for (let i = 0; i < length; i += 1) {
      if (block !== 0) cells.set(index, block);
      index += 1;
    }
  }
  state.cells = cells;
  state.runs = null;
  return cells;
}

export class ChunkSync {
  private readonly queue: Array<{ patch: ChunkPatch; frame: Uint8Array | null }> = [];
  private readonly columns = new Map<string, ColumnState>();

  /** Queue a decoded patch. `flush` decides when it reaches the world. */
  enqueue(patch: ChunkPatch, frame: Uint8Array | null = null): void {
    this.queue.push({ patch, frame });
  }

  reset(): void {
    this.queue.length = 0;
    this.columns.clear();
  }

  /** Decode and queue a wire frame, retaining the bytes for the wasm sink. */
  enqueueFrame(frame: Uint8Array): ChunkPatch {
    const patch = decodeChunkDiff(frame);
    this.enqueue(patch, frame.slice());
    return patch;
  }

  get pending(): number {
    return this.queue.length;
  }

  version(cx: number, cz: number): number {
    return this.columns.get(keyOf(cx, cz))?.version ?? 0;
  }

  /**
   * Apply queued patches nearest-first. Returns the patches that passed the
   * version check; stale and duplicate frames are dropped.
   */
  flush(
    playerX: number,
    playerZ: number,
    sink: (patch: ChunkPatch, frame: Uint8Array | null) => void,
    limit = Number.POSITIVE_INFINITY,
  ): ChunkPatch[] {
    const pcx = Math.floor(playerX / COLUMN_SIZE);
    const pcz = Math.floor(playerZ / COLUMN_SIZE);
    const scored = this.queue.map((entry, order) => {
      const dx = entry.patch.cx - pcx;
      const dz = entry.patch.cz - pcz;
      return {
        entry,
        order,
        distance: dx * dx + dz * dz,
      };
    });
    scored.sort(
      (a, b) =>
        a.distance - b.distance ||
        a.entry.patch.cx - b.entry.patch.cx ||
        a.entry.patch.cz - b.entry.patch.cz ||
        a.order - b.order,
    );

    const applied: ChunkPatch[] = [];
    let consumed = 0;
    for (const entry of scored) {
      if (applied.length >= limit) break;
      consumed += 1;
      if (!this.apply(entry.entry.patch)) continue;
      sink(entry.entry.patch, entry.entry.frame);
      applied.push(entry.entry.patch);
    }
    this.queue.splice(0, consumed);
    return applied;
  }

  /** The current block at a local cell, or 0 for air. */
  cell(cx: number, cz: number, index: number): number {
    const state = this.columns.get(keyOf(cx, cz));
    if (state === undefined) return 0;
    if (state.cells !== null) return state.cells.get(index) ?? 0;
    let at = 0;
    for (const [block, length] of state.runs ?? []) {
      at += length;
      if (index < at) return block;
    }
    return 0;
  }

  private apply(patch: ChunkPatch): boolean {
    const key = keyOf(patch.cx, patch.cz);
    const previous = this.columns.get(key);
    if (previous !== undefined && patch.version <= previous.version) {
      return false;
    }
    if (patch.keyframe) {
      this.columns.set(key, {
        version: patch.version,
        cells: null,
        runs: patch.runs.map(([block, length]) => [block, length]),
      });
      return true;
    }
    const state =
      previous ?? { version: 0, cells: new Map<number, number>(), runs: null };
    const cells = expand(state);
    for (const [index, block] of patch.entries) {
      if (block === 0) {
        cells.delete(index);
      } else {
        cells.set(index, block);
      }
    }
    state.version = patch.version;
    this.columns.set(key, state);
    return true;
  }
}
