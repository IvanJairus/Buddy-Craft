/**
 * Presentation juice (task T-044, plan §4.9).
 *
 * Everything here reacts to an event packet and never reads authoritative
 * state to decide that something happened. The caller passes the event slots
 * from `RenderDesc` or an equivalent decoded snapshot; hit flash, damage
 * numbers, shake, vignette, particles, slow motion and the airdrop marker are
 * outcomes, not polls.
 *
 * Presentation time is separate from the simulation clock. Hitstop and the
 * end-of-match slow motion scale only the seconds handed to particles and
 * effects; the room tick in wasm keeps advancing. That is the rule the
 * `hitstop_does_not_stop_simulation` spec pins.
 */

import * as THREE from "three";
import type { EntityRenderer } from "./entities";
import type { ParticleSystem } from "./particles";
import { CameraShake, type ShakeOffset } from "./shake";

export const EVENT_CRAFT = 0;
export const EVENT_KILL = 1;
export const EVENT_BASE_HIT = 2;
export const EVENT_ELIMINATION = 3;
export const EVENT_AIRDROP = 4;
export const EVENT_HIT = 5;
export const EVENT_PICKUP = 6;
export const EVENT_DEPOSIT = 7;
export const EVENT_AIRDROP_TELEGRAPH = 8;
export const EVENT_LOW_HP = 9;

export const DAMAGE_NUMBER_CAP = 32;

/** Every row of §4.9, including the ones the renderer only represents. */
export const EFFECTS = [
  "hit_flash",
  "hitstop",
  "knockback",
  "shake",
  "damage_numbers",
  "airdrop_telegraph",
  "airdrop_landing",
  "pickup",
  "deposit",
  "craft",
  "vignette",
  "buddy_death",
  "low_hp",
  "slow_motion",
] as const;

export interface JuiceStats {
  eventCount: number;
  damageActive: number;
  damageEvicted: number;
  flashes: number;
  shakeActive: boolean;
  vignette: number;
  lowHp: boolean;
  telegraphVisible: boolean;
  timeScale: number;
  presentationSeconds: number;
  lastUpdateMs: number;
}

export interface JuiceOptions {
  readonly camera: THREE.PerspectiveCamera;
  readonly scene: THREE.Scene;
  readonly entities: EntityRenderer | null;
  readonly particles: ParticleSystem | null;
  readonly reducedMotion: () => boolean;
  readonly surfaceAt?: (x: number, z: number) => number;
}

const STYLE = `
.cb-juice { position: fixed; inset: 0; pointer-events: none; overflow: hidden; z-index: 15; }
.cb-juice canvas { position: absolute; inset: 0; width: 100%; height: 100%; }
.cb-juice-vignette, .cb-juice-edge {
  position: absolute; inset: 0; opacity: 0;
}
.cb-juice-vignette { box-shadow: inset 0 0 90px 28px rgba(190, 24, 24, 0.9); }
.cb-juice-edge { box-shadow: inset 0 0 40px 18px rgba(255, 245, 210, 0.9); }
.cb-juice-low { box-shadow: inset 0 0 120px 45px rgba(0, 0, 0, 0.55); }
.cb-juice-telegraph {
  position: absolute; display: none; transform: translate(-50%, -50%);
  color: #ffe38a; font: 700 13px ui-monospace, monospace; letter-spacing: 0.12em;
  text-shadow: 0 1px 4px #000, 0 0 12px #c47c19;
  border: 1px solid rgba(255, 213, 112, 0.7); padding: 3px 7px; white-space: nowrap;
}
`;

export class Juice {
  readonly root: HTMLDivElement;
  presentationSeconds = 0;
  timeScale = 1;

  private readonly camera: THREE.PerspectiveCamera;
  private readonly scene: THREE.Scene;
  private readonly entities: EntityRenderer | null;
  private readonly particles: ParticleSystem | null;
  private readonly reducedMotion: () => boolean;
  private readonly surfaceAt: ((x: number, z: number) => number) | null;
  private readonly shake = new CameraShake();
  private readonly shakeOffset: ShakeOffset = { x: 0, y: 0, z: 0 };
  private readonly applied: ShakeOffset = { x: 0, y: 0, z: 0 };
  private readonly scratch = new THREE.Vector3();
  private readonly direction = new THREE.Vector3();
  private readonly velocity = new THREE.Vector3();
  private readonly canvas: HTMLCanvasElement;
  private readonly ctx: CanvasRenderingContext2D;
  private readonly vignette: HTMLDivElement;
  private readonly edge: HTMLDivElement;
  private readonly telegraph: HTMLDivElement;
  private readonly telegraphShadow: THREE.Mesh<THREE.CircleGeometry, THREE.MeshBasicMaterial>;
  private readonly beam: THREE.Mesh<THREE.CylinderGeometry, THREE.MeshBasicMaterial>;
  private readonly damageUntil = new Float64Array(DAMAGE_NUMBER_CAP);
  private readonly damageAmount = new Uint16Array(DAMAGE_NUMBER_CAP);
  private readonly damageX = new Float32Array(DAMAGE_NUMBER_CAP);
  private readonly damageY = new Float32Array(DAMAGE_NUMBER_CAP);
  private readonly damageZ = new Float32Array(DAMAGE_NUMBER_CAP);
  private readonly damageText: string[] = new Array(DAMAGE_NUMBER_CAP).fill("");
  private damageCursor = 0;
  private damageEvicted = 0;
  private hitstopUntil = 0;
  private slowUntil = 0;
  private pulseStart = 0;
  private pulseDuration = 0;
  private pulseStrength = 0;
  private edgeStart = 0;
  private edgeDuration = 0;
  private edgeStrength = 0;
  private telegraphUntil = 0;
  private telegraphStart = 0;
  private telegraphX = 0;
  private telegraphY = 0;
  private telegraphZ = 0;
  private lastTelegraphSecond = -1;
  private beamUntil = 0;
  private lastVignette = -1;
  private lowHp = false;
  private eventCount = 0;
  private lastUpdateMs = 0;

  constructor(options: JuiceOptions) {
    this.camera = options.camera;
    this.scene = options.scene;
    this.entities = options.entities;
    this.particles = options.particles;
    this.reducedMotion = options.reducedMotion;
    this.surfaceAt = options.surfaceAt ?? null;

    if (document.getElementById("cb-juice-style") === null) {
      const style = document.createElement("style");
      style.id = "cb-juice-style";
      style.textContent = STYLE;
      document.head.append(style);
    }
    this.root = document.createElement("div");
    this.root.className = "cb-juice";
    this.root.dataset.juice = "root";
    this.canvas = document.createElement("canvas");
    this.ctx = this.canvas.getContext("2d")!;
    this.vignette = document.createElement("div");
    this.vignette.className = "cb-juice-vignette";
    this.edge = document.createElement("div");
    this.edge.className = "cb-juice-edge";
    this.telegraph = document.createElement("div");
    this.telegraph.className = "cb-juice-telegraph";
    this.telegraph.textContent = "DROP";
    this.root.append(this.canvas, this.vignette, this.edge, this.telegraph);
    document.body.append(this.root);
    this.resize();
    window.addEventListener("resize", this.resize);

    this.beam = new THREE.Mesh(
      new THREE.CylinderGeometry(0.16, 0.42, 4, 10, 1, true),
      new THREE.MeshBasicMaterial({
        color: 0xffe3a1,
        transparent: true,
        opacity: 0.55,
        depthWrite: false,
        side: THREE.DoubleSide,
      }),
    );
    this.beam.visible = false;
    this.beam.frustumCulled = false;
    this.scene.add(this.beam);

    this.telegraphShadow = new THREE.Mesh(
      new THREE.CircleGeometry(2.4, 20),
      new THREE.MeshBasicMaterial({
        color: 0x090604,
        transparent: true,
        opacity: 0.26,
        depthWrite: false,
        side: THREE.DoubleSide,
      }),
    );
    this.telegraphShadow.rotation.x = -Math.PI / 2;
    this.telegraphShadow.visible = false;
    this.telegraphShadow.frustumCulled = false;
    this.scene.add(this.telegraphShadow);
  }

  /** Read the event slots of one RenderDesc-shaped frame. */
  consumeEvents(events: DataView, count: number, nowMs: number): number {
    const limit = Math.min(count, Math.floor(events.byteLength / 16), 64);
    let consumed = 0;
    for (let i = 0; i < limit; i += 1) {
      const at = i * 16;
      this.handleEvent(
        events.getUint32(at, true),
        events.getInt32(at + 4, true),
        events.getInt32(at + 8, true),
        events.getInt32(at + 12, true),
        nowMs,
      );
      consumed += 1;
    }
    return consumed;
  }

  /** Handle one event. Public for the test hook and for decoded snapshots. */
  handleEvent(kind: number, a: number, b: number, c: number, nowMs: number): void {
    this.eventCount += 1;
    switch (kind) {
      case EVENT_CRAFT:
        this.craft(nowMs);
        break;
      case EVENT_KILL:
        this.kill(a, nowMs);
        break;
      case EVENT_BASE_HIT:
        if (!this.reducedMotion()) {
          this.pulseVignette(nowMs, 0.9, 420);
          this.shake.add(nowMs, 0.06, 120);
        }
        break;
      case EVENT_ELIMINATION:
        if (!this.reducedMotion()) {
          this.slowUntil = Math.max(this.slowUntil, nowMs + 1_500);
        }
        break;
      case EVENT_AIRDROP:
        this.airdrop(a, b, c, nowMs);
        break;
      case EVENT_HIT:
        this.hit(a, b, nowMs);
        break;
      case EVENT_PICKUP:
        this.pickup(a, nowMs);
        break;
      case EVENT_DEPOSIT:
        this.deposit(a, b, c, nowMs);
        break;
      case EVENT_AIRDROP_TELEGRAPH:
        this.telegraphStart = nowMs;
        this.telegraphUntil = nowMs + 5_000;
        this.telegraphX = a;
        this.telegraphY = b;
        this.telegraphZ = c;
        this.lastTelegraphSecond = -1;
        this.telegraphShadow.visible = true;
        for (let i = 0; i < 8; i += 1) {
          const angle = (i / 8) * Math.PI * 2;
          this.scratch.set(a, b + 0.5, c);
          this.velocity.set(Math.cos(angle) * 0.7, 1.1, Math.sin(angle) * 0.7);
          this.particles?.spawn(this.scratch, this.velocity, 1.2, 0.18, 0xd9d2c4);
        }
        break;
      case EVENT_LOW_HP:
        this.lowHp = true;
        break;
      default:
        break;
    }
  }

  /** Freeze presentation. The simulation clock is owned elsewhere. */
  hitstop(nowMs: number, durationMs = 60): void {
    this.hitstopUntil = Math.max(this.hitstopUntil, nowMs + durationMs);
    this.timeScale = 0;
  }

  addDamage(amount: number, x: number, y: number, z: number, nowMs: number): void {
    const slot = this.damageCursor;
    this.damageCursor = (slot + 1) % DAMAGE_NUMBER_CAP;
    if (this.damageUntil[slot] > nowMs) this.damageEvicted += 1;
    this.damageUntil[slot] = nowMs + 900;
    this.damageAmount[slot] = Math.max(0, Math.min(65_535, Math.round(amount)));
    this.damageX[slot] = x;
    this.damageY[slot] = y;
    this.damageZ[slot] = z;
    this.damageText[slot] = String(this.damageAmount[slot]);
  }

  /** Advance presentation only. Returns the current presentation scale. */
  update(nowMs: number, dtMs: number): JuiceStats {
    const start = performance.now();
    const reduced = this.reducedMotion();
    const hitstop = nowMs < this.hitstopUntil;
    const slow = !reduced && nowMs < this.slowUntil;
    this.timeScale = hitstop ? 0 : slow ? 0.35 : 1;
    this.presentationSeconds += (Math.max(0, dtMs) / 1_000) * this.timeScale;

    if (reduced || hitstop) {
      this.shakeOffset.x = 0;
      this.shakeOffset.y = 0;
      this.shakeOffset.z = 0;
      if (hitstop) {
        this.shakeOffset.x = this.applied.x;
        this.shakeOffset.y = this.applied.y;
        this.shakeOffset.z = this.applied.z;
      }
    } else {
      this.shake.sample(nowMs, this.shakeOffset);
    }

    const vignette = reduced ? 0 : this.pulseAt(nowMs, this.pulseStart, this.pulseDuration, this.pulseStrength);
    if (Math.abs(vignette - this.lastVignette) > 0.01) {
      this.vignette.style.opacity = vignette.toFixed(3);
      this.lastVignette = vignette;
    }
    const edge = reduced
      ? 0
      : this.pulseAt(nowMs, this.edgeStart, this.edgeDuration, this.edgeStrength);
    this.edge.style.opacity = edge.toFixed(3);
    this.root.classList.toggle("cb-juice-low", this.lowHp);

    this.updateTelegraph(nowMs);
    if (this.beam.visible && nowMs >= this.beamUntil) {
      this.beam.visible = false;
    }
    this.drawDamage(nowMs);

    const result = this.stats(nowMs);
    this.lastUpdateMs = performance.now() - start;
    result.lastUpdateMs = this.lastUpdateMs;
    return result;
  }

  /** Apply the sampled shake just before rendering, and remove it after. */
  applyCamera(): void {
    this.camera.position.x += this.shakeOffset.x;
    this.camera.position.y += this.shakeOffset.y;
    this.camera.position.z += this.shakeOffset.z;
    this.applied.x = this.shakeOffset.x;
    this.applied.y = this.shakeOffset.y;
    this.applied.z = this.shakeOffset.z;
  }

  restoreCamera(): void {
    this.camera.position.x -= this.applied.x;
    this.camera.position.y -= this.applied.y;
    this.camera.position.z -= this.applied.z;
    this.applied.x = 0;
    this.applied.y = 0;
    this.applied.z = 0;
  }

  stats(nowMs = performance.now()): JuiceStats {
    let active = 0;
    for (let i = 0; i < DAMAGE_NUMBER_CAP; i += 1) {
      if (this.damageUntil[i] > nowMs) active += 1;
    }
    return {
      eventCount: this.eventCount,
      damageActive: active,
      damageEvicted: this.damageEvicted,
      flashes: this.entities?.flashCount(nowMs) ?? 0,
      shakeActive: this.shake.active(nowMs),
      vignette: this.lastVignette < 0 ? 0 : this.lastVignette,
      lowHp: this.lowHp,
      telegraphVisible: this.telegraphShadow.visible && nowMs < this.telegraphUntil,
      timeScale: this.timeScale,
      presentationSeconds: this.presentationSeconds,
      lastUpdateMs: this.lastUpdateMs,
    };
  }

  dispose(): void {
    window.removeEventListener("resize", this.resize);
    this.root.remove();
    this.beam.geometry.dispose();
    this.beam.material.dispose();
    this.beam.removeFromParent();
    this.telegraphShadow.geometry.dispose();
    this.telegraphShadow.material.dispose();
    this.telegraphShadow.removeFromParent();
  }

  private readonly resize = (): void => {
    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    this.canvas.width = Math.max(1, Math.round(window.innerWidth * dpr));
    this.canvas.height = Math.max(1, Math.round(window.innerHeight * dpr));
    this.ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    this.ctx.font = "700 18px ui-monospace, monospace";
    this.ctx.textAlign = "center";
    this.ctx.lineWidth = 3;
  };

  private hit(id: number, amount: number, nowMs: number): void {
    this.entities?.flash(id, nowMs);
    const at = this.entities?.positionOf(id);
    if (at !== undefined && at !== null) {
      this.addDamage(amount, at.x, at.y + 1.6, at.z, nowMs);
    }
    if (!this.reducedMotion()) {
      this.shake.add(nowMs, Math.min(Math.abs(amount) / 50, 1) * 0.06, 120);
    }
  }

  private kill(id: number, nowMs: number): void {
    this.entities?.flash(id, nowMs, 140);
    const at = this.entities?.positionOf(id);
    if (at !== undefined && at !== null) {
      this.spawnBurst(at.x, at.y + 0.8, at.z, 10, 0xffb06a);
    }
  }

  private pickup(player: number, nowMs: number): void {
    const at = this.entities?.positionOf(player);
    if (at !== undefined && at !== null) {
      this.spawnBurst(at.x, at.y + 1.1, at.z, 4, 0xffe08a);
    }
    void nowMs;
  }

  private deposit(x: number, y: number, z: number, nowMs: number): void {
    this.spawnBurst(x + 0.5, y + 0.5, z + 0.5, 5, 0xbfe6a0);
    void nowMs;
  }

  private craft(nowMs: number): void {
    this.scratch.copy(this.camera.position);
    this.camera.getWorldDirection(this.direction);
    this.scratch.addScaledVector(this.direction, 4);
    this.beam.position.set(this.scratch.x, this.scratch.y - 2, this.scratch.z);
    this.beam.visible = true;
    this.beamUntil = nowMs + 600;
    for (let i = 0; i < 12; i += 1) {
      const angle = (i / 12) * Math.PI * 2;
      this.direction.set(Math.cos(angle) * 0.5, 2.2, Math.sin(angle) * 0.5);
      this.velocity.copy(this.direction);
      this.particles?.spawn(this.beam.position, this.velocity, 0.7, 0.1, 0xffe3a1);
    }
  }

  private airdrop(x: number, z: number, crates: number, nowMs: number): void {
    const y = (this.surfaceAt?.(x, z) ?? 40) + 0.4;
    for (let i = 0; i < 16; i += 1) {
      const angle = (i / 16) * Math.PI * 2;
      this.scratch.set(x, y, z);
      this.velocity.set(Math.cos(angle) * 2.4, 0.4, Math.sin(angle) * 2.4);
      this.particles?.spawn(this.scratch, this.velocity, 0.65, 0.16, 0xd8c7a4);
    }
    void crates;
    const dx = x - this.camera.position.x;
    const dz = z - this.camera.position.z;
    if (!this.reducedMotion() && dx * dx + dz * dz <= 15 * 15) {
      this.edgeStart = nowMs;
      this.edgeDuration = 280;
      this.edgeStrength = 0.8;
    }
    void nowMs;
  }

  private spawnBurst(x: number, y: number, z: number, count: number, color: number): void {
    for (let i = 0; i < count; i += 1) {
      const angle = (i / count) * Math.PI * 2;
      this.scratch.set(x, y, z);
      this.velocity.set(Math.cos(angle) * 1.2, 1.4, Math.sin(angle) * 1.2);
      this.particles?.spawn(this.scratch, this.velocity, 0.45, 0.1, color);
    }
  }

  private pulseVignette(nowMs: number, strength: number, durationMs: number): void {
    this.pulseStart = nowMs;
    this.pulseDuration = durationMs;
    this.pulseStrength = strength;
  }

  private pulseAt(nowMs: number, start: number, duration: number, strength: number): number {
    if (duration <= 0 || nowMs >= start + duration) return 0;
    const t = Math.max(0, nowMs - start) / duration;
    return strength * (1 - t);
  }

  private updateTelegraph(nowMs: number): void {
    if (nowMs >= this.telegraphUntil) {
      if (this.telegraph.style.display !== "none") {
        this.telegraph.style.display = "none";
      }
      this.telegraphShadow.visible = false;
      return;
    }
    const progress = Math.min(
      1,
      Math.max(0, (nowMs - this.telegraphStart) / (this.telegraphUntil - this.telegraphStart)),
    );
    const ground = this.surfaceAt?.(this.telegraphX, this.telegraphZ) ?? 40;
    this.telegraphShadow.position.set(
      this.telegraphX,
      ground + 0.08,
      this.telegraphZ,
    );
    this.telegraphShadow.scale.setScalar(1 + progress * 1.4);
    this.telegraphShadow.material.opacity = 0.08 + progress * 0.3;
    this.scratch.set(this.telegraphX, this.telegraphY + 2, this.telegraphZ);
    this.scratch.project(this.camera);
    if (this.scratch.z > 1) {
      this.telegraph.style.display = "none";
      return;
    }
    const x = (this.scratch.x * 0.5 + 0.5) * window.innerWidth;
    const y = (-this.scratch.y * 0.5 + 0.5) * window.innerHeight;
    this.telegraph.style.display = "block";
    this.telegraph.style.transform = `translate(-50%, -50%) translate(${x}px, ${y}px)`;
    const second = Math.ceil((this.telegraphUntil - nowMs) / 1_000);
    if (second !== this.lastTelegraphSecond) {
      this.telegraph.textContent = `DROP ${second}`;
      this.lastTelegraphSecond = second;
    }
  }

  private drawDamage(nowMs: number): void {
    const ctx = this.ctx;
    ctx.clearRect(0, 0, window.innerWidth, window.innerHeight);
    for (let i = 0; i < DAMAGE_NUMBER_CAP; i += 1) {
      if (this.damageUntil[i] <= nowMs) continue;
      this.scratch.set(this.damageX[i], this.damageY[i], this.damageZ[i]);
      this.scratch.project(this.camera);
      if (this.scratch.z > 1) continue;
      const x = (this.scratch.x * 0.5 + 0.5) * window.innerWidth;
      const rise = ((nowMs - (this.damageUntil[i] - 900)) / 900) * 24;
      const y = (-this.scratch.y * 0.5 + 0.5) * window.innerHeight - rise;
      const fade = Math.min(1, Math.max(0, (this.damageUntil[i] - nowMs) / 350));
      ctx.globalAlpha = fade;
      ctx.strokeStyle = "rgba(0, 0, 0, 0.75)";
      ctx.fillStyle = this.damageAmount[i] >= 25 ? "#ffd166" : "#f4f1e8";
      ctx.strokeText(this.damageText[i], x, y);
      ctx.fillText(this.damageText[i], x, y);
    }
    ctx.globalAlpha = 1;
  }
}
