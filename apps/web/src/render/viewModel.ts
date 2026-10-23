/**
 * First-person hand and held item, drawn as a separate pass over the world.
 *
 * The local body is hidden in first person, so without this the view has no
 * hands. A dedicated scene with a cleared depth buffer keeps the hand out of
 * the world's near-plane and fog problems. Slot 0 shows a pickaxe; the other
 * slots show the held crate.
 */

import * as THREE from "three";

const SKIN = 0xd6a07a;
const SLEEVE = 0x4f8fd6;
const WOOD = 0x8a6b4d;
const STRAP = 0x4a321d;
const METAL = 0xbfc6cf;

export interface ViewModelInfo {
  active: boolean;
  selected: number;
  pickVisible: boolean;
  crateVisible: boolean;
  frames: number;
  swings: number;
}

export class ViewModel {
  readonly scene = new THREE.Scene();
  readonly camera = new THREE.PerspectiveCamera(70, 16 / 9, 0.05, 5);
  readonly root = new THREE.Group();
  private readonly pick = new THREE.Group();
  private readonly crate = new THREE.Group();
  private active = false;
  private selected = 0;
  private frames = 0;
  private swings = 0;
  private swingStart = Number.NEGATIVE_INFINITY;

  constructor() {
    this.scene.add(this.root);

    const material = (color: number): THREE.MeshBasicMaterial =>
      new THREE.MeshBasicMaterial({ color });
    const part = (
      group: THREE.Group,
      size: [number, number, number],
      at: [number, number, number],
      color: number,
      rotation?: [number, number, number],
    ): THREE.Mesh => {
      const mesh = new THREE.Mesh(
        new THREE.BoxGeometry(size[0], size[1], size[2]),
        material(color),
      );
      mesh.position.set(at[0], at[1], at[2]);
      if (rotation !== undefined) {
        mesh.rotation.set(rotation[0], rotation[1], rotation[2]);
      }
      group.add(mesh);
      return mesh;
    };
    const arm = (group: THREE.Group): void => {
      part(group, [0.13, 0.32, 0.13], [0.02, -0.17, 0.09], SLEEVE, [0.12, 0, 0.18]);
      part(group, [0.11, 0.11, 0.11], [0, 0.01, 0.04], SKIN);
    };

    arm(this.pick);
    const pickTool = new THREE.Group();
    pickTool.position.set(0, 0.04, 0);
    pickTool.rotation.set(-0.12, 0, 0.42);
    part(pickTool, [0.042, 0.46, 0.042], [0, 0.22, 0], WOOD);
    part(pickTool, [0.065, 0.075, 0.065], [0, 0.41, 0], METAL);
    part(pickTool, [0.28, 0.05, 0.07], [0, 0.45, 0], METAL);
    part(pickTool, [0.05, 0.12, 0.065], [-0.15, 0.4, 0], METAL, [0, 0, 0.5]);
    part(pickTool, [0.05, 0.12, 0.065], [0.15, 0.4, 0], METAL, [0, 0, -0.5]);
    this.pick.add(pickTool);

    arm(this.crate);
    const crateBox = new THREE.Group();
    crateBox.position.set(0, 0.17, 0);
    crateBox.rotation.set(0.1, -0.25, 0);
    part(crateBox, [0.22, 0.22, 0.22], [0, 0, 0], WOOD);
    part(crateBox, [0.23, 0.024, 0.012], [0, 0.075, 0.111], STRAP);
    part(crateBox, [0.23, 0.024, 0.012], [0, -0.075, 0.111], STRAP);
    part(crateBox, [0.024, 0.23, 0.012], [0, 0, 0.111], STRAP);
    this.crate.add(crateBox);

    this.root.add(this.pick, this.crate);
    this.root.position.set(0.62, -0.52, -1);
    this.setSelected(0);
  }

  setSelected(slot: number): void {
    this.selected = Math.max(0, Math.min(8, Math.floor(slot)));
    this.pick.visible = this.selected === 0;
    this.crate.visible = this.selected !== 0;
  }

  setActive(active: boolean): void {
    this.active = active;
    this.root.visible = active;
  }

  swing(nowMs = performance.now()): void {
    this.swingStart = nowMs;
    this.swings += 1;
  }

  get isActive(): boolean {
    return this.active;
  }

  /** Advance the idle bob, the short swing curve and the overlay projection. */
  update(nowMs: number, moving: boolean, aspect: number): void {
    if (!this.active) return;
    this.camera.aspect = aspect;
    this.camera.updateProjectionMatrix();
    const duration = 180;
    const progress = Math.min(1, Math.max(0, (nowMs - this.swingStart) / duration));
    const swing = progress < 1 ? Math.sin(progress * Math.PI) : 0;
    const bob = moving
      ? Math.sin(nowMs * 0.012) * 0.012
      : Math.sin(nowMs * 0.0015) * 0.004;
    this.root.position.set(0.62 + bob * 0.4, -0.52 + bob - swing * 0.05, -1 + swing * 0.08);
    this.root.rotation.set(-0.05 + swing * 0.7, -0.04, 0.04 - swing * 0.3);
  }

  /** Draw over the world with a cleared depth buffer, like a hand pass. */
  draw(renderer: THREE.WebGLRenderer): void {
    if (!this.active) return;
    renderer.autoClear = false;
    renderer.clearDepth();
    renderer.render(this.scene, this.camera);
    renderer.autoClear = true;
    this.frames += 1;
  }

  info(): ViewModelInfo {
    return {
      active: this.active,
      selected: this.selected,
      pickVisible: this.pick.visible,
      crateVisible: this.crate.visible,
      frames: this.frames,
      swings: this.swings,
    };
  }
}
