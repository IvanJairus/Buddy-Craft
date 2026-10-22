#!/usr/bin/env node
/**
 * T-051 placeholder atlas generator.
 *
 * 256 x 256 RGBA, sixteen 16x16 tiles per row. The first 20 tiles correspond to
 * `data/blocks.ron` in order; the last four are spare. The palette is deliberately
 * flat and low-noise so the PNG stays inside the 30 KB blocking budget.
 */
import { deflateSync } from "node:zlib";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const SIZE = 256;
const TILE = 16;
const TILES = 16;
const pixels = new Uint8Array(SIZE * SIZE * 4);

const palette = [
  [0, 0, 0, 0],         // air
  [125, 128, 127, 255], // stone
  [122, 82, 48, 255],   // dirt
  [94, 156, 74, 255],   // grass
  [217, 193, 122, 255], // sand
  [139, 131, 120, 255], // gravel
  [47, 111, 159, 210],  // water
  [107, 74, 42, 255],   // log
  [63, 122, 58, 255],   // leaves
  [58, 58, 64, 255],    // bedrock
  [93, 102, 112, 255],  // pad floor
  [169, 119, 63, 255],  // crate
  [75, 36, 64, 255],    // core
  [166, 107, 85, 255],  // clay
  [232, 238, 242, 255], // snow
  [78, 107, 61, 255],   // moss
  [76, 77, 85, 255],    // basalt
  [201, 169, 110, 255], // sandstone
  [109, 111, 113, 255], // cobble
  [212, 212, 74, 120],  // marker
  [86, 140, 100, 255],
  [140, 100, 86, 255],
  [110, 120, 150, 255],
  [150, 120, 80, 255],
];

let seed = 0x51a7e;
function rand() {
  seed = (seed * 1664525 + 1013904223) >>> 0;
  return seed;
}

function put(x, y, color) {
  const at = (y * SIZE + x) * 4;
  pixels[at] = color[0];
  pixels[at + 1] = color[1];
  pixels[at + 2] = color[2];
  pixels[at + 3] = color[3] ?? 255;
}

for (let tile = 0; tile < 24; tile += 1) {
  const tx = (tile % TILES) * TILE;
  const ty = Math.floor(tile / TILES) * TILE;
  const base = palette[tile];
  for (let y = 0; y < TILE; y += 1) {
    for (let x = 0; x < TILE; x += 1) {
      const noise = ((rand() >>> 28) & 3) * 4 - 6;
      const shade = (value) => Math.max(0, Math.min(255, value + noise));
      put(tx + x, ty + y, [shade(base[0]), shade(base[1]), shade(base[2]), base[3]]);
    }
  }
  // A few deliberate marks make the tiles readable at 16 px.
  if ([1, 2, 5, 9, 13, 16, 18].includes(tile)) {
    for (let i = 2; i < 14; i += 4) put(tx + i, ty + 11, [base[0] - 18, base[1] - 18, base[2] - 18, 255]);
  }
  if (tile === 3) {
    for (let i = 2; i < 16; i += 5) put(tx + i, ty + 3, [45, 95, 45, 255]);
  }
  if (tile === 7) {
    for (let y = 1; y < 15; y += 3) put(tx + 7, ty + y, [70, 45, 25, 255]);
  }
  if (tile === 10) {
    for (let x = 1; x < 15; x += 2) put(tx + x, ty + 2, [130, 140, 148, 255]);
    for (let y = 1; y < 15; y += 2) put(tx + 2, ty + y, [130, 140, 148, 255]);
  }
  if (tile === 11) {
    for (let x = 1; x < 15; x += 1) {
      put(tx + x, ty + 2, [80, 48, 22, 255]);
      put(tx + x, ty + 13, [80, 48, 22, 255]);
    }
    for (let y = 3; y < 13; y += 1) put(tx + 8, ty + y, [110, 70, 34, 255]);
  }
  if (tile === 12) {
    for (let i = 4; i < 12; i += 1) {
      put(tx + i, ty + 7, [220, 110, 190, 255]);
      put(tx + 7, ty + i, [220, 110, 190, 255]);
    }
  }
  if (tile === 6) {
    for (let x = 1; x < 16; x += 4) put(tx + x, ty + 5, [110, 180, 220, 230]);
  }
  if (tile === 14) {
    for (let x = 1; x < 16; x += 3) put(tx + x, ty + 1, [255, 255, 255, 255]);
  }
}

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
