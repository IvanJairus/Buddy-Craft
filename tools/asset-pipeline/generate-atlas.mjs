#!/usr/bin/env node
/**
 * T-051 block atlas.
 *
 * 256 x 256 RGBA, sixteen 16x16 tiles per row. Tiles 0..19 follow
 * `data/blocks.ron`; 20 is a log end, 21 a grass side, 22 a crate top and 23 a
 * core top. The face remap in `chunkManager.ts` uses 20 and 21. The output is
 * deterministic and stays under the 30 KB blocking budget.
 */

import { deflateSync } from "node:zlib";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const SIZE = 256;
const TILE = 16;
const TILES = 16;
const pixels = new Uint8Array(SIZE * SIZE * 4);

const clamp = (value) => Math.max(0, Math.min(255, Math.round(value)));

function hash(x, y, salt) {
  let h = Math.imul(x ^ salt, 374761393) ^ Math.imul(y + salt, 668265263);
  h = Math.imul(h ^ (h >>> 13), 1274126177);
  return ((h ^ (h >>> 16)) >>> 0) / 4294967295;
}

function put(tile, x, y, color, alpha = 255) {
  if (x < 0 || y < 0 || x >= TILE || y >= TILE) return;
  // PNG row 0 lands on texture v=0, which is the bottom of a vertical face;
  // flip inside the tile so authored y=0 is the world top.
  const row = TILE - 1 - y;
  const at = ((Math.floor(tile / TILES) * TILE + row) * SIZE + (tile % TILES) * TILE + x) * 4;
  pixels[at] = clamp(color[0]);
  pixels[at + 1] = clamp(color[1]);
  pixels[at + 2] = clamp(color[2]);
  pixels[at + 3] = alpha;
}

function shade(color, delta) {
  return [color[0] + delta, color[1] + delta, color[2] + delta];
}

function base(tile, color, variance, alpha = 255) {
  for (let y = 0; y < TILE; y += 1) {
    for (let x = 0; x < TILE; x += 1) {
      const n = (hash(x, y, tile) - 0.5) * 2;
      put(tile, x, y, shade(color, n * variance), alpha);
    }
  }
}

function rect(tile, x0, y0, x1, y1, color, alpha = 255) {
  for (let y = y0; y <= y1; y += 1) {
    for (let x = x0; x <= x1; x += 1) put(tile, x, y, color, alpha);
  }
}

function line(tile, x0, y0, x1, y1, color, alpha = 255) {
  const dx = Math.abs(x1 - x0);
  const dy = -Math.abs(y1 - y0);
  const sx = x0 < x1 ? 1 : -1;
  const sy = y0 < y1 ? 1 : -1;
  let error = dx + dy;
  let x = x0;
  let y = y0;
  for (;;) {
    put(tile, x, y, color, alpha);
    if (x === x1 && y === y1) break;
    const e2 = 2 * error;
    if (e2 >= dy) {
      error += dy;
      x += sx;
    }
    if (e2 <= dx) {
      error += dx;
      y += sy;
    }
  }
}

function speckle(tile, count, salt, color, min = 0, max = TILE - 1) {
  for (let i = 0; i < count; i += 1) {
    const x = min + Math.floor(hash(i, salt, tile) * (max - min + 1));
    const y = min + Math.floor(hash(salt, i, tile + 7) * (max - min + 1));
    put(tile, x, y, color);
  }
}

function stone() {
  base(1, [120, 122, 124], 10);
  line(1, 2, 3, 6, 8, [96, 98, 101]);
  line(1, 6, 8, 5, 13, [96, 98, 101]);
  line(1, 10, 1, 13, 6, [104, 106, 109]);
  line(1, 13, 6, 11, 11, [104, 106, 109]);
  speckle(1, 7, 2, [148, 150, 152]);
  speckle(1, 5, 9, [102, 104, 106]);
}

function dirt() {
  base(2, [121, 82, 50], 12);
  for (const [x, y] of [[2, 4], [7, 10], [12, 3], [4, 13], [10, 6]]) {
    rect(2, x, y, x + 1, y + 1, [100, 64, 38]);
  }
  speckle(2, 9, 3, [146, 105, 68]);
  speckle(2, 5, 11, [88, 56, 34]);
  line(2, 13, 12, 14, 15, [92, 58, 34]);
}

function grassTop() {
  base(3, [88, 150, 70], 9);
  for (let i = 0; i < 18; i += 1) {
    const x = Math.floor(hash(i, 1, 3) * TILE);
    const y = Math.floor(hash(1, i, 3) * TILE);
    const light = hash(i, i, 13) > 0.55;
    put(3, x, y, light ? [126, 184, 92] : [66, 120, 54]);
    if (hash(x, y, 21) > 0.6) put(3, x, y - 1, light ? [146, 200, 108] : [76, 134, 60]);
  }
  for (const [x, y] of [[4, 5], [12, 11]]) {
    put(3, x, y, [240, 220, 120]);
    put(3, x + 1, y, [255, 240, 170]);
  }
  speckle(3, 4, 17, [54, 104, 46]);
}

function sand() {
  base(4, [214, 190, 124], 8);
  speckle(4, 10, 4, [196, 170, 108]);
  speckle(4, 8, 14, [232, 210, 150]);
  for (const y of [4, 10]) {
    for (let x = 1; x < 14; x += 1) {
      if (hash(x, y, 4) > 0.45) put(4, x, y, [202, 176, 112]);
    }
  }
}

function gravel() {
  base(5, [126, 120, 112], 12);
  for (const [x, y, w, h, c] of [
    [1, 2, 2, 2, [156, 148, 138]],
    [6, 1, 2, 3, [148, 140, 128]],
    [11, 3, 3, 2, [166, 156, 144]],
    [2, 8, 3, 3, [142, 134, 122]],
    [8, 9, 2, 2, [162, 152, 140]],
    [12, 11, 2, 3, [150, 142, 132]],
  ]) {
    rect(5, x, y, x + w - 1, y + h - 1, c);
    rect(5, x, y + h - 1, x + w - 1, y + h - 1, shade(c, -28));
  }
  speckle(5, 6, 5, [92, 88, 84]);
}

function water() {
  base(6, [48, 112, 160], 7, 210);
  for (const y of [3, 8, 12]) {
    for (let x = 0; x < TILE; x += 1) {
      if (hash(x, y, 6) > 0.3) put(6, x, y, [92, 160, 202], 210);
    }
  }
  speckle(6, 5, 16, [196, 226, 244], 1, 14);
}

function logSide() {
  base(7, [104, 72, 42], 8);
  for (const x of [2, 5, 9, 13]) {
    for (let y = 0; y < TILE; y += 1) {
      if (hash(x, y, 7) > 0.15) put(7, x, y, [78, 51, 28]);
    }
  }
  for (const x of [3, 11]) {
    for (let y = 2; y < 13; y += 4) put(7, x, y, [128, 92, 56]);
  }
  put(7, 8, 6, [62, 40, 22]);
  put(7, 7, 7, [142, 102, 62]);
  put(7, 8, 8, [62, 40, 22]);
}

function leaves() {
  base(8, [56, 116, 52], 12);
  for (let i = 0; i < 9; i += 1) {
    const x = Math.floor(hash(i, 8, 8) * TILE);
    const y = Math.floor(hash(8, i, 8) * TILE);
    const light = hash(i, i, 9) > 0.4;
    const c = light ? [86, 152, 68] : [40, 90, 40];
    rect(8, x, y, x + 1, y + 1, c);
    if (hash(x, y, 10) > 0.5) put(8, x + 2, y + 1, shade(c, 14));
  }
  speckle(8, 6, 12, [32, 74, 34]);
}

function bedrock() {
  base(9, [50, 52, 56], 12);
  line(9, 1, 4, 6, 3, [34, 36, 40]);
  line(9, 6, 3, 9, 7, [34, 36, 40]);
  line(9, 9, 7, 14, 10, [34, 36, 40]);
  rect(9, 2, 11, 5, 13, [70, 72, 78]);
  rect(9, 10, 2, 13, 5, [66, 68, 74]);
  speckle(9, 5, 9, [84, 86, 92]);
}

function padFloor() {
  base(10, [84, 94, 106], 5);
  for (const at of [0, 8]) {
    for (let i = 0; i < TILE; i += 1) {
      put(10, at, i, [62, 72, 84]);
      put(10, i, at, [62, 72, 84]);
      put(10, at + 1, i, [96, 106, 118]);
    }
  }
  for (const [x, y] of [[2, 2], [10, 2], [2, 10], [10, 10]]) {
    put(10, x, y, [126, 136, 148]);
    put(10, x + 1, y, [110, 120, 132]);
  }
  speckle(10, 4, 10, [72, 82, 94]);
}

function crate() {
  base(11, [162, 112, 58], 8);
  for (const y of [5, 10]) {
    for (let x = 0; x < TILE; x += 1) put(11, x, y, [96, 62, 30]);
  }
  line(11, 1, 1, 14, 14, [118, 78, 38]);
  line(11, 14, 1, 1, 14, [118, 78, 38]);
  line(11, 2, 1, 15, 14, [138, 92, 46]);
  line(11, 13, 1, 0, 14, [138, 92, 46]);
  for (const [x, y] of [[1, 1], [14, 1], [1, 14], [14, 14]]) {
    put(11, x, y, [196, 198, 202]);
    put(11, x + (x < 8 ? 1 : -1), y, [160, 162, 166]);
  }
  rect(11, 0, 0, 15, 0, [82, 52, 26]);
  rect(11, 0, 15, 15, 15, [82, 52, 26]);
}

function core() {
  base(12, [72, 32, 62], 8);
  for (let x = 0; x < TILE; x += 1) {
    put(12, x, 0, [48, 20, 42]);
    put(12, x, 15, [48, 20, 42]);
  }
  line(12, 8, 2, 13, 8, [220, 90, 180]);
  line(12, 13, 8, 8, 14, [220, 90, 180]);
  line(12, 8, 14, 3, 8, [220, 90, 180]);
  line(12, 3, 8, 8, 2, [220, 90, 180]);
  rect(12, 7, 7, 8, 8, [255, 170, 230]);
  speckle(12, 5, 12, [255, 210, 240]);
}

function clay() {
  base(13, [162, 104, 84], 11);
  for (const [x, y] of [[2, 3], [9, 2], [5, 9], [11, 11]]) {
    rect(13, x, y, x + 1, y + 1, [142, 88, 70]);
  }
  speckle(13, 7, 13, [184, 124, 100]);
  speckle(13, 5, 23, [126, 76, 60]);
}

function snow() {
  base(14, [236, 242, 246], 4);
  for (const [x, y] of [[2, 3], [9, 5], [5, 11], [12, 12]]) {
    rect(14, x, y, x + 1, y, [214, 228, 240]);
  }
  speckle(14, 7, 14, [255, 255, 255]);
  speckle(14, 3, 24, [206, 220, 234]);
}

function moss() {
  base(15, [74, 104, 58], 10);
  for (const [x, y] of [[2, 2], [7, 4], [11, 8], [4, 11]]) {
    rect(15, x, y, x + 2, y + 1, [96, 132, 70]);
  }
  rect(15, 12, 2, 13, 3, [98, 96, 92]);
  rect(15, 3, 7, 4, 8, [104, 102, 98]);
  speckle(15, 6, 15, [54, 82, 44]);
}

function basalt() {
  base(16, [66, 68, 74], 9);
  for (const x of [3, 8, 12]) {
    for (let y = 0; y < TILE; y += 1) {
      if (hash(x, y, 16) > 0.2) put(16, x, y, [48, 50, 56]);
      if (hash(y, x, 16) > 0.75) put(16, x + 1, y, [84, 86, 92]);
    }
  }
  line(16, 1, 12, 5, 14, [42, 44, 50]);
  speckle(16, 5, 26, [92, 94, 100]);
}

function sandstone() {
  base(17, [196, 166, 110], 7);
  for (const y of [3, 7, 12]) {
    for (let x = 0; x < TILE; x += 1) {
      if (hash(x, y, 17) > 0.2) put(17, x, y, [172, 142, 90]);
    }
  }
  for (let x = 0; x < TILE; x += 1) {
    if (hash(x, 5, 17) > 0.3) put(17, x, 5, [216, 190, 136]);
  }
  speckle(17, 5, 27, [146, 118, 78]);
}

function cobble() {
  base(18, [96, 98, 100], 8);
  const stones = [
    [1, 1, 5, 5, [134, 136, 138]],
    [8, 1, 13, 5, [124, 126, 128]],
    [1, 8, 5, 13, [142, 144, 146]],
    [7, 7, 12, 12, [130, 132, 134]],
    [13, 9, 15, 14, [120, 122, 124]],
  ];
  for (const [x0, y0, x1, y1, c] of stones) {
    rect(18, x0 + 1, y0, x1 - 1, y1, c);
    rect(18, x0, y0 + 1, x1, y1 - 1, c);
    rect(18, x0, y1, x1, y1, shade(c, -26));
  }
  speckle(18, 6, 18, [168, 170, 172]);
}

function marker() {
  base(19, [208, 198, 76], 6, 120);
  for (const y of [4, 9]) {
    line(19, 3, y + 2, 8, y - 2, [255, 236, 130], 170);
    line(19, 8, y - 2, 12, y + 2, [255, 236, 130], 170);
  }
  rect(19, 0, 0, 15, 0, [236, 224, 96], 160);
  rect(19, 0, 15, 15, 15, [236, 224, 96], 160);
}

function logTop() {
  base(20, [158, 124, 78], 6);
  for (let y = 0; y < TILE; y += 1) {
    for (let x = 0; x < TILE; x += 1) {
      const d = Math.max(Math.abs(x - 7.5), Math.abs(y - 7.5));
      if (d > 6.7) put(20, x, y, [92, 62, 34]);
      else if (Math.abs(d - 5.2) < 0.55) put(20, x, y, [126, 92, 52]);
      else if (Math.abs(d - 3.4) < 0.55) put(20, x, y, [132, 98, 58]);
      else if (Math.abs(d - 1.5) < 0.55) put(20, x, y, [124, 90, 50]);
    }
  }
  put(20, 7, 7, [86, 58, 34]);
  put(20, 8, 8, [86, 58, 34]);
  speckle(20, 4, 20, [176, 142, 94]);
}

function grassSide() {
  base(21, [121, 82, 50], 10);
  for (let x = 0; x < TILE; x += 1) {
    const depth = 3 + Math.floor(hash(x, 1, 21) * 3);
    for (let y = 0; y < depth; y += 1) {
      const light = hash(x, y, 22) > 0.6;
      put(21, x, y, light ? [110, 172, 80] : [76, 134, 58]);
    }
    put(21, x, depth, [58, 104, 46]);
    if (hash(x, depth, 23) > 0.7) put(21, x, depth + 1, [70, 96, 48]);
  }
  speckle(21, 5, 31, [146, 105, 68]);
}

function crateTop() {
  base(22, [156, 108, 56], 6);
  rect(22, 0, 0, 15, 1, [112, 74, 36]);
  rect(22, 0, 14, 15, 15, [112, 74, 36]);
  rect(22, 0, 0, 1, 15, [112, 74, 36]);
  rect(22, 14, 0, 15, 15, [112, 74, 36]);
  for (let i = 2; i < 14; i += 4) line(22, i, 2, i + 3, 13, [136, 92, 46]);
  for (const [x, y] of [[3, 3], [12, 3], [3, 12], [12, 12]]) put(22, x, y, [192, 194, 198]);
}

function coreTop() {
  base(23, [64, 28, 56], 6);
  line(23, 8, 2, 13, 8, [210, 86, 170]);
  line(23, 13, 8, 8, 14, [210, 86, 170]);
  line(23, 8, 14, 3, 8, [210, 86, 170]);
  line(23, 3, 8, 8, 2, [210, 86, 170]);
  rect(23, 7, 7, 8, 8, [255, 190, 236]);
  speckle(23, 6, 23, [150, 60, 120]);
}

stone();
dirt();
grassTop();
sand();
gravel();
water();
logSide();
leaves();
bedrock();
padFloor();
crate();
core();
clay();
snow();
moss();
basalt();
sandstone();
cobble();
marker();
logTop();
grassSide();
crateTop();
coreTop();

function chunk(type, data) {
  const name = Buffer.from(type, "ascii");
  const length = Buffer.alloc(4);
  length.writeUInt32BE(data.length, 0);
  const body = Buffer.concat([name, data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body) >>> 0, 0);
  return Buffer.concat([length, body, crc]);
}

let crcTable = null;
function crc32(buffer) {
  if (crcTable === null) {
    crcTable = new Uint32Array(256);
    for (let n = 0; n < 256; n += 1) {
      let c = n;
      for (let k = 0; k < 8; k += 1) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
      crcTable[n] = c >>> 0;
    }
  }
  let crc = 0xffffffff;
  for (const byte of buffer) crc = crcTable[(crc ^ byte) & 0xff] ^ (crc >>> 8);
  return (crc ^ 0xffffffff) >>> 0;
}

const ihdr = Buffer.alloc(13);
ihdr.writeUInt32BE(SIZE, 0);
ihdr.writeUInt32BE(SIZE, 4);
ihdr[8] = 8;
ihdr[9] = 6;
const scanlines = Buffer.alloc((SIZE * 4 + 1) * SIZE);
for (let y = 0; y < SIZE; y += 1) {
  scanlines[y * (SIZE * 4 + 1)] = 0;
  Buffer.from(pixels.buffer, pixels.byteOffset + y * SIZE * 4, SIZE * 4).copy(
    scanlines,
    y * (SIZE * 4 + 1) + 1,
  );
}
const png = Buffer.concat([
  Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
  chunk("IHDR", ihdr),
  chunk("IDAT", deflateSync(scanlines, { level: 9 })),
  chunk("IEND", Buffer.alloc(0)),
]);
const target = join(
  dirname(fileURLToPath(import.meta.url)),
  "..",
  "..",
  "apps",
  "web",
  "public",
  "assets",
  "atlas.png",
);
if (png.length > 30 * 1024) {
  console.error(`atlas is ${png.length} bytes, over the 30 KB budget`);
  process.exit(1);
}
mkdirSync(dirname(target), { recursive: true });
writeFileSync(target, png);
console.log(`wrote ${target} (${png.length} bytes)`);
