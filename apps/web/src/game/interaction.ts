/**
 * Block interaction: a DDA voxel raycast, the target highlight and the
 * translucent placement preview (task T-034).
 *
 * # Why a voxel raycast and not a mesh raycast
 *
 * The highlighted cell must be a block the server will accept, and the
 * placement cell comes from the face normal. A triangle hit gives a point on a
 * greedy quad, not an integer cell; converting it back to a voxel is another
 * source of off-by-one bugs. The DDA walks the same integer grid the edit
 * pipeline validates against, so "what you see" and "what you send" are the
 * same cell by construction.
 *
 * # The VoxelSource seam
 *
 * The 13-export bridge has no block-at query. The chunk manager derives a
 * surface height map from its meshes and implements [`VoxelSource`] over it;
 * that is enough to target the terrain the player can see. It is not a full
 * column mirror (caves and the air under a canopy are approximated), so T-042's
 * diff stream will replace the source without touching this file.
 *
 * # Pending edits
 *
 * `breakBlock` and `place` call the sink, which in the client is
 * `cb_local_edit`, and keep the returned sequence in a pending map. An accepted
 * ack retires the record; a rejected one removes it. The local world is not
 * mutated before the ack, so "rollback" means "forget the prediction", and the
 * record is what a later predicted-edit layer needs to reconcile.
 */

import * as THREE from "three";
import { BLOCK_CRATE, isBreakable } from "./blocks";

/** The 6 m reach, shared with the server's edit pipeline. */
export const REACH = 6;

/** `edit_action::BREAK` (§7). */
export const EDIT_BREAK = 0;
/** `edit_action::PLACE` (§7). */
export const EDIT_PLACE = 1;

/** Hold time from first contact to a break request. */
export const MINE_MS = 400;

/** Five deterministic crack stages; later stages include the earlier lines. */
const CRACK_LINES: ReadonlyArray<
  ReadonlyArray<readonly [number, number, number, number]>
> = [
  [[8, 8, 5, 5], [8, 8, 11, 6], [8, 8, 7, 12]],
  [[5, 5, 3, 8], [11, 6, 13, 4], [7, 12, 4, 14]],
  [[8, 8, 2, 3], [8, 8, 14, 10], [4, 14, 7, 15]],
  [[2, 3, 1, 6], [14, 10, 15, 13], [3, 8, 1, 11], [10, 2, 12, 1]],
  [[1, 6, 0, 9], [15, 13, 15, 16], [5, 15, 2, 16], [12, 1, 14, 0], [9, 9, 12, 12]],
];

function makeCrackTexture(stage: number): THREE.CanvasTexture {
  const canvas = document.createElement("canvas");
  canvas.width = 16;
  canvas.height = 16;
  const ctx = canvas.getContext("2d");
  if (ctx !== null) {
    ctx.clearRect(0, 0, 16, 16);
    for (let s = 0; s <= stage; s += 1) {
      for (const [x0, y0, x1, y1] of CRACK_LINES[s]) {
        // Dark halo plus a light core so the crack reads on both the dark
        // platform and bright sand.
        ctx.strokeStyle = "rgba(16, 10, 6, 0.78)";
        ctx.lineWidth = 2;
        ctx.beginPath();
        ctx.moveTo(x0 + 0.5, y0 + 0.5);
        ctx.lineTo(x1 + 0.5, y1 + 0.5);
        ctx.stroke();
        ctx.strokeStyle = "rgba(245, 238, 222, 0.9)";
        ctx.lineWidth = 1;
        ctx.beginPath();
        ctx.moveTo(x0 + 0.5, y0 + 0.5);
        ctx.lineTo(x1 + 0.5, y1 + 0.5);
        ctx.stroke();
      }
    }
  }
  const texture = new THREE.CanvasTexture(canvas);
  texture.magFilter = THREE.NearestFilter;
  texture.minFilter = THREE.NearestFilter;
  texture.needsUpdate = true;
  return texture;
}

export interface Vec3Like {
  x: number;
  y: number;
  z: number;
}

/** Anything that can answer "is this cell solid?" and optionally its block id. */
export interface VoxelSource {
  solid(x: number, y: number, z: number): boolean;
  /** Block id at a cell, or -1 when the source cannot tell. */
  block?(x: number, y: number, z: number): number;
}

export interface RayHit {
  /** The first solid cell. */
  cell: Vec3Like;
  /** Face normal of the cell that was entered. */
  normal: Vec3Like;
  /** Cell a placement should target: `cell + normal`. */
  place: Vec3Like;
  /** Distance from the origin to the face, in metres. */
  distance: number;
  /** Block id at the hit cell, or -1 when the source cannot tell. */
  block: number;
}

/** The boundary to `cb_local_edit`. */
export interface EditSink {
  localEdit(x: number, y: number, z: number, block: number, action: number): number;
}

export interface PendingEdit {
  seq: number;
  x: number;
  y: number;
  z: number;
  block: number;
  action: number;
}

export interface VisualState {
  highlightVisible: boolean;
  ghostVisible: boolean;
  highlight: [number, number, number] | null;
  ghost: [number, number, number] | null;
  breakable: boolean;
  placeable: boolean;
  crackVisible: boolean;
  crackStage: number;
}

/**
 * Amanatides and Woo's voxel traversal.
 *
 * Returns the first solid cell entered at or before `maxDistance`. An origin
 * inside a solid cell returns null: there is no face to highlight and the
 * caller is in a state no rule describes.
 */
export function raycastVoxels(
  origin: Vec3Like,
  direction: Vec3Like,
  maxDistance: number,
  world: VoxelSource,
): RayHit | null {
  const length = Math.hypot(direction.x, direction.y, direction.z);
  if (!(length > 0) || !(maxDistance > 0)) {
    return null;
  }
  const dx = direction.x / length;
  const dy = direction.y / length;
  const dz = direction.z / length;

  let x = Math.floor(origin.x);
  let y = Math.floor(origin.y);
  let z = Math.floor(origin.z);
  if (world.solid(x, y, z)) {
    return null;
  }

  const stepX = dx > 0 ? 1 : dx < 0 ? -1 : 0;
  const stepY = dy > 0 ? 1 : dy < 0 ? -1 : 0;
  const stepZ = dz > 0 ? 1 : dz < 0 ? -1 : 0;

  const tDeltaX = stepX === 0 ? Infinity : Math.abs(1 / dx);
  const tDeltaY = stepY === 0 ? Infinity : Math.abs(1 / dy);
  const tDeltaZ = stepZ === 0 ? Infinity : Math.abs(1 / dz);

  let tMaxX =
    stepX > 0 ? (x + 1 - origin.x) / dx : stepX < 0 ? (x - origin.x) / dx : Infinity;
  let tMaxY =
    stepY > 0 ? (y + 1 - origin.y) / dy : stepY < 0 ? (y - origin.y) / dy : Infinity;
  let tMaxZ =
    stepZ > 0 ? (z + 1 - origin.z) / dz : stepZ < 0 ? (z - origin.z) / dz : Infinity;

  while (true) {
    let normal: Vec3Like;
    let distance: number;
    if (tMaxX <= tMaxY && tMaxX <= tMaxZ) {
      x += stepX;
      distance = tMaxX;
      tMaxX += tDeltaX;
      normal = { x: -stepX, y: 0, z: 0 };
    } else if (tMaxY <= tMaxZ) {
      y += stepY;
      distance = tMaxY;
      tMaxY += tDeltaY;
      normal = { x: 0, y: -stepY, z: 0 };
    } else {
      z += stepZ;
      distance = tMaxZ;
      tMaxZ += tDeltaZ;
      normal = { x: 0, y: 0, z: -stepZ };
    }

    if (distance > maxDistance) {
      return null;
    }
    if (world.solid(x, y, z)) {
      return {
        cell: { x, y, z },
        normal,
        place: { x: x + normal.x, y: y + normal.y, z: z + normal.z },
        distance,
        block: world.block?.(x, y, z) ?? -1,
      };
    }
  }
}

export class Interaction {
  private readonly highlight: THREE.LineSegments;
  private readonly ghost: THREE.Mesh;
  private readonly crack: THREE.Mesh<THREE.PlaneGeometry, THREE.MeshBasicMaterial>;
  private readonly crackTextures: THREE.CanvasTexture[] = [];
  private readonly world: VoxelSource;
  private readonly edit: EditSink;
  private readonly reach: number;
  private readonly pendingEdits = new Map<number, PendingEdit>();
  private lastHit: RayHit | null = null;
  private readonly direction = new THREE.Vector3();
  private readonly crackForward = new THREE.Vector3(0, 0, 1);
  private readonly crackNormal = new THREE.Vector3();
  private crackStage = -1;
  private canPlace: ((x: number, y: number, z: number) => boolean) | null = null;
  private placeable = false;
  private mineX = Number.NaN;
  private mineY = Number.NaN;
  private mineZ = Number.NaN;
  private mineStart = 0;

  constructor(
    scene: THREE.Scene,
    world: VoxelSource,
    edit: EditSink,
    reach = REACH,
  ) {
    this.world = world;
    this.edit = edit;
    this.reach = reach;

    const box = new THREE.BoxGeometry(1.002, 1.002, 1.002);
    this.highlight = new THREE.LineSegments(
      new THREE.EdgesGeometry(box),
      new THREE.LineBasicMaterial({
        color: 0x0a0e12,
        transparent: true,
        opacity: 0.92,
        depthTest: false,
        depthWrite: false,
      }),
    );
    box.dispose();
    this.highlight.visible = false;
    this.highlight.renderOrder = 999;
    scene.add(this.highlight);

    this.ghost = new THREE.Mesh(
      new THREE.BoxGeometry(1, 1, 1),
      new THREE.MeshBasicMaterial({
        color: 0x66ccff,
        transparent: true,
        opacity: 0.35,
        depthWrite: false,
      }),
    );
    this.ghost.visible = false;
    this.ghost.renderOrder = 998;
    scene.add(this.ghost);

    this.crack = new THREE.Mesh(
      new THREE.PlaneGeometry(1.004, 1.004),
      new THREE.MeshBasicMaterial({
        transparent: true,
        opacity: 0.92,
        depthWrite: false,
        side: THREE.DoubleSide,
        polygonOffset: true,
        polygonOffsetFactor: -2,
      }),
    );
    this.crack.visible = false;
    this.crack.renderOrder = 997;
    scene.add(this.crack);
  }

  /**
   * Restrict placement to the player's own pad. The callback is read at aim
   * time, so a rule installed before a match starts still sees the later base
   * data.
   */
  setPlacementRule(rule: ((x: number, y: number, z: number) => boolean) | null): void {
    this.canPlace = rule;
  }

  /** Cast from an explicit eye and direction and update the previews. */
  aim(origin: Vec3Like, direction: Vec3Like): RayHit | null {
    const hit = raycastVoxels(origin, direction, this.reach, this.world);
    this.lastHit = hit;
    if (hit === null) {
      this.highlight.visible = false;
      this.ghost.visible = false;
      this.placeable = false;
      this.hideCrack();
      return null;
    }
    this.highlight.position.set(hit.cell.x + 0.5, hit.cell.y + 0.5, hit.cell.z + 0.5);
    this.highlight.visible = true;
    this.placeable =
      this.canPlace === null || this.canPlace(hit.place.x, hit.place.y, hit.place.z);
    (this.ghost.material as THREE.MeshBasicMaterial).color.setHex(
      this.placeable ? 0x66ccff : 0xff6a5e,
    );
    this.ghost.position.set(hit.place.x + 0.5, hit.place.y + 0.5, hit.place.z + 0.5);
    this.ghost.visible = true;
    return hit;
  }

  /** Cast from the camera's eye and facing. */
  aimFromCamera(camera: THREE.PerspectiveCamera): RayHit | null {
    camera.getWorldDirection(this.direction);
    return this.aim(camera.position, this.direction);
  }

  /** Break the highlighted cell. Returns the local edit sequence, or 0. */
  breakBlock(): number {
    if (this.lastHit === null || !isBreakable(this.lastHit.block)) {
      return 0;
    }
    const { x, y, z } = this.lastHit.cell;
    return this.record(this.edit.localEdit(x, y, z, 0, EDIT_BREAK), x, y, z, 0, EDIT_BREAK);
  }

  /**
   * Advance the hold-to-break crack overlay and return its 0..1 progress.
   * `resetMining` must be called after a break request or on release.
   */
  mine(held: boolean, nowMs: number): number {
    if (!held || this.lastHit === null || !isBreakable(this.lastHit.block)) {
      this.hideCrack();
      return 0;
    }
    if (this.crackTextures.length === 0) {
      for (let stage = 0; stage < CRACK_LINES.length; stage += 1) {
        this.crackTextures.push(makeCrackTexture(stage));
      }
    }
    const cell = this.lastHit.cell;
    if (cell.x !== this.mineX || cell.y !== this.mineY || cell.z !== this.mineZ) {
      this.mineX = cell.x;
      this.mineY = cell.y;
      this.mineZ = cell.z;
      this.mineStart = nowMs;
    }
    const progress = Math.min(1, Math.max(0, (nowMs - this.mineStart) / MINE_MS));
    const stage = Math.min(
      this.crackTextures.length - 1,
      Math.floor(progress * this.crackTextures.length),
    );
    if (stage !== this.crackStage) {
      this.crack.material.map = this.crackTextures[stage];
      this.crack.material.needsUpdate = true;
      this.crackStage = stage;
    }
    const normal = this.lastHit.normal;
    this.crack.position.set(
      cell.x + 0.5 + normal.x * 0.502,
      cell.y + 0.5 + normal.y * 0.502,
      cell.z + 0.5 + normal.z * 0.502,
    );
    this.crack.quaternion.setFromUnitVectors(
      this.crackForward,
      this.crackNormal.set(normal.x, normal.y, normal.z),
    );
    this.crack.visible = true;
    // The ghost sits on the adjacent cell and would tint the hit face.
    this.ghost.visible = false;
    return progress;
  }

  resetMining(): void {
    this.hideCrack();
  }

  private hideCrack(): void {
    this.crack.visible = false;
    this.crackStage = -1;
    this.mineX = Number.NaN;
    this.mineY = Number.NaN;
    this.mineZ = Number.NaN;
  }

  /** Place into the cell above the highlighted face. */
  place(block: number): number {
    if (this.lastHit === null || block !== BLOCK_CRATE || !this.placeable) {
      return 0;
    }
    const { x, y, z } = this.lastHit.place;
    return this.record(
      this.edit.localEdit(x, y, z, block, EDIT_PLACE),
      x,
      y,
      z,
      block,
      EDIT_PLACE,
    );
  }

  /** Retire a prediction. Returns false for an unknown sequence. */
  ack(seq: number, accepted: boolean): boolean {
    void accepted;
    return this.pendingEdits.delete(seq);
  }

  pending(): PendingEdit[] {
    return [...this.pendingEdits.values()];
  }

  visuals(): VisualState {
    return {
      highlightVisible: this.highlight.visible,
      ghostVisible: this.ghost.visible,
      highlight: this.highlight.visible
        ? [this.highlight.position.x, this.highlight.position.y, this.highlight.position.z]
        : null,
      ghost: this.ghost.visible
        ? [this.ghost.position.x, this.ghost.position.y, this.ghost.position.z]
        : null,
      breakable: this.lastHit !== null && isBreakable(this.lastHit.block),
      placeable: this.ghost.visible && this.placeable,
      crackVisible: this.crack.visible,
      crackStage: this.crackStage,
    };
  }

  dispose(): void {
    this.highlight.removeFromParent();
    this.ghost.removeFromParent();
    this.crack.removeFromParent();
    this.crack.geometry.dispose();
    this.crack.material.dispose();
    for (const texture of this.crackTextures) {
      texture.dispose();
    }
    (this.highlight.geometry as THREE.BufferGeometry).dispose();
    (this.highlight.material as THREE.Material).dispose();
    (this.ghost.geometry as THREE.BufferGeometry).dispose();
    (this.ghost.material as THREE.Material).dispose();
    this.pendingEdits.clear();
  }

  private record(seq: number, x: number, y: number, z: number, block: number, action: number): number {
    if (seq > 0) {
      this.pendingEdits.set(seq, { seq, x, y, z, block, action });
    }
    return seq;
  }
}
