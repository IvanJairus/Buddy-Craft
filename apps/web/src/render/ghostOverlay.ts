/**
 * The client-side recipe ghost (plan §4.5.5, task T-038).
 *
 * One translucent `InstancedMesh`; blocks come from the recipe row decoded from
 * wasm, and the anchor is the world cell the player is aiming at. The overlay
 * never touches game state. `show` is called when a placement target changes,
 * not every frame.
 */

import * as THREE from "three";

export interface GhostInfo {
  visible: boolean;
  count: number;
  blocks: Array<[number, number, number]>;
}

export class GhostOverlay {
  private readonly mesh: THREE.InstancedMesh<
    THREE.BoxGeometry,
    THREE.MeshBasicMaterial
  >;
  private readonly dummy = new THREE.Object3D();
  private blocks: Array<[number, number, number]> = [];

  constructor(scene: THREE.Scene, maxBlocks = 32) {
    const geometry = new THREE.BoxGeometry(0.98, 0.98, 0.98);
    const material = new THREE.MeshBasicMaterial({
      color: 0x8fd0ff,
      transparent: true,
      opacity: 0.28,
      depthWrite: false,
    });
    this.mesh = new THREE.InstancedMesh(geometry, material, maxBlocks);
    this.mesh.count = 0;
    this.mesh.visible = false;
    this.mesh.frustumCulled = false;
    this.mesh.renderOrder = 6;
    this.mesh.instanceMatrix.setUsage(THREE.DynamicDrawUsage);
    scene.add(this.mesh);
  }

  /** Place a recipe at `origin`, in world block coordinates. */
  show(blocks: ReadonlyArray<readonly [number, number, number]>, origin: readonly [number, number, number]): GhostInfo {
    const count = Math.min(blocks.length, this.mesh.instanceMatrix.count);
    for (let i = 0; i < count; i += 1) {
      const block = blocks[i];
      this.dummy.position.set(
        origin[0] + block[0] + 0.5,
        origin[1] + block[1] + 0.5,
        origin[2] + block[2] + 0.5,
      );
      this.dummy.rotation.set(0, 0, 0);
      this.dummy.scale.set(1, 1, 1);
      this.dummy.updateMatrix();
      this.mesh.setMatrixAt(i, this.dummy.matrix);
    }
    this.mesh.count = count;
    this.mesh.visible = count > 0;
    this.mesh.instanceMatrix.needsUpdate = true;
    this.blocks = blocks.slice(0, count).map((block) => [block[0], block[1], block[2]]);
    return this.info();
  }

  hide(): void {
    this.mesh.visible = false;
    this.mesh.count = 0;
    this.blocks = [];
  }

  info(): GhostInfo {
    return {
      visible: this.mesh.visible,
      count: this.mesh.count,
      blocks: this.blocks,
    };
  }

  dispose(): void {
    this.mesh.geometry.dispose();
    this.mesh.material.dispose();
    this.mesh.removeFromParent();
  }
}
