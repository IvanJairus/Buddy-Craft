/**
 * Local first-person view (playability pass, 2026-10-22).
 *
 * Yaw is a byte turn and pitch is ±64 for ±90 degrees, matching
 * `sim::input::look_direction`. Three.js cameras look down -Z, while yaw 0
 * faces +Z here, so the camera rotation adds a half turn.
 */

export const EYE_HEIGHT = 1.62;
const TAU = Math.PI * 2;

export interface EyePose {
  x: number;
  y: number;
  z: number;
  rotationX: number;
  rotationY: number;
}

export function yawRadians(yaw: number): number {
  return ((((yaw % 256) + 256) % 256) * TAU) / 256;
}

export function pitchRadians(pitch: number): number {
  return (Math.max(-64, Math.min(64, pitch)) * Math.PI) / 128;
}

export function eyePose(x: number, y: number, z: number, yaw: number, pitch: number): EyePose {
  return {
    x,
    y: y + EYE_HEIGHT,
    z,
    rotationX: pitchRadians(pitch),
    rotationY: yawRadians(yaw) + Math.PI,
  };
}
