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
 * mesh's world position is the column origin. The header is necessary because
 * the mesher picks the AO-safe diagonal per quad, which cannot be recovered
 * from vertex order alone.
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
}

interface ChunkEntry {
  mesh: THREE.Mesh<THREE.BufferGeometry, THREE.MeshBasicMaterial>;
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
  private readonly material = new THREE.MeshBasicMaterial({ color: 0x8aa96b });
  private readonly loaded = new Set<string>();
  private readonly meshes = new Map<string, ChunkEntry>();
  private pending: Array<{ cx: number; cz: number; distance: number }> = [];
  private meshed = 0;
  private lastMeshBytes = 0;

  constructor(parent: THREE.Object3D, source: MeshSource, options: ChunkOptions = {}) {
    this.source = source;
    this.loadRadius = Math.max(0, options.loadRadius ?? DEFAULT_LOAD_RADIUS);
    this.keepRadius = Math.max(
      this.loadRadius,
      options.keepRadius ?? DEFAULT_KEEP_RADIUS,
    );
    this.meshCapacity = options.meshCapacity ?? DEFAULT_MESH_CAPACITY;
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

  info(): ChunkInfo {
    return {
      loaded: this.loaded.size,
      pending: this.pending.length,
      drawCalls: this.meshes.size,
      meshed: this.meshed,
      lastMeshBytes: this.lastMeshBytes,
    };
  }

  dispose(): void {
    for (const key of [...this.loaded]) {
      this.unload(key);
    }
    this.group.removeFromParent();
    this.material.dispose();
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

    const geometry = new THREE.BufferGeometry();
    geometry.setAttribute("position", new THREE.BufferAttribute(positions, 3));
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
