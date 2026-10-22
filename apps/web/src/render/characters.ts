/**
 * T-051 character meshes for the instanced entity renderer.
 *
 * The pipeline writes one merged, vertex-coloured mesh per model. This module
 * reads that narrow GLB subset directly instead of shipping GLTFLoader: one
 * primitive, no materials, no nodes beyond the mesh. Every failure falls back
 * to the T-033 boxes in `entities.ts`.
 */

import * as THREE from "three";

export const PLAYER_MODEL_FILES = [
  "player_scout.glb",
  "player_guard.glb",
  "player_raider.glb",
] as const;

export const BUDDY_MODEL_FILES = [
  "buddy_swordsman.glb",
  "buddy_archer.glb",
  "buddy_mage.glb",
  "buddy_healer.glb",
  "buddy_berserker.glb",
  "buddy_sniper.glb",
] as const;

export interface CharacterModels {
  readonly players: THREE.BufferGeometry[];
  readonly buddies: THREE.BufferGeometry[];
}

const GLB_MAGIC = 0x46546c67;
const JSON_CHUNK = 0x4e4f534a;
const BIN_CHUNK = 0x004e4942;
const FLOAT = 5126;
const UNSIGNED_BYTE = 5121;
const UNSIGNED_SHORT = 5123;
const UNSIGNED_INT = 5125;

interface Gltf {
  meshes: Array<{ primitives: Array<{ attributes: Record<string, number>; indices?: number }> }>;
  accessors: Array<{
    bufferView: number;
    byteOffset?: number;
    componentType: number;
    count: number;
    type: string;
    normalized?: boolean;
  }>;
  bufferViews: Array<{ byteOffset?: number; byteLength: number }>;
}

function readAccessor(gltf: Gltf, bin: Uint8Array, index: number): THREE.BufferAttribute {
  const accessor = gltf.accessors[index];
  if (accessor === undefined) throw new Error(`missing accessor ${index}`);
  const view = gltf.bufferViews[accessor.bufferView];
  if (view === undefined) throw new Error(`missing bufferView for accessor ${index}`);
  const itemSize = accessor.type === "VEC3" ? 3 : 1;
  const start = (view.byteOffset ?? 0) + (accessor.byteOffset ?? 0);
  const end = start + accessor.count * itemSize * componentSize(accessor.componentType);
  if (end > bin.byteLength) throw new Error(`accessor ${index} runs past the BIN chunk`);
  const slice = bin.slice(start, end);

  if (accessor.componentType === FLOAT) {
    return new THREE.BufferAttribute(new Float32Array(slice.buffer, slice.byteOffset, accessor.count * itemSize), itemSize);
  }
  if (accessor.componentType === UNSIGNED_BYTE) {
    return new THREE.BufferAttribute(slice, itemSize, accessor.normalized === true);
  }
  if (accessor.componentType === UNSIGNED_SHORT) {
    return new THREE.BufferAttribute(new Uint16Array(slice.buffer, slice.byteOffset, accessor.count), 1);
  }
  if (accessor.componentType === UNSIGNED_INT) {
    return new THREE.BufferAttribute(new Uint32Array(slice.buffer, slice.byteOffset, accessor.count), 1);
  }
  throw new Error(`unsupported accessor component ${accessor.componentType}`);
}

function componentSize(componentType: number): number {
  if (componentType === FLOAT || componentType === UNSIGNED_INT) return 4;
  if (componentType === UNSIGNED_SHORT) return 2;
  if (componentType === UNSIGNED_BYTE) return 1;
  throw new Error(`unsupported component type ${componentType}`);
}

export function parseGlb(buffer: ArrayBuffer): THREE.BufferGeometry {
  const view = new DataView(buffer);
  if (view.byteLength < 20 || view.getUint32(0, true) !== GLB_MAGIC) {
    throw new Error("not a GLB file");
  }
  if (view.getUint32(4, true) !== 2) throw new Error("unsupported GLB version");
  const length = view.getUint32(8, true);
  if (length > buffer.byteLength) throw new Error("truncated GLB file");

  let gltf: Gltf | null = null;
  let bin: Uint8Array | null = null;
  let offset = 12;
  while (offset + 8 <= length) {
    const chunkLength = view.getUint32(offset, true);
    const chunkType = view.getUint32(offset + 4, true);
    const start = offset + 8;
    if (start + chunkLength > length) throw new Error("truncated GLB chunk");
    if (chunkType === JSON_CHUNK) {
      const text = new TextDecoder().decode(new Uint8Array(buffer, start, chunkLength));
      gltf = JSON.parse(text.trim()) as Gltf;
    } else if (chunkType === BIN_CHUNK) {
      bin = new Uint8Array(buffer, start, chunkLength);
    }
    offset = start + chunkLength;
  }
  if (gltf === null || bin === null) throw new Error("GLB is missing its JSON or BIN chunk");
  const primitive = gltf.meshes[0]?.primitives[0];
  if (primitive === undefined) throw new Error("GLB has no primitive");
  const positionIndex = primitive.attributes.POSITION;
  const colorIndex = primitive.attributes.COLOR_0;
  if (positionIndex === undefined || colorIndex === undefined) {
    throw new Error("GLB primitive needs POSITION and COLOR_0");
  }

  const geometry = new THREE.BufferGeometry();
  geometry.setAttribute("position", readAccessor(gltf, bin, positionIndex));
  const color = readAccessor(gltf, bin, colorIndex);
  // The pipeline authors hex colours in sRGB; vertex colours are consumed in
  // the renderer's working (linear) space.
  const scratch = new THREE.Color();
  for (let i = 0; i < color.count; i += 1) {
    scratch.setRGB(color.getX(i), color.getY(i), color.getZ(i), THREE.SRGBColorSpace);
    color.setXYZ(i, scratch.r, scratch.g, scratch.b);
  }
  color.setUsage(THREE.StaticDrawUsage);
  geometry.setAttribute("color", color);
  if (primitive.indices !== undefined) {
    geometry.setIndex(readAccessor(gltf, bin, primitive.indices));
  }
  geometry.computeBoundingSphere();
  return geometry;
}

async function fetchModel(file: string): Promise<THREE.BufferGeometry> {
  const response = await fetch(`/assets/models/${file}`);
  if (!response.ok) throw new Error(`${file}: HTTP ${response.status}`);
  return parseGlb(await response.arrayBuffer());
}

export async function loadCharacterModels(): Promise<CharacterModels> {
  const [players, buddies] = await Promise.all([
    Promise.all(PLAYER_MODEL_FILES.map(fetchModel)),
    Promise.all(BUDDY_MODEL_FILES.map(fetchModel)),
  ]);
  return { players, buddies };
}
