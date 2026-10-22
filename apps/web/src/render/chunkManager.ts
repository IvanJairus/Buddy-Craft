/**
 * Column streaming and mesh handoff (task T-032).
 *
 * The world is 16 x 16 columns of 16 m. The manager keeps the columns within
 * six chunks of the player loaded, keeps the ring at seven for hysteresis, and
 * unloads anything beyond seven. Loading is a budget: one column per client
 * frame by default, so a teleport does not mesh 169 columns in one tick.
 *
 * # Ownership
 *
 * [`MeshSource`] returns an owned copy of one mesh payload, never a view into
 * wasm memory. The wasm loader's `meshChunk` is the real implementation and it
 * copies with `.slice()` before releasing the generation. The manager can
 * therefore keep the bytes and hand them to three.js without worrying about a
 * later `memory.grow`; a detached `ArrayBuffer` would be a zero-length geometry
 * in this layer, and the loader's dev assert catches it at the boundary.
 *
 * # Payload
 *
 * `u32 vertex_bytes`, `u32 index_bytes`, then 12-byte vertices and `u32`
 * triangle indices. Positions are the mesher's local block coordinates; the
 * mesh's world position is the column origin. The vertex also carries a tile
 * index and in-plane cell coordinates, so the shader can tile the shared
 * 256x256 block atlas once per block. The header is necessary because the
 * mesher picks the AO-safe diagonal per quad, which cannot be recovered from
 * vertex order alone.
 */

import * as THREE from "three";
import type { ClientWasm } from "../wasm/loader";
import type { VoxelSource } from "../game/interaction";

export const COLUMN_SIZE = 16;
export const MAP_COLUMNS = 16;
export const DEFAULT_LOAD_RADIUS = 6;
export const DEFAULT_KEEP_RADIUS = 7;
export const DEFAULT_MESH_CAPACITY = 512 * 1024;
export const VERTEX_BYTES = 12;

export interface MeshSource {
  /**
   * Mesh one column, returning an owned payload or `null` for an empty column.
   * Cap is a hint; a source that needs more may return null and the caller
   * retries at a larger capacity in a later task.
   */
  meshColumn(cx: number, cz: number, capacity: number): Uint8Array | null;
}

/** Adapt the wasm loader to the chunk manager's source contract. */
export function clientMeshSource(client: ClientWasm): MeshSource {
  return {
    meshColumn: (cx, cz, capacity) => client.meshChunk(cx, cz, capacity),
  };
}

export interface ChunkOptions {
  /** Columns within this Chebyshev radius are required. */
  loadRadius?: number;
  /** Loaded columns survive until they are beyond this radius. */
  keepRadius?: number;
  /** Staging capacity per mesh request. */
  meshCapacity?: number;
}

export interface ChunkInfo {
  loaded: number;
  pending: number;
  drawCalls: number;
  meshed: number;
  lastMeshBytes: number;
  /** True once the browser swapped the 1x1 placeholder for the real atlas. */
  atlasReady: boolean;
}

interface ChunkEntry {
  mesh: THREE.Mesh<THREE.BufferGeometry, THREE.ShaderMaterial>;
  cx: number;
  cz: number;
  /** Top solid boundary per local (x, z), 0 when empty. */
  heights: Uint8Array;
}

function keyOf(cx: number, cz: number): string {
  return `${cx},${cz}`;
}

export class ChunkManager {
  readonly group = new THREE.Group();

  private readonly source: MeshSource;
  private readonly loadRadius: number;
  private readonly keepRadius: number;
  private readonly meshCapacity: number;
  private atlas: THREE.Texture;
  private readonly material: THREE.ShaderMaterial;
  private readonly loaded = new Set<string>();
  private readonly meshes = new Map<string, ChunkEntry>();
  private pending: Array<{ cx: number; cz: number; distance: number }> = [];
  private meshed = 0;
  private lastMeshBytes = 0;
  private atlasReady = false;
  private disposed = false;

  constructor(parent: THREE.Object3D, source: MeshSource, options: ChunkOptions = {}) {
    this.source = source;
    this.loadRadius = Math.max(0, options.loadRadius ?? DEFAULT_LOAD_RADIUS);
    this.keepRadius = Math.max(
      this.loadRadius,
      options.keepRadius ?? DEFAULT_KEEP_RADIUS,
    );
    this.meshCapacity = options.meshCapacity ?? DEFAULT_MESH_CAPACITY;
    this.atlas = createPlaceholderAtlas();
    this.material = createTerrainMaterial(this.atlas);
    if (typeof document !== "undefined") {
      loadAtlas((texture) => {
        if (this.disposed) {
          texture.dispose();
          return;
        }
        const placeholder = this.atlas;
        this.atlas = texture;
        this.material.uniforms.uAtlas.value = texture;
        placeholder.dispose();
        this.atlasReady = true;
      });
    }
    parent.add(this.group);
  }

  /**
   * Reconcile the loaded set with a player position.
   *
   * `budget` is the maximum number of new columns meshed in this call. The real
   * loop passes 1; tests pass a large budget to reach a steady state without
   * waiting a frame per column.
   */
  update(playerX: number, playerZ: number, budget = 1): void {
    const pcx = Math.floor(playerX / COLUMN_SIZE);
    const pcz = Math.floor(playerZ / COLUMN_SIZE);

    for (const key of [...this.loaded]) {
      const [cx, cz] = key.split(",").map(Number);
      const distance = Math.max(Math.abs(cx - pcx), Math.abs(cz - pcz));
      if (distance > this.keepRadius) {
        this.unload(key);
      }
    }

    this.pending = [];
    for (let dz = -this.loadRadius; dz <= this.loadRadius; dz += 1) {
      for (let dx = -this.loadRadius; dx <= this.loadRadius; dx += 1) {
        const cx = pcx + dx;
        const cz = pcz + dz;
        if (cx < 0 || cz < 0 || cx >= MAP_COLUMNS || cz >= MAP_COLUMNS) {
          continue;
        }
        if (this.loaded.has(keyOf(cx, cz))) {
          continue;
        }
        this.pending.push({ cx, cz, distance: dx * dx + dz * dz });
      }
    }
    this.pending.sort(
      (a, b) => a.distance - b.distance || a.cx - b.cx || a.cz - b.cz,
    );

    for (let i = 0; i < budget && this.pending.length > 0; i += 1) {
      const next = this.pending.shift()!;
      this.meshOne(next.cx, next.cz);
    }
  }

  /** True when a column is considered loaded, including an empty one. */
  hasColumn(cx: number, cz: number): boolean {
    return this.loaded.has(keyOf(cx, cz));
  }

  /**
   * Top solid boundary at a world column from the rendered mesh.
   *
   * This is the highest vertex in that local column, so it is the top face of
   * the highest solid block. It is a surface approximation: caves and the air
   * under a tree canopy are not represented. T-042's column mirror replaces
   * it for edits; T-034 only needs a nearby, coherent surface.
   */
  surfaceAt(x: number, z: number): number {
    const cx = Math.floor(x / COLUMN_SIZE);
    const cz = Math.floor(z / COLUMN_SIZE);
    const entry = this.meshes.get(keyOf(cx, cz));
    if (entry === undefined) {
      return 0;
    }
    const lx = x - cx * COLUMN_SIZE;
    const lz = z - cz * COLUMN_SIZE;
    if (lx < 0 || lz < 0 || lx >= COLUMN_SIZE || lz >= COLUMN_SIZE) {
      return 0;
    }
    return entry.heights[lz * COLUMN_SIZE + lx];
  }

  /** True when a world cell lies under the rendered surface. */
  solidAt(x: number, y: number, z: number): boolean {
    return y < this.surfaceAt(x, z);
  }

  /** A `VoxelSource` for the interaction raycast. */
  voxelSource(): VoxelSource {
    return { solid: (x, y, z) => this.solidAt(x, y, z) };
  }

  /**
   * Remesh one loaded column after its voxel data changed.
   *
   * Returns false when the column is outside the current streaming set; it
   * will be meshed from the new data when it loads. The caller refreshes the
   * four neighbours too, because a border edit changes their culled faces.
   */
  refreshColumn(cx: number, cz: number): boolean {
    const key = keyOf(cx, cz);
    if (!this.loaded.has(key)) {
      return false;
    }
    const entry = this.meshes.get(key);
    if (entry !== undefined) {
      this.group.remove(entry.mesh);
      entry.mesh.geometry.dispose();
      this.meshes.delete(key);
    }
    this.loaded.delete(key);
    this.meshOne(cx, cz);
    return true;
  }

  /** Refresh a column and its four neighbours, returning how many were live. */
  refreshAround(cx: number, cz: number): number {
    let refreshed = 0;
    for (const [dx, dz] of [
      [0, 0],
      [-1, 0],
      [1, 0],
      [0, -1],
      [0, 1],
    ]) {
      if (this.refreshColumn(cx + dx, cz + dz)) refreshed += 1;
    }
    return refreshed;
  }

  info(): ChunkInfo {
    return {
      loaded: this.loaded.size,
      pending: this.pending.length,
      drawCalls: this.meshes.size,
      meshed: this.meshed,
      lastMeshBytes: this.lastMeshBytes,
      atlasReady: this.atlasReady,
    };
  }

  dispose(): void {
    this.disposed = true;
    for (const key of [...this.loaded]) {
      this.unload(key);
    }
    this.group.removeFromParent();
    this.material.dispose();
    this.atlas.dispose();
    this.pending = [];
  }

  private meshOne(cx: number, cz: number): void {
    const key = keyOf(cx, cz);
    const payload = this.source.meshColumn(cx, cz, this.meshCapacity);
    if (payload === null || payload.byteLength === 0) {
      this.loaded.add(key);
      return;
    }
    if (payload.byteLength < 8) {
      throw new Error(`column ${key}: mesh payload shorter than its header`);
    }

    const view = new DataView(payload.buffer, payload.byteOffset, payload.byteLength);
    const vertexBytes = view.getUint32(0, true);
    const indexBytes = view.getUint32(4, true);
    const heightBytes = view.getUint32(8, true);
    if (
      vertexBytes % VERTEX_BYTES !== 0 ||
      indexBytes % 4 !== 0 ||
      heightBytes !== COLUMN_SIZE * COLUMN_SIZE ||
      12 + vertexBytes + indexBytes + heightBytes !== payload.byteLength
    ) {
      throw new Error(
        `column ${key}: malformed mesh payload (${vertexBytes} vertex bytes, ` +
          `${indexBytes} index bytes, ${heightBytes} height bytes, ` +
          `${payload.byteLength} total)`,
      );
    }

    const vertexCount = vertexBytes / VERTEX_BYTES;
    const positions = new Float32Array(vertexCount * 3);
    for (let i = 0; i < vertexCount; i += 1) {
      const at = 12 + i * VERTEX_BYTES;
      positions[i * 3] = payload[at];
      positions[i * 3 + 1] = payload[at + 1];
      positions[i * 3 + 2] = payload[at + 2];
    }

    const indices = new Uint32Array(indexBytes / 4);
    for (let i = 0; i < indices.length; i += 1) {
      indices[i] = view.getUint32(12 + vertexBytes + i * 4, true);
    }
    const heights = payload.slice(
      12 + vertexBytes + indexBytes,
      12 + vertexBytes + indexBytes + heightBytes,
    );

    const faces = new Float32Array(vertexCount);
    const tiles = new Float32Array(vertexCount);
    const ao = new Float32Array(vertexCount);
    const uvs = new Float32Array(vertexCount * 2);
    const flags = new Float32Array(vertexCount);
    for (let i = 0; i < vertexCount; i += 1) {
      const at = 12 + i * VERTEX_BYTES;
      faces[i] = payload[at + 3];
      tiles[i] = payload[at + 4];
      ao[i] = payload[at + 5];
      uvs[i * 2] = payload[at + 6];
      uvs[i * 2 + 1] = payload[at + 7];
      flags[i] = payload[at + 10];
    }

    const geometry = new THREE.BufferGeometry();
    geometry.setAttribute("position", new THREE.BufferAttribute(positions, 3));
    geometry.setAttribute("aFace", new THREE.BufferAttribute(faces, 1));
    geometry.setAttribute("aTile", new THREE.BufferAttribute(tiles, 1));
    geometry.setAttribute("aAo", new THREE.BufferAttribute(ao, 1));
    geometry.setAttribute("aUv", new THREE.BufferAttribute(uvs, 2));
    geometry.setAttribute("aFlags", new THREE.BufferAttribute(flags, 1));
    geometry.setIndex(new THREE.BufferAttribute(indices, 1));
    geometry.computeBoundingSphere();

    const mesh = new THREE.Mesh(geometry, this.material);
    mesh.position.set(cx * COLUMN_SIZE, 0, cz * COLUMN_SIZE);
    this.group.add(mesh);
    this.meshes.set(key, { mesh, cx, cz, heights });
    this.loaded.add(key);
    this.meshed += 1;
    this.lastMeshBytes = payload.byteLength;
  }

  private unload(key: string): void {
    const entry = this.meshes.get(key);
    if (entry !== undefined) {
      this.group.remove(entry.mesh);
      entry.mesh.geometry.dispose();
      this.meshes.delete(key);
    }
    this.loaded.delete(key);
  }
}

const TERRAIN_VERTEX = /* glsl */ `
attribute float aFace;
attribute float aTile;
attribute float aAo;
attribute vec2 aUv;
attribute float aFlags;

varying vec2 vCellUv;
varying float vTile;
varying float vFace;
varying float vShade;
varying float vAlpha;

void main() {
  int face = int(aFace + 0.5);
  float light = 0.78;
  if (face == 3) light = 1.0;
  else if (face == 2) light = 0.5;
  else if (face == 1 || face == 0) light = 0.84;
  vShade = light * (0.62 + 0.1266667 * clamp(aAo, 0.0, 3.0));
  vAlpha = aFlags > 0.5 ? 0.78 : 1.0;
  vCellUv = aUv;
  vTile = aTile;
  vFace = aFace;

  vec4 mv = modelViewMatrix * vec4(position, 1.0);
  gl_Position = projectionMatrix * mv;
}
`;

const TERRAIN_FRAGMENT = /* glsl */ `
uniform sampler2D uAtlas;
varying vec2 vCellUv;
varying float vTile;
varying float vFace;
varying float vShade;
varying float vAlpha;

void main() {
  // Interpolated cell coordinates repeat once per block, including across a
  // greedy quad. The inset keeps the sample inside the authored tile.
  vec2 tile = vec2(mod(vTile, 16.0), floor(vTile / 16.0));
  // Tiles 20 and 21 hold a log end and a grass side; 0..19 stay the block
  // registry order so the content test keeps its literal mapping.
  if (abs(vTile - 3.0) < 0.5) {
    if (vFace > 1.5 && vFace < 2.5) tile = vec2(2.0, 0.0);
    else if (vFace < 2.5 || vFace > 3.5) tile = vec2(5.0, 1.0);
  }
  if (abs(vTile - 7.0) < 0.5 && vFace > 1.5 && vFace < 3.5) {
    tile = vec2(4.0, 1.0);
  }
  vec2 local = fract(vCellUv) * 0.96 + 0.02;
  vec4 texel = texture2D(uAtlas, (tile + local) / 16.0);
  if (texel.a < 0.15) discard;
  gl_FragColor = vec4(texel.rgb * vShade, texel.a * vAlpha);
}
`;

function createPlaceholderAtlas(): THREE.Texture {
  // Node unit tests construct ChunkManager without a DOM. A one-pixel texture
  // keeps the shader valid there; the browser swaps in the real atlas.
  const texture = new THREE.DataTexture(
    new Uint8Array([124, 145, 106, 255]),
    1,
    1,
  );
  texture.magFilter = THREE.NearestFilter;
  texture.minFilter = THREE.NearestFilter;
  texture.generateMipmaps = false;
  texture.colorSpace = THREE.NoColorSpace;
  texture.needsUpdate = true;
  return texture;
}

function loadAtlas(onLoad: (texture: THREE.Texture) => void): void {
  // A DataTexture keeps uploading its small pixel buffer after its image is
  // replaced, so the loaded texture becomes the material uniform instead.
  new THREE.TextureLoader().load("/assets/atlas.png", (texture) => {
    texture.flipY = false;
    texture.magFilter = THREE.NearestFilter;
    texture.minFilter = THREE.NearestFilter;
    texture.generateMipmaps = false;
    texture.colorSpace = THREE.NoColorSpace;
    texture.needsUpdate = true;
    onLoad(texture);
  });
}

function createTerrainMaterial(atlas: THREE.Texture): THREE.ShaderMaterial {
  return new THREE.ShaderMaterial({
    uniforms: { uAtlas: { value: atlas } },
    vertexShader: TERRAIN_VERTEX,
    fragmentShader: TERRAIN_FRAGMENT,
    transparent: true,
    depthWrite: true,
    side: THREE.FrontSide,
  });
}
