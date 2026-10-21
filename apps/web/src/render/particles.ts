/**
 * GPU-lifetime particles (plan §8.3, task T-033).
 *
 * One `InstancedMesh` of 256 quads, one draw call. The CPU writes a particle
 * once, when it spawns: origin, velocity, spawn time and life. The vertex
 * shader computes age, position and alpha from `uTime`, so a live particle
 * costs no per-frame JavaScript. `update()` only changes a uniform, which the
 * tests assert by counting attribute uploads.
 *
 * The quad is billboarded in the shader from the view matrix, so the mesh does
 * not need to face the camera on the CPU and 256 particles stay one draw call.
 */

import * as THREE from "three";

const VERTEX_SHADER = /* glsl */ `
uniform float uTime;

attribute vec3 iOrigin;
attribute vec3 iVelocity;
attribute float iSpawn;
attribute float iLife;
attribute float iSize;
attribute vec3 iColor;

varying float vAlpha;
varying vec3 vColor;

void main() {
  float age = uTime - iSpawn;
  float t = clamp(age / max(iLife, 0.001), 0.0, 1.0);
  vAlpha = (1.0 - t) * step(0.0, age);
  vColor = iColor;

  vec3 world = iOrigin + iVelocity * age + vec3(0.0, -4.0, 0.0) * age * age;
  vec3 right = vec3(viewMatrix[0][0], viewMatrix[1][0], viewMatrix[2][0]);
  vec3 up = vec3(viewMatrix[0][1], viewMatrix[1][1], viewMatrix[2][1]);
  world += right * position.x * iSize + up * position.y * iSize;

  gl_Position = projectionMatrix * viewMatrix * vec4(world, 1.0);
  if (vAlpha <= 0.0) {
    gl_Position = vec4(2.0, 2.0, 2.0, 1.0);
  }
}
`;

const FRAGMENT_SHADER = /* glsl */ `
varying float vAlpha;
varying vec3 vColor;

void main() {
  if (vAlpha <= 0.0) {
    discard;
  }
  gl_FragColor = vec4(vColor, vAlpha);
}
`;

export interface ParticleInfo {
  capacity: number;
  drawCalls: number;
  attributeWrites: number;
  lastUpdateMs: number;
  time: number;
}

export class ParticleSystem {
  readonly mesh: THREE.Mesh<THREE.InstancedBufferGeometry, THREE.ShaderMaterial>;

  private readonly max: number;
  private capacity: number;
  private cursor = 0;
  private time = 0;
  private attributeWrites = 0;
  private lastUpdateMs = 0;
  private readonly origins: Float32Array;
  private readonly velocities: Float32Array;
  private readonly spawns: Float32Array;
  private readonly lives: Float32Array;
  private readonly sizes: Float32Array;
  private readonly colors: Float32Array;
  private readonly color = new THREE.Color();

  constructor(parent: THREE.Object3D, capacity = 256) {
    this.max = capacity;
    this.capacity = capacity;
    this.origins = new Float32Array(capacity * 3);
    this.velocities = new Float32Array(capacity * 3);
    this.spawns = new Float32Array(capacity).fill(-1_000_000);
    this.lives = new Float32Array(capacity);
    this.sizes = new Float32Array(capacity).fill(0.12);
    this.colors = new Float32Array(capacity * 3).fill(1);

    const quad = new THREE.PlaneGeometry(1, 1);
    const geometry = new THREE.InstancedBufferGeometry();
    geometry.index = quad.index;
    geometry.setAttribute("position", quad.getAttribute("position"));
    geometry.setAttribute("uv", quad.getAttribute("uv"));
    geometry.setAttribute("iOrigin", new THREE.InstancedBufferAttribute(this.origins, 3));
    geometry.setAttribute("iVelocity", new THREE.InstancedBufferAttribute(this.velocities, 3));
    geometry.setAttribute("iSpawn", new THREE.InstancedBufferAttribute(this.spawns, 1));
    geometry.setAttribute("iLife", new THREE.InstancedBufferAttribute(this.lives, 1));
    geometry.setAttribute("iSize", new THREE.InstancedBufferAttribute(this.sizes, 1));
    geometry.setAttribute("iColor", new THREE.InstancedBufferAttribute(this.colors, 3));
    geometry.instanceCount = capacity;
    geometry.boundingSphere = new THREE.Sphere(new THREE.Vector3(), 1_000_000);

    const material = new THREE.ShaderMaterial({
      uniforms: { uTime: { value: 0 } },
      vertexShader: VERTEX_SHADER,
      fragmentShader: FRAGMENT_SHADER,
      transparent: true,
      depthWrite: false,
      side: THREE.DoubleSide,
    });

    this.mesh = new THREE.Mesh(geometry, material);
    this.mesh.frustumCulled = false;
    parent.add(this.mesh);
  }

  /**
   * Write one particle. The CPU sets its four lifetime inputs once; the shader
   * does the rest until the slot is reused.
   */
  spawn(
    origin: THREE.Vector3,
    velocity: THREE.Vector3,
    life: number,
    size = 0.12,
    color = 0xffffff,
  ): void {
    if (this.capacity === 0) {
      return;
    }
    const index = this.cursor;
    this.cursor = (index + 1) % this.capacity;
    const at = index * 3;
    this.origins[at] = origin.x;
    this.origins[at + 1] = origin.y;
    this.origins[at + 2] = origin.z;
    this.velocities[at] = velocity.x;
    this.velocities[at + 1] = velocity.y;
    this.velocities[at + 2] = velocity.z;
    this.spawns[index] = this.time;
    this.lives[index] = Math.max(0.001, life);
    this.sizes[index] = size;
    this.color.setHex(color);
    this.colors[at] = this.color.r;
    this.colors[at + 1] = this.color.g;
    this.colors[at + 2] = this.color.b;
    this.attributeWrites += 1;
    this.markAttributes();
  }

  /**
   * Advance the clock. This is the per-frame path: it writes one uniform and
   * does not touch an attribute, so live particles age on the GPU.
   */
  update(timeSeconds: number): ParticleInfo {
    const start = performance.now();
    this.time = timeSeconds;
    this.mesh.material.uniforms.uTime.value = timeSeconds;
    this.lastUpdateMs = performance.now() - start;
    return this.info();
  }

  /** Apply a quality tier's particle cap without reallocating the pool. */
  setCapacity(capacity: number): void {
    this.capacity = Math.max(0, Math.min(this.max, Math.floor(capacity)));
    this.mesh.geometry.instanceCount = this.capacity;
    this.mesh.visible = this.capacity > 0;
    if (this.capacity === 0) {
      this.cursor = 0;
    } else {
      this.cursor %= this.capacity;
    }
  }

  info(): ParticleInfo {
    return {
      capacity: this.capacity,
      drawCalls: this.mesh.visible && this.capacity > 0 ? 1 : 0,
      attributeWrites: this.attributeWrites,
      lastUpdateMs: this.lastUpdateMs,
      time: this.time,
    };
  }

  dispose(): void {
    this.mesh.geometry.dispose();
    this.mesh.material.dispose();
    this.mesh.removeFromParent();
  }

  private markAttributes(): void {
    for (const name of ["iOrigin", "iVelocity", "iSpawn", "iLife", "iSize", "iColor"]) {
      const attribute = this.mesh.geometry.getAttribute(name);
      if (attribute !== undefined) {
        attribute.needsUpdate = true;
      }
    }
  }
}
