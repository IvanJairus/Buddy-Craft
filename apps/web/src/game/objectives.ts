/**
 * Shared objective data: the worldgen bases the map and compass draw.
 *
 * The layout is packed by `cb_bases` in client-wasm as `u16 count` followed by
 * five `i16` per base: `base_x, base_z, pad_x, pad_z, ground_y`.
 */

export interface BaseRow {
  x: number;
  z: number;
  padX: number;
  padZ: number;
  y: number;
}

export interface DropMark {
  x: number;
  y: number;
  z: number;
  telegraph: boolean;
}

const BASE_BYTES = 10;

export function parseBaseTable(view: DataView): BaseRow[] {
  if (view.byteLength < 2) return [];
  const count = view.getUint16(0, true);
  const available = Math.floor((view.byteLength - 2) / BASE_BYTES);
  const rows: BaseRow[] = [];
  for (let i = 0; i < Math.min(count, available); i += 1) {
    const at = 2 + i * BASE_BYTES;
    rows.push({
      x: view.getInt16(at, true),
      z: view.getInt16(at + 2, true),
      padX: view.getInt16(at + 4, true),
      padZ: view.getInt16(at + 6, true),
      y: view.getInt16(at + 8, true),
    });
  }
  return rows;
}

/** The 8 x 8 stack pad, centred on `padX, padZ`. */
export function padContains(base: BaseRow, x: number, z: number): boolean {
  return (
    x >= base.padX - 4 &&
    x < base.padX + 4 &&
    z >= base.padZ - 4 &&
    z < base.padZ + 4
  );
}

export function padHeightAllows(base: BaseRow, y: number): boolean {
  return y >= base.y && y <= base.y + 10;
}

export interface Bearing {
  distance: number;
  /** Radians, zero ahead and positive to the screen-right. */
  angle: number;
}

/**
 * Distance and screen-space bearing from the player's facing.
 *
 * `rightX/rightZ` is the camera's world-space right axis, so the result can
 * rotate a DOM arrow without repeating the yaw convention here.
 */
export function bearingTo(
  fromX: number,
  fromZ: number,
  forwardX: number,
  forwardZ: number,
  rightX: number,
  rightZ: number,
  toX: number,
  toZ: number,
): Bearing {
  const dx = toX - fromX;
  const dz = toZ - fromZ;
  return {
    distance: Math.hypot(dx, dz),
    angle: Math.atan2(dx * rightX + dz * rightZ, dx * forwardX + dz * forwardZ),
  };
}
