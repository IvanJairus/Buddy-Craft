/**
 * T-042 acceptance specs: wire decode, per-column versions, nearest-first
 * application and keyframe replacement. These are browser-side unit specs;
 * the server side is `edit_then_join` in `crates/room/src/edit.rs`.
 */

import { expect, test } from "@playwright/test";
import {
  COLUMN_VOLUME,
  ChunkSync,
  MAX_DIFF_BYTES,
  decodeChunkDiff,
  type ChunkPatch,
} from "../src/game/net/chunkSync";

const SPARSE_FRAME = new Uint8Array([
  0x87, 0x06, 0x03, 0x07, 0x00, 0x01, 0x05, 0x13, 0x00,
]);
const KEYFRAME_FRAME = new Uint8Array([
  0x87, 0x00, 0x00, 0x02, 0x01, 0x00, 0x02, 0x00, 0xff, 0xbf, 0x01, 0x13, 0x01,
]);

function sparse(cx: number, cz: number, version: number, index: number, block: number): ChunkPatch {
  return {
    cx,
    cz,
    version,
    keyframe: false,
    entries: [[index, block]],
    runs: [],
  };
}

test("the decoder reads the Rust wire vector and refuses oversized frames", () => {
  expect(decodeChunkDiff(SPARSE_FRAME)).toEqual({
    cx: 3,
    cz: -2,
    version: 7,
    keyframe: false,
    entries: [[5, 19]],
    runs: [],
  });
  expect(() => decodeChunkDiff(new Uint8Array(MAX_DIFF_BYTES + 1))).toThrow(/budget/);
  const keyframe = decodeChunkDiff(KEYFRAME_FRAME);
  expect(keyframe.keyframe).toBe(true);
  expect(keyframe.entries).toEqual([]);
  expect(keyframe.runs).toEqual([
    [0, 24_575],
    [19, 1],
  ]);
});

test("only a newer version reaches the sink", () => {
  const sync = new ChunkSync();
  const applied: number[] = [];
  sync.enqueue(sparse(1, 1, 2, 4, 19));
  sync.enqueue(sparse(1, 1, 1, 4, 0));
  sync.enqueue(sparse(1, 1, 2, 4, 0));
  sync.flush(0, 0, (patch) => applied.push(patch.version));
  expect(applied).toEqual([2]);
  expect(sync.cell(1, 1, 4)).toBe(19);
  expect(sync.version(1, 1)).toBe(2);
});

test("the column under the player lands first, then the nearest", () => {
  const sync = new ChunkSync();
  const order: number[] = [];
  sync.enqueue(sparse(10, 10, 1, 0, 19));
  sync.enqueue(sparse(6, 5, 1, 0, 19));
  sync.enqueue(sparse(5, 5, 1, 0, 19));
  sync.flush(80, 80, (patch) => order.push(patch.cx));
  expect(order).toEqual([5, 6, 10]);
});

test("a keyframe replaces the column and a later diff updates it", () => {
  const sync = new ChunkSync();
  sync.enqueue(sparse(2, 2, 1, 0, 19));
  sync.flush(0, 0, () => {});
  expect(sync.cell(2, 2, 0)).toBe(19);

  sync.enqueue({
    cx: 2,
    cz: 2,
    version: 2,
    keyframe: true,
    entries: [],
    runs: [[0, COLUMN_VOLUME]],
  });
  sync.flush(0, 0, () => {});
  expect(sync.cell(2, 2, 0)).toBe(0);

  sync.enqueue(sparse(2, 2, 3, 5, 19));
  sync.flush(0, 0, () => {});
  expect(sync.cell(2, 2, 5)).toBe(19);
  expect(sync.version(2, 2)).toBe(3);
});
