/**
 * Instanced player and buddy rendering (plan §8.3, task T-033).
 *
 * Every entity is a three-part hierarchy: body, head and weapon. Each part is
 * one `InstancedMesh`, so six players cost three draw calls and each buddy
 * role costs three more. There is no skeleton; the animation is a per-instance
 * matrix and alpha write driven by the floats in `GpuPlayer`.
 *
 * # Reading the contract
 *
 * `update(views, animate)` reads the `RenderDesc` and the player/buddy arrays
 * directly through their `DataView`s. No per-entity objects are allocated:
 * the pools are created once and refilled in place. The tests use
 * `updateFromValues` to feed the same pools without a wasm instance.
 *
 * # Animation
 *
 * - `anim` is a turn phase in `[0, 1)`, so the bob is `sin(anim * 2π) * 0.06`.
 * - `pitch` is clamped to ±0.25 rad and applied as a body/head tilt.
 * - `flags` bit 2 marks the attack window; the weapon pulses by up to 35%.
 * - A dead entity (`hp == 0` or !alive) dissolves as `1 - anim`: `anim` is the
 *   death progress in that case, because the 32-byte record has no second
 *   float for a death timer.
 *
 * The last two are conventions the bridge and T-041 must keep. They are
 * documented here because a renderer that silently reinterprets a float is
 * exactly the kind of bug the flat layout was supposed to prevent.
 */

import * as THREE from "three";
import { DESC_BUDDY_COUNT, DESC_PLAYER_COUNT, GPU_PLAYER_BYTES } from "../wasm/loader";
import type { RenderViews } from "../wasm/loader";
import type { CharacterModels } from "./characters";

export const FLAG_ALIVE = 1 << 0;
export const FLAG_ON_GROUND = 1 << 1;
export const FLAG_ATTACKING = 1 << 2;

export const BUDDY_ROLE_COUNT = 6;
export const MAX_PLAYERS = 18;
export const MAX_BUDDIES = 64;

const BODY_HEIGHT = 0.9;
const BOB_AMPLITUDE = 0.06;
const ATTACK_PULSE = 0.35;

const PLAYER_COLORS = [0x4f8fd6, 0xd66a4f];
const ROLE_COLORS = [0xc45b5b, 0x5ba36f, 0x6c7bd6, 0xd9d3a2, 0xd98f4a, 0xa873c9];
const HEAD_LIGHT = 0.18;
const WEAPON_COLOR = new THREE.Color(0x8b8f98);
const SCRATCH = new THREE.Object3D();
SCRATCH.rotation.order = "YXZ";

/** One entity as the renderer consumes it. */
export interface EntityValue {
  id: number;
  team: number;
  flags: number;
  x: number;
  y: number;
  z: number;
  yaw: number;
  pitch: number;
  anim: number;
  hp: number;
  role: number;
}

export interface EntityInfo {
  drawCalls: number;
  players: number;
  buddies: number;
  hiddenLocal: number;
  lastUpdateMs: number;
}

/** The pooled output of `Interpolator`, structurally. */
export interface PooledEntities {
  playerCount: number;
  buddyCount: number;
  readonly playerIds: Uint8Array;
  readonly playerX: Float32Array;
  readonly playerY: Float32Array;
  readonly playerZ: Float32Array;
  readonly playerYaw: Float32Array;
  readonly playerFlags: Uint8Array;
  readonly playerHp: Uint8Array;
  readonly playerAnim: Float32Array;
  readonly playerTeam: Uint8Array;
  readonly buddyIds: Uint8Array;
  readonly buddyX: Float32Array;
  readonly buddyY: Float32Array;
  readonly buddyZ: Float32Array;
  readonly buddyHp: Uint8Array;
  readonly buddyRole: Uint8Array;
}

export function bobOffset(anim: number): number {
  return Math.sin(anim * Math.PI * 2) * BOB_AMPLITUDE;
}

export function tiltAngle(pitch: number): number {
  return Math.max(-0.25, Math.min(0.25, pitch));
}

export function attackScale(flags: number, anim: number): number {
  if ((flags & FLAG_ATTACKING) === 0) {
    return 1;
  }
  return 1 + ATTACK_PULSE * Math.abs(Math.sin(anim * Math.PI * 2));
}

export function dissolveAlpha(flags: number, hp: number, anim: number): number {
  const alive = (flags & FLAG_ALIVE) !== 0 && hp > 0;
  if (alive) {
    return 1;
  }
  return Math.min(1, Math.max(0, 1 - anim));
}

interface Part {
  mesh: THREE.InstancedMesh<THREE.BufferGeometry, THREE.MeshBasicMaterial>;
  alpha: THREE.InstancedBufferAttribute;
}

type ModelPool = Part;

function makePart(
  material: THREE.MeshBasicMaterial,
  size: [number, number, number],
  max: number,
): Part {
  const geometry = new THREE.BoxGeometry(size[0], size[1], size[2]);
  const alpha = new THREE.InstancedBufferAttribute(new Float32Array(max).fill(1), 1);
  geometry.setAttribute("instanceAlpha", alpha);
  const mesh = new THREE.InstancedMesh(geometry, material, max);
  mesh.count = 0;
  mesh.visible = false;
  mesh.frustumCulled = false;
  mesh.instanceMatrix.setUsage(THREE.DynamicDrawUsage);
  return { mesh, alpha };
}

function makeModelPool(
  material: THREE.MeshBasicMaterial,
  source: THREE.BufferGeometry,
  max: number,
): ModelPool {
  const geometry = source.clone();
  const alpha = new THREE.InstancedBufferAttribute(new Float32Array(max).fill(1), 1);
  geometry.setAttribute("instanceAlpha", alpha);
  const mesh = new THREE.InstancedMesh(geometry, material, max);
  mesh.count = 0;
  mesh.visible = false;
  mesh.frustumCulled = false;
  mesh.instanceMatrix.setUsage(THREE.DynamicDrawUsage);
  return { mesh, alpha };
}

function createMaterial(vertexColors: boolean, key: string): THREE.MeshBasicMaterial {
  const material = new THREE.MeshBasicMaterial({ color: 0xffffff, transparent: true, vertexColors });
  material.onBeforeCompile = (shader) => {
    shader.vertexShader = shader.vertexShader.replace(
      "#include <common>",
      "#include <common>\nattribute float instanceAlpha;\nvarying float vInstanceAlpha;",
    );
    shader.vertexShader = shader.vertexShader.replace(
      "#include <begin_vertex>",
      "#include <begin_vertex>\nvInstanceAlpha = instanceAlpha;",
    );
    shader.fragmentShader = shader.fragmentShader.replace(
      "#include <common>",
      "#include <common>\nvarying float vInstanceAlpha;",
    );
    shader.fragmentShader = shader.fragmentShader.replace(
      "#include <dithering_fragment>",
      "#include <dithering_fragment>\ngl_FragColor.a *= vInstanceAlpha;",
    );
  };
  material.customProgramCacheKey = () => key;
  return material;
}

export class EntityRenderer {
  readonly group = new THREE.Group();

  private readonly material = createMaterial(false, "entity-instance-alpha");
  private readonly modelMaterial: THREE.MeshBasicMaterial | null;
  private readonly playerParts: [Part, Part, Part];
  private readonly roleParts: Array<[Part, Part, Part]>;
  private readonly playerModelPools: ModelPool[] = [];
  private readonly buddyModelPools: ModelPool[] = [];
  private readonly slotCounts = new Uint16Array(BUDDY_ROLE_COUNT);
  private readonly players: EntityValue[] = [];
  private readonly buddies: EntityValue[] = [];
  private readonly color = new THREE.Color();
  private readonly headColor = new THREE.Color();
  private readonly flashUntil = new Map<number, number>();
  private playerCount = 0;
  private buddyCount = 0;
  private localId = -1;
  private viewX = Number.POSITIVE_INFINITY;
  private viewZ = Number.POSITIVE_INFINITY;
  private lastUpdateMs = 0;

  constructor(parent: THREE.Object3D, models: CharacterModels | null = null) {
    this.playerParts = [
      makePart(this.material, [0.6, BODY_HEIGHT, 0.4], MAX_PLAYERS),
      makePart(this.material, [0.4, 0.4, 0.4], MAX_PLAYERS),
      makePart(this.material, [0.12, 0.12, 0.65], MAX_PLAYERS),
    ];
    this.roleParts = Array.from({ length: BUDDY_ROLE_COUNT }, () => [
      makePart(this.material, [0.6, BODY_HEIGHT, 0.4], MAX_BUDDIES),
      makePart(this.material, [0.4, 0.4, 0.4], MAX_BUDDIES),
      makePart(this.material, [0.12, 0.12, 0.65], MAX_BUDDIES),
    ]);

    for (const parts of [this.playerParts, ...this.roleParts]) {
      for (const part of parts) {
        this.group.add(part.mesh);
      }
    }

    if (models !== null && models.players.length > 0 && models.buddies.length > 0) {
      this.modelMaterial = createMaterial(true, "entity-model-alpha");
      for (const geometry of models.players) {
        const pool = makeModelPool(this.modelMaterial, geometry, MAX_PLAYERS);
        this.playerModelPools.push(pool);
        this.group.add(pool.mesh);
      }
      for (const geometry of models.buddies) {
        const pool = makeModelPool(this.modelMaterial, geometry, MAX_BUDDIES);
        this.buddyModelPools.push(pool);
        this.group.add(pool.mesh);
      }
    } else {
      this.modelMaterial = null;
    }

    for (let i = 0; i < MAX_PLAYERS; i += 1) {
      this.players.push(emptyEntity());
    }
    for (let i = 0; i < MAX_BUDDIES; i += 1) {
      this.buddies.push(emptyEntity());
    }
    parent.add(this.group);
  }

  /** Read the wasm render buffer and redraw. */
  update(views: RenderViews, animate: boolean): EntityInfo {
    const start = performance.now();
    const playerCount = Math.min(
      views.desc.getUint32(DESC_PLAYER_COUNT, true),
      MAX_PLAYERS,
    );
    const buddyCount = Math.min(
      views.desc.getUint32(DESC_BUDDY_COUNT, true),
      MAX_BUDDIES,
    );
    for (let i = 0; i < playerCount; i += 1) {
      readEntity(views.players, i, this.players[i]);
    }
    for (let i = 0; i < buddyCount; i += 1) {
      readEntity(views.buddies, i, this.buddies[i]);
    }
    this.apply(playerCount, buddyCount, animate, start);
    return this.info();
  }

  /** Test and offline path: the same pools, filled from plain values. */
  updateFromValues(
    players: readonly EntityValue[],
    buddies: readonly EntityValue[],
    animate: boolean,
  ): EntityInfo {
    const start = performance.now();
    const playerCount = Math.min(players.length, MAX_PLAYERS);
    const buddyCount = Math.min(buddies.length, MAX_BUDDIES);
    for (let i = 0; i < playerCount; i += 1) {
      copyEntity(players[i], this.players[i]);
    }
    for (let i = 0; i < buddyCount; i += 1) {
      copyEntity(buddies[i], this.buddies[i]);
    }
    this.apply(playerCount, buddyCount, animate, start);
    return this.info();
  }

  /**
   * Online path: the interpolator's typed-array pools go straight into the
   * renderer's pools. No per-entity object is created between snapshot decode
   * and matrix write.
   */
  updateFromInterp(source: PooledEntities, animate: boolean): EntityInfo {
    const start = performance.now();
    const playerCount = Math.min(source.playerCount, MAX_PLAYERS);
    const buddyCount = Math.min(source.buddyCount, MAX_BUDDIES);
    for (let i = 0; i < playerCount; i += 1) {
      const value = this.players[i];
      value.id = source.playerIds[i];
      value.team = source.playerTeam[i];
      value.flags = source.playerFlags[i];
      value.x = source.playerX[i];
      value.y = source.playerY[i];
      value.z = source.playerZ[i];
      value.yaw = source.playerYaw[i];
      value.pitch = 0;
      value.anim = source.playerAnim[i];
      value.hp = source.playerHp[i];
      value.role = 0;
    }
    for (let i = 0; i < buddyCount; i += 1) {
      const value = this.buddies[i];
      const hp = source.buddyHp[i];
      value.id = source.buddyIds[i];
      value.team = 0;
      value.flags = hp > 0 ? FLAG_ALIVE : 0;
      value.x = source.buddyX[i];
      value.y = source.buddyY[i];
      value.z = source.buddyZ[i];
      value.yaw = 0;
      value.pitch = 0;
      value.anim = 0;
      value.hp = hp;
      value.role = source.buddyRole[i];
    }
    this.apply(playerCount, buddyCount, animate, start);
    return this.info();
  }

  /** First-person view: the local body must not cover the reticle. */
  hideLocalPlayer(id: number): void {
    this.localId = id;
  }

  /** Hide a remote body that is standing inside the first-person camera. */
  setViewOrigin(x: number, z: number): void {
    this.viewX = x;
    this.viewZ = z;
  }

  private isInsideCamera(x: number, z: number): boolean {
    return Math.hypot(x - this.viewX, z - this.viewZ) < 1;
  }

  /** A one-event hit flash on one network id. Duration is presentation-only. */
  flash(id: number, nowMs = performance.now(), durationMs = 80): void {
    this.flashUntil.set(id, nowMs + durationMs);
  }

  /** Number of entity ids still inside a flash window. */
  flashCount(nowMs = performance.now()): number {
    let count = 0;
    for (const until of this.flashUntil.values()) {
      if (until > nowMs) count += 1;
    }
    return count;
  }

  /** One pooled entity by network id, for projectile or effect positioning. */
  positionOf(id: number): EntityValue | null {
    for (let i = 0; i < this.playerCount; i += 1) {
      if (this.players[i].id === id) return this.players[i];
    }
    for (let i = 0; i < this.buddyCount; i += 1) {
      if (this.buddies[i].id === id) return this.buddies[i];
    }
    return null;
  }

  info(): EntityInfo {
    let drawCalls = 0;
    if (this.modelMaterial !== null) {
      for (const pool of [...this.playerModelPools, ...this.buddyModelPools]) {
        if (pool.mesh.visible) drawCalls += 1;
      }
    } else {
      if (this.playerCount > 0) {
        drawCalls += this.playerParts.length;
      }
      for (let role = 0; role < BUDDY_ROLE_COUNT; role += 1) {
        if (this.roleCount(role) > 0) {
          drawCalls += this.roleParts[role].length;
        }
      }
    }
    return {
      drawCalls,
      players: this.playerCount,
      buddies: this.buddyCount,
      hiddenLocal: this.localId,
      lastUpdateMs: this.lastUpdateMs,
    };
  }

  dispose(): void {
    for (const parts of [this.playerParts, ...this.roleParts]) {
      for (const part of parts) {
        part.mesh.geometry.dispose();
        this.group.remove(part.mesh);
      }
    }
    this.material.dispose();
    for (const pool of [...this.playerModelPools, ...this.buddyModelPools]) {
      pool.mesh.geometry.dispose();
      this.group.remove(pool.mesh);
    }
    this.modelMaterial?.dispose();
    this.group.removeFromParent();
  }

  private roleCount(role: number): number {
    let count = 0;
    for (let i = 0; i < this.buddyCount; i += 1) {
      if (this.buddies[i].role === role) {
        count += 1;
      }
    }
    return count;
  }

  private apply(playerCount: number, buddyCount: number, animate: boolean, start: number): void {
    this.playerCount = playerCount;
    this.buddyCount = buddyCount;
    const now = performance.now();

    if (this.modelMaterial !== null) {
      this.applyModels(this.playerModelPools, this.players, playerCount, animate, now);
      this.applyModels(this.buddyModelPools, this.buddies, buddyCount, animate, now);
    } else {
      for (let i = 0; i < playerCount; i += 1) {
        const value = this.players[i];
        const base = PLAYER_COLORS[value.team & 1] ?? PLAYER_COLORS[0];
        this.color.setHex((this.flashUntil.get(value.id) ?? 0) > now ? 0xffffff : base);
        this.writeEntity(this.playerParts, i, value, animate);
      }
      this.finishParts(this.playerParts, playerCount);

      for (let role = 0; role < BUDDY_ROLE_COUNT; role += 1) {
        const parts = this.roleParts[role];
        let slot = 0;
        for (let i = 0; i < buddyCount; i += 1) {
          const value = this.buddies[i];
          if (value.role !== role) {
            continue;
          }
          this.color.setHex(
            (this.flashUntil.get(value.id) ?? 0) > now ? 0xffffff : ROLE_COLORS[role],
          );
          this.writeEntity(parts, slot, value, animate);
          slot += 1;
        }
        this.finishParts(parts, slot);
      }
    }

    this.lastUpdateMs = performance.now() - start;
  }

  private applyModels(
    pools: ModelPool[],
    values: EntityValue[],
    count: number,
    animate: boolean,
    now: number,
  ): void {
    this.slotCounts.fill(0);
    for (let i = 0; i < count; i += 1) {
      const value = values[i];
      const poolIndex = pools === this.playerModelPools
        ? value.team % pools.length
        : value.role % pools.length;
      const slot = this.slotCounts[poolIndex];
      const pool = pools[poolIndex];
      if (slot >= pool.mesh.instanceMatrix.count) {
        continue;
      }
      this.slotCounts[poolIndex] = slot + 1;

      const hidden = value.id === this.localId || this.isInsideCamera(value.x, value.z);
      const bob = animate ? bobOffset(value.anim) : 0;
      const tilt = tiltAngle(value.pitch);
      const pulse = animate ? (attackScale(value.flags, value.anim) - 1) / ATTACK_PULSE : 0;
      const forwardX = Math.sin(value.yaw);
      const forwardZ = Math.cos(value.yaw);
      SCRATCH.position.set(
        value.x + forwardX * pulse * 0.14,
        value.y + bob,
        value.z + forwardZ * pulse * 0.14,
      );
      SCRATCH.rotation.set(tilt, value.yaw, 0);
      SCRATCH.scale.setScalar(hidden ? 0 : 1);
      SCRATCH.updateMatrix();
      pool.mesh.setMatrixAt(slot, SCRATCH.matrix);

      pool.alpha.setX(slot, dissolveAlpha(value.flags, value.hp, value.anim));
      const flash = (this.flashUntil.get(value.id) ?? 0) > now;
      this.color.setHex(flash ? 0xffc7b8 : 0xffffff);
      pool.mesh.setColorAt(slot, this.color);
    }

    for (let i = 0; i < pools.length; i += 1) {
      const pool = pools[i];
      const used = this.slotCounts[i];
      pool.mesh.count = used;
      pool.mesh.visible = used > 0;
      if (used > 0) {
        pool.mesh.instanceMatrix.needsUpdate = true;
        pool.alpha.needsUpdate = true;
        if (pool.mesh.instanceColor !== null) {
          pool.mesh.instanceColor.needsUpdate = true;
        }
      }
    }
  }

  private writeEntity(
    parts: [Part, Part, Part],
    index: number,
    value: EntityValue,
    animate: boolean,
  ): void {
    const bob = animate ? bobOffset(value.anim) : 0;
    const tilt = tiltAngle(value.pitch);
    const weaponScale = animate ? attackScale(value.flags, value.anim) : 1;
    const alpha = dissolveAlpha(value.flags, value.hp, value.anim);
    const shown =
      value.id === this.localId || this.isInsideCamera(value.x, value.z) ? 0 : 1;
    const [body, head, weapon] = parts;

    place(body, index, value.x, value.y + BODY_HEIGHT / 2 + bob, value.z, tilt, value.yaw, shown);
    place(head, index, value.x, value.y + BODY_HEIGHT + 0.35 + bob, value.z, tilt, value.yaw, shown);

    const forwardX = Math.sin(value.yaw);
    const forwardZ = Math.cos(value.yaw);
    const rightX = Math.cos(value.yaw);
    const rightZ = -Math.sin(value.yaw);
    place(
      weapon,
      index,
      value.x + forwardX * 0.34 + rightX * 0.3,
      value.y + 0.75 + bob,
      value.z + forwardZ * 0.34 + rightZ * 0.3,
      tilt,
      value.yaw,
      shown === 0 ? 0 : weaponScale,
    );

    const tint = this.color;
    body.mesh.setColorAt(index, tint);
    this.headColor.copy(tint).offsetHSL(0, 0, HEAD_LIGHT);
    head.mesh.setColorAt(index, this.headColor);
    weapon.mesh.setColorAt(index, WEAPON_COLOR);

    for (const part of parts) {
      part.alpha.array[index] = alpha;
      part.alpha.needsUpdate = true;
    }
  }

  private finishParts(parts: [Part, Part, Part], count: number): void {
    for (const part of parts) {
      part.mesh.count = count;
      part.mesh.visible = count > 0;
      part.mesh.instanceMatrix.needsUpdate = true;
      if (part.mesh.instanceColor !== null) {
        part.mesh.instanceColor.needsUpdate = true;
      }
    }
  }
}

function place(
  part: Part,
  index: number,
  x: number,
  y: number,
  z: number,
  tilt: number,
  yaw: number,
  scale: number,
): void {
  SCRATCH.position.set(x, y, z);
  SCRATCH.rotation.set(tilt, yaw, 0);
  SCRATCH.scale.setScalar(scale);
  SCRATCH.updateMatrix();
  part.mesh.setMatrixAt(index, SCRATCH.matrix);
}

function emptyEntity(): EntityValue {
  return {
    id: 0,
    team: 0,
    flags: 0,
    x: 0,
    y: 0,
    z: 0,
    yaw: 0,
    pitch: 0,
    anim: 0,
    hp: 0,
    role: 0,
  };
}

function copyEntity(from: EntityValue, to: EntityValue): void {
  to.id = from.id;
  to.team = from.team;
  to.flags = from.flags;
  to.x = from.x;
  to.y = from.y;
  to.z = from.z;
  to.yaw = from.yaw;
  to.pitch = from.pitch;
  to.anim = from.anim;
  to.hp = from.hp;
  to.role = from.role;
}

function readEntity(view: DataView, index: number, to: EntityValue): void {
  const at = index * GPU_PLAYER_BYTES;
  to.id = view.getUint16(at, true);
  to.team = view.getUint8(at + 2);
  to.flags = view.getUint8(at + 3);
  to.x = view.getFloat32(at + 4, true);
  to.y = view.getFloat32(at + 8, true);
  to.z = view.getFloat32(at + 12, true);
  to.yaw = view.getFloat32(at + 16, true);
  to.pitch = view.getFloat32(at + 20, true);
  to.anim = view.getFloat32(at + 24, true);
  to.hp = view.getUint8(at + 28);
  to.role = view.getUint8(at + 29);
}