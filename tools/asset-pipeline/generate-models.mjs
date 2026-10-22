#!/usr/bin/env node
/**
 * T-051 low-poly model generator.
 *
 * Nine GLB meshes: six buddy roles and three player silhouettes. Each is a
 * merged set of boxes with per-box vertex colours, no textures, no materials
 * and no animation. The budget is strict: under 400 triangles per mesh and
 * 40 KB for the set, so the files stay easily inspectable and can be regenerated
 * deterministically from this script.
 */

import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const OUTPUT = join(
  dirname(fileURLToPath(import.meta.url)),
  "..",
  "..",
  "apps",
  "web",
  "public",
  "assets",
  "models",
);
const MAX_TRIS = 400;
const MAX_TOTAL_BYTES = 40 * 1024;

const SKIN = 0xd6a07a;
const DARK = 0x2b2f35;
const WOOD = 0x6b4a2b;

function box(center, size, color = null) {
  return { center, size, color };
}

function humanoid(body, accent, options = {}) {
  const torso = options.torso ?? [0.46, 0.5, 0.26];
  const head = options.head ?? 0.3;
  const limb = options.limb ?? 0.16;
  const boxes = [
    box([-0.12, 0.225, 0], [0.16, 0.45, 0.18], DARK),
    box([0.12, 0.225, 0], [0.16, 0.45, 0.18], DARK),
    box([0, 0.73, 0], torso, body),
    box([0, 0.73 + torso[1] * 0.28, 0], [torso[0] + 0.04, 0.08, torso[2] + 0.02], accent),
    box([0, 1.06, 0], [head, head, head], SKIN),
    box([-0.31, 0.78, 0], [limb, 0.44, 0.16], body),
    box([0.31, 0.78, 0], [limb, 0.44, 0.16], body),
    box([-0.31, 0.54, 0], [limb + 0.02, 0.1, 0.17], accent),
    box([0.31, 0.54, 0], [limb + 0.02, 0.1, 0.17], accent),
  ];
  return boxes;
}

function swordsman() {
  return [
    ...humanoid(0x4f6f8f, 0xc8d2dc),
    box([0.43, 1.0, 0.02], [0.07, 0.72, 0.07], 0xd8dde3),
    box([0.43, 0.67, 0.02], [0.28, 0.07, 0.1], 0x8a6a3a),
    box([0.43, 0.6, 0.02], [0.06, 0.18, 0.06], 0x3a2e22),
    box([-0.43, 0.75, 0], [0.08, 0.5, 0.36], 0x7a4a32),
  ];
}

function archer() {
  return [
    ...humanoid(0x4f7a4a, 0x8a6a3a),
    box([0.4, 0.85, 0.08], [0.05, 0.86, 0.05], WOOD),
    box([0.4, 1.26, 0.12], [0.05, 0.08, 0.12], WOOD),
    box([0.4, 0.44, 0.12], [0.05, 0.08, 0.12], WOOD),
    box([-0.17, 1.03, -0.2], [0.14, 0.42, 0.14], 0x3f5a35),
    box([-0.17, 1.28, -0.2], [0.05, 0.12, 0.05], 0xd8d2b0),
    box([0, 1.27, 0], [0.34, 0.1, 0.34], 0x3f5a35),
  ];
}

function mage() {
  return [
    ...humanoid(0x6a4f8f, 0xd0b0ff, { torso: [0.44, 0.54, 0.28] }),
    box([0, 0.36, 0], [0.52, 0.3, 0.34], 0x4a3566),
    box([0.45, 0.82, 0], [0.06, 1.35, 0.06], WOOD),
    box([0.45, 1.55, 0], [0.17, 0.17, 0.17], 0xb98cff),
    box([0, 1.31, 0], [0.44, 0.06, 0.44], 0x3f2a5a),
    box([0, 1.4, 0], [0.3, 0.14, 0.3], 0x5a3f7a),
    box([0, 1.52, 0], [0.22, 0.14, 0.22], 0x5a3f7a),
    box([0, 1.62, 0], [0.14, 0.12, 0.14], 0x5a3f7a),
  ];
}

function healer() {
  return [
    ...humanoid(0xd8d0b8, 0x6aa07a, { torso: [0.48, 0.52, 0.3] }),
    box([0, 0.38, 0], [0.54, 0.34, 0.36], 0xe4ddc9),
    box([0.43, 0.82, 0], [0.06, 1.36, 0.06], WOOD),
    box([0.43, 1.3, 0], [0.34, 0.07, 0.07], 0x6aa07a),
    box([0.43, 1.38, 0], [0.07, 0.24, 0.07], 0x6aa07a),
    box([0, 1.28, 0], [0.34, 0.1, 0.34], 0xe4ddc9),
  ];
}

function berserker() {
  return [
    ...humanoid(0xa05038, 0xd0c0a0, { torso: [0.6, 0.55, 0.34], head: 0.34, limb: 0.2 }),
    box([-0.4, 0.98, 0], [0.22, 0.18, 0.32], 0x7a3a28),
    box([0.4, 0.98, 0], [0.22, 0.18, 0.32], 0x7a3a28),
    box([0.55, 0.82, 0], [0.08, 1.18, 0.08], 0x3a2a22),
    box([0.71, 1.34, 0], [0.38, 0.3, 0.1], 0xb8b2a6),
    box([-0.16, 1.3, 0], [0.08, 0.16, 0.08], 0xe0d6b8),
    box([0.16, 1.3, 0], [0.08, 0.16, 0.08], 0xe0d6b8),
  ];
}

function sniper() {
  return [
    ...humanoid(0x59626a, 0x9aa8b2, { torso: [0.42, 0.5, 0.25] }),
    box([0, 0.88, -0.22], [0.1, 0.14, 0.34], 0x33383d),
    box([0, 0.92, 0.55], [0.055, 0.055, 0.72], 0x202428),
    box([0, 1.0, 0.2], [0.09, 0.08, 0.2], 0x3a4248),
    box([0, 1.27, 0], [0.34, 0.1, 0.34], 0x33383d),
    box([-0.2, 0.55, -0.24], [0.12, 0.34, 0.1], 0x6a5a3a),
  ];
}

function playerScout() {
  return [
    ...humanoid(0x4f8fd6, 0x9fc4ee, { torso: [0.4, 0.46, 0.24], limb: 0.14 }),
    box([0, 0.76, 0.16], [0.3, 0.06, 0.05], 0xd6e8ff),
    box([0, 0.82, -0.19], [0.28, 0.24, 0.14], 0x355f8c),
    box([0, 1.24, 0], [0.2, 0.05, 0.2], 0x9fc4ee),
  ];
}

function playerGuard() {
  return [
    ...humanoid(0xc08a3a, 0xe0c080, { torso: [0.56, 0.52, 0.32], head: 0.32, limb: 0.18 }),
    box([0, 0.78, 0.19], [0.4, 0.26, 0.06], 0xf0dcc0),
    box([-0.42, 0.96, 0], [0.16, 0.18, 0.3], 0x8a5f22),
    box([0.42, 0.96, 0], [0.16, 0.18, 0.3], 0x8a5f22),
    box([0, 1.25, 0], [0.36, 0.06, 0.32], 0xe0c080),
  ];
}

function playerRaider() {
  return [
    ...humanoid(0xb0534a, 0xe0a090, { torso: [0.5, 0.5, 0.28] }),
    box([0, 1.3, 0], [0.38, 0.12, 0.38], 0x5a2a26),
    box([-0.4, 1.0, 0], [0.18, 0.16, 0.3], 0x7a3630),
    box([0.43, 0.92, 0.02], [0.07, 0.55, 0.07], 0xd8c0b0),
    box([0.43, 0.66, 0.02], [0.26, 0.06, 0.1], 0x6a4a2a),
  ];
}

const MODELS = [
  { file: "buddy_swordsman.glb", name: "BuddySwordsman", boxes: swordsman() },
  { file: "buddy_archer.glb", name: "BuddyArcher", boxes: archer() },
  { file: "buddy_mage.glb", name: "BuddyMage", boxes: mage() },
  { file: "buddy_healer.glb", name: "BuddyHealer", boxes: healer() },
  { file: "buddy_berserker.glb", name: "BuddyBerserker", boxes: berserker() },
  { file: "buddy_sniper.glb", name: "BuddySniper", boxes: sniper() },
  { file: "player_scout.glb", name: "PlayerScout", boxes: playerScout() },
  { file: "player_guard.glb", name: "PlayerGuard", boxes: playerGuard() },
  { file: "player_raider.glb", name: "PlayerRaider", boxes: playerRaider() },
];

// Box corners in the order expected by FACES below: (-x,-y,-z) first.
const CORNERS = [
  [-1, -1, -1],
  [1, -1, -1],
  [1, 1, -1],
  [-1, 1, -1],
  [-1, -1, 1],
  [1, -1, 1],
  [1, 1, 1],
  [-1, 1, 1],
];
const FACES = [
  [1, 2, 6, 1, 6, 5],
  [4, 7, 3, 4, 3, 0],
  [3, 7, 6, 3, 6, 2],
  [0, 1, 5, 0, 5, 4],
  [5, 6, 7, 5, 7, 4],
  [1, 0, 3, 1, 3, 2],
];

function colorsOf(hex) {
  return [(hex >> 16) & 0xff, (hex >> 8) & 0xff, hex & 0xff];
}

function meshData(boxes) {
  const positions = [];
  const colors = [];
  const indices = [];
  for (const part of boxes) {
    const [cx, cy, cz] = part.center;
    const [sx, sy, sz] = part.size;
    const base = positions.length / 3;
    for (const corner of CORNERS) {
      positions.push(cx + (corner[0] * sx) / 2, cy + (corner[1] * sy) / 2, cz + (corner[2] * sz) / 2);
      colors.push(...colorsOf(part.color));
    }
    for (const face of FACES) {
      for (const index of face) indices.push(base + index);
    }
  }
  return { positions, colors, indices };
}

function pad4(buffer) {
  const remainder = buffer.length % 4;
  return remainder === 0 ? buffer : Buffer.concat([buffer, Buffer.alloc(4 - remainder)]);
}

function buildGlb(name, boxes) {
  const data = meshData(boxes);
  const vertexCount = data.positions.length / 3;
  const positionBytes = Buffer.from(new Float32Array(data.positions).buffer);
  const colorBytes = Buffer.from(Uint8Array.from(data.colors));
  const indexBytes = Buffer.from(new Uint16Array(data.indices).buffer);
  const positionView = pad4(positionBytes);
  const colorView = pad4(colorBytes);
  const indexView = pad4(indexBytes);
  const bin = Buffer.concat([positionView, colorView, indexView]);

  let min = [Infinity, Infinity, Infinity];
  let max = [-Infinity, -Infinity, -Infinity];
  for (let i = 0; i < data.positions.length; i += 3) {
    for (let axis = 0; axis < 3; axis += 1) {
      min[axis] = Math.min(min[axis], data.positions[i + axis]);
      max[axis] = Math.max(max[axis], data.positions[i + axis]);
    }
  }

  const json = {
    asset: { version: "2.0", generator: "cratebound-asset-pipeline" },
    scene: 0,
    scenes: [{ nodes: [0] }],
    nodes: [{ mesh: 0, name }],
    meshes: [
      {
        name,
        primitives: [
          {
            attributes: { POSITION: 0, COLOR_0: 1 },
            indices: 2,
            mode: 4,
          },
        ],
      },
    ],
    buffers: [{ byteLength: bin.length }],
    bufferViews: [
      { buffer: 0, byteOffset: 0, byteLength: positionBytes.length, target: 34962 },
      {
        buffer: 0,
        byteOffset: positionView.length,
        byteLength: colorBytes.length,
        target: 34962,
      },
      {
        buffer: 0,
        byteOffset: positionView.length + colorView.length,
        byteLength: indexBytes.length,
        target: 34963,
      },
    ],
    accessors: [
      {
        bufferView: 0,
        componentType: 5126,
        count: vertexCount,
        type: "VEC3",
        min,
        max,
      },
      {
        bufferView: 1,
        componentType: 5121,
        normalized: true,
        count: vertexCount,
        type: "VEC3",
      },
      {
        bufferView: 2,
        componentType: 5123,
        count: data.indices.length,
        type: "SCALAR",
      },
    ],
  };

  const jsonBytes = Buffer.from(JSON.stringify(json), "utf8");
  const jsonView = pad4(jsonBytes).fill(0x20, jsonBytes.length);
  const binView = pad4(bin);
  const total = 12 + 8 + jsonView.length + 8 + binView.length;
  const header = Buffer.alloc(12);
  header.writeUInt32LE(0x46546c67, 0);
  header.writeUInt32LE(2, 4);
  header.writeUInt32LE(total, 8);
  const jsonHeader = Buffer.alloc(8);
  jsonHeader.writeUInt32LE(jsonView.length, 0);
  jsonHeader.writeUInt32LE(0x4e4f534a, 4);
  const binHeader = Buffer.alloc(8);
  binHeader.writeUInt32LE(binView.length, 0);
  binHeader.writeUInt32LE(0x004e4942, 4);
  return {
    bytes: Buffer.concat([header, jsonHeader, jsonView, binHeader, binView]),
    triangles: data.indices.length / 3,
  };
}

mkdirSync(OUTPUT, { recursive: true });
let total = 0;
for (const model of MODELS) {
  const { bytes, triangles } = buildGlb(model.name, model.boxes);
  if (triangles >= MAX_TRIS) {
    throw new Error(`${model.file} has ${triangles} triangles, budget is under ${MAX_TRIS}`);
  }
  writeFileSync(join(OUTPUT, model.file), bytes);
  total += bytes.length;
  console.log(`${model.file.padEnd(24)} ${String(triangles).padStart(3)} tris ${bytes.length} bytes`);
}
console.log(`${MODELS.length} models, ${total} bytes total`);
if (total > MAX_TOTAL_BYTES) {
  throw new Error(`models total ${total} bytes, budget is ${MAX_TOTAL_BYTES}`);
}
