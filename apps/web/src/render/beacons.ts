/**
 * World-space objective beacons: one beam and floating diamond per base, a
 * ring on your own pad, and a gold beam on the active drop. These are pure
 * presentation; positions come from `cb_bases` and snapshot events.
 */

import * as THREE from "three";
import type { BaseRow } from "../game/objectives";

const OWN_COLOR = 0x6fd3ff;
const ENEMY_COLOR = 0xf08a7a;
const DROP_COLOR = 0xffd166;

export class BeaconField {
  private readonly group = new THREE.Group();
  private readonly meshes: THREE.Mesh[] = [];
  private readonly beams: Array<{ mesh: THREE.Mesh; x: number; z: number }> = [];
  private readonly rings: Array<{ mesh: THREE.Mesh; x: number; z: number }> = [];
  private readonly beamGeometry = new THREE.CylinderGeometry(0.35, 0.35, 28, 6, 1, true);
  private readonly dropGeometry = new THREE.CylinderGeometry(0.45, 0.45, 24, 6, 1, true);
  private readonly diamondGeometry = new THREE.OctahedronGeometry(0.75, 0);
  private readonly ringGeometry = new THREE.RingGeometry(3.7, 4.15, 28);
  private bases: BaseRow[] = [];
  private yourTeam = 0;
  private drop: { x: number; y: number; z: number } | null = null;

  constructor(scene: THREE.Scene) {
    this.group.visible = false;
    scene.add(this.group);
  }

  setBases(bases: BaseRow[], yourTeam: number): void {
    const changed =
      this.yourTeam !== yourTeam ||
      this.bases.length !== bases.length ||
      bases.some((base, index) => {
        const old = this.bases[index];
        return (
          old === undefined ||
          old.padX !== base.padX ||
          old.padZ !== base.padZ ||
          old.y !== base.y
        );
      });
    if (!changed) return;
    this.bases = bases.slice();
    this.yourTeam = yourTeam;
    this.rebuild();
  }

  setDrop(drop: { x: number; y: number; z: number } | null): void {
    const changed =
      (this.drop === null) !== (drop === null) ||
      (drop !== null &&
        this.drop !== null &&
        (drop.x !== this.drop.x || drop.y !== this.drop.y || drop.z !== this.drop.z));
    if (!changed) return;
    this.drop = drop === null ? null : { ...drop };
    this.rebuild();
  }

  setVisible(visible: boolean): void {
    this.group.visible = visible;
  }

  /**
   * Hide a beam whose axis is within 3.5 m of the eye. A beam at your own pad
   * otherwise sits a few centimetres from the camera and floods the view.
   */
  setCamera(x: number, z: number): void {
    for (const beam of this.beams) {
      beam.mesh.visible = Math.hypot(x - beam.x, z - beam.z) > 3.5;
    }
    // The pad ring is flat and additive; from on top of it the band washes the
    // whole pad cyan, so only show it once the player has stepped away.
    for (const ring of this.rings) {
      ring.mesh.visible = Math.hypot(x - ring.x, z - ring.z) > 6;
    }
  }

  info(): Array<{ x: number; z: number; visible: boolean }> {
    return this.beams.map((beam) => ({ x: beam.x, z: beam.z, visible: beam.mesh.visible }));
  }

  private rebuild(): void {
    for (const mesh of this.meshes) {
      mesh.removeFromParent();
      if (mesh.geometry !== this.beamGeometry &&
          mesh.geometry !== this.dropGeometry &&
          mesh.geometry !== this.diamondGeometry &&
          mesh.geometry !== this.ringGeometry) {
        mesh.geometry.dispose();
      }
      if (Array.isArray(mesh.material)) {
        for (const material of mesh.material) material.dispose();
      } else {
        mesh.material.dispose();
      }
    }
    this.meshes.length = 0;
    this.beams.length = 0;
    this.rings.length = 0;

    for (let index = 0; index < this.bases.length; index += 1) {
      const base = this.bases[index];
      const own = index === this.yourTeam;
      const color = own ? OWN_COLOR : ENEMY_COLOR;
      const beam = new THREE.Mesh(
        this.beamGeometry,
        new THREE.MeshBasicMaterial({
          color,
          transparent: true,
          opacity: own ? 0.36 : 0.28,
          depthWrite: false,
          side: THREE.DoubleSide,
          blending: THREE.AdditiveBlending,
          fog: false,
        }),
      );
      beam.position.set(base.padX + 0.5, base.y + 14, base.padZ + 0.5);
      beam.renderOrder = 4;
      this.group.add(beam);
      this.meshes.push(beam);
      this.beams.push({ mesh: beam, x: base.padX + 0.5, z: base.padZ + 0.5 });

      const diamond = new THREE.Mesh(
        this.diamondGeometry,
        new THREE.MeshBasicMaterial({
          color,
          transparent: true,
          opacity: 0.95,
          depthTest: false,
          depthWrite: false,
          fog: false,
        }),
      );
      diamond.position.set(base.padX + 0.5, base.y + 29, base.padZ + 0.5);
      diamond.renderOrder = 12;
      this.group.add(diamond);
      this.meshes.push(diamond);

      if (own) {
        const ring = new THREE.Mesh(
          this.ringGeometry,
          new THREE.MeshBasicMaterial({
            color,
            transparent: true,
            opacity: 0.38,
            depthWrite: false,
            side: THREE.DoubleSide,
            blending: THREE.AdditiveBlending,
            fog: false,
          }),
        );
        ring.rotation.x = -Math.PI / 2;
        ring.position.set(base.padX + 0.5, base.y + 0.06, base.padZ + 0.5);
        ring.renderOrder = 5;
        this.group.add(ring);
        this.meshes.push(ring);
        this.rings.push({ mesh: ring, x: base.padX + 0.5, z: base.padZ + 0.5 });
      }
    }

    if (this.drop !== null) {
      const beam = new THREE.Mesh(
        this.dropGeometry,
        new THREE.MeshBasicMaterial({
          color: DROP_COLOR,
          transparent: true,
          opacity: 0.42,
          depthWrite: false,
          side: THREE.DoubleSide,
          blending: THREE.AdditiveBlending,
          fog: false,
        }),
      );
      beam.position.set(this.drop.x, this.drop.y + 12, this.drop.z);
      beam.renderOrder = 4;
      this.group.add(beam);
      this.meshes.push(beam);
      this.beams.push({ mesh: beam, x: this.drop.x, z: this.drop.z });

      const diamond = new THREE.Mesh(
        this.diamondGeometry,
        new THREE.MeshBasicMaterial({
          color: DROP_COLOR,
          transparent: true,
          opacity: 0.95,
          depthTest: false,
          depthWrite: false,
          fog: false,
        }),
      );
      diamond.position.set(this.drop.x, this.drop.y + 25, this.drop.z);
      diamond.renderOrder = 12;
      this.group.add(diamond);
      this.meshes.push(diamond);
    }
  }

  dispose(): void {
    for (const mesh of this.meshes) {
      mesh.removeFromParent();
      if (Array.isArray(mesh.material)) {
        for (const material of mesh.material) material.dispose();
      } else {
        mesh.material.dispose();
      }
    }
    this.meshes.length = 0;
    this.beamGeometry.dispose();
    this.dropGeometry.dispose();
    this.diamondGeometry.dispose();
    this.ringGeometry.dispose();
    this.group.removeFromParent();
  }
}
