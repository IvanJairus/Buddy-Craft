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
  lastUpdateMs: number;
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

function createMaterial(): THREE.MeshBasicMaterial {
  const material = new THREE.MeshBasicMaterial({ color: 0xffffff, transparent: true });
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
  material.customProgramCacheKey = () => "entity-instance-alpha";
  return material;
}

export class EntityRenderer {
  readonly group = new THREE.Group();

  private readonly material = createMaterial();
  private readonly playerParts: [Part, Part, Part];
  private readonly roleParts: Array<[Part, Part, Part]>;
  private readonly players: EntityValue[] = [];
  private readonly buddies: EntityValue[] = [];
  private readonly color = new THREE.Color();
  private readonly headColor = new THREE.Color();
  private playerCount = 0;
  private buddyCount = 0;
  private lastUpdateMs = 0;

  constructor(parent: THREE.Object3D) {
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

  info(): EntityInfo {
    let drawCalls = 0;
    if (this.playerCount > 0) {
      drawCalls += this.playerParts.length;
    }
    for (let role = 0; role < BUDDY_ROLE_COUNT; role += 1) {
      if (this.roleCount(role) > 0) {
        drawCalls += this.roleParts[role].length;
      }
    }
    return {
      drawCalls,
      players: this.playerCount,
      buddies: this.buddyCount,
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

    for (let i = 0; i < playerCount; i += 1) {
      const value = this.players[i];
      this.color.setHex(PLAYER_COLORS[value.team & 1] ?? PLAYER_COLORS[0]);
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
        this.color.setHex(ROLE_COLORS[role]);
        this.writeEntity(parts, slot, value, animate);
        slot += 1;
      }
      this.finishParts(parts, slot);
    }

    this.lastUpdateMs = performance.now() - start;
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
    const [body, head, weapon] = parts;

    place(body, index, value.x, value.y + BODY_HEIGHT / 2 + bob, value.z, tilt, value.yaw, 1);
    place(head, index, value.x, value.y + BODY_HEIGHT + 0.35 + bob, value.z, tilt, value.yaw, 1);

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
      weaponScale,
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