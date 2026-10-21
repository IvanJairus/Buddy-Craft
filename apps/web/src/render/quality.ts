/**
 * Quality tiers and the boot benchmark policy (plan §8.5, task T-031).
 *
 * The plan pins the numbers for the three tiers; it does not pin the frame-time
 * thresholds that choose one during the boot benchmark. The thresholds here are
 * calibrated to a 60 Hz display, whose frame is 16.7 ms: keeping up is HIGH,
 * 18 to 25 ms is MED, and slower than that is LOW. A HIGH threshold below
 * 16.7 ms would be unreachable on every 60 Hz screen, which is how the first
 * version of this policy managed to classify a perfect 60 fps benchmark as MED.
 *
 * Fog end is not in the §8.5 row, which names only fog start. It is set to the
 * render distance in metres (distance chunks x 16 m), which is also the camera
 * far plane: the fog hides the streaming boundary at exactly the edge of the
 * rendered world.
 */

export type QualityTier = "LOW" | "MED" | "HIGH";

export interface QualitySettings {
  /** Chunks visible in each direction. */
  readonly renderDistance: 4 | 5 | 6;
  /** Maximum live particles. */
  readonly particleCap: 64 | 128 | 256;
  /** Whether buddy and player limbs animate. */
  readonly entityAnimation: boolean;
  /** Linear fog near plane, metres. */
  readonly fogStart: 40 | 52 | 64;
  /** Linear fog far plane, metres. Also the camera far plane. */
  readonly fogEnd: 64 | 80 | 96;
}

export const QUALITY: Record<QualityTier, QualitySettings> = Object.freeze({
  LOW: Object.freeze({
    renderDistance: 4,
    particleCap: 64,
    entityAnimation: false,
    fogStart: 40,
    fogEnd: 64,
  }),
  MED: Object.freeze({
    renderDistance: 5,
    particleCap: 128,
    entityAnimation: true,
    fogStart: 52,
    fogEnd: 80,
  }),
  HIGH: Object.freeze({
    renderDistance: 6,
    particleCap: 256,
    entityAnimation: true,
    fogStart: 64,
    fogEnd: 96,
  }),
});

export const QUALITY_STORAGE_KEY = "buddycraft.quality";

/** Parse a tier name case-insensitively, or `null` for anything unknown. */
export function parseTier(value: string | null | undefined): QualityTier | null {
  if (value === null || value === undefined) {
    return null;
  }
  const upper = value.toUpperCase();
  return upper === "LOW" || upper === "MED" || upper === "HIGH" ? upper : null;
}

/** Read the persisted tier, tolerating a storage that throws (private mode). */
export function readStoredTier(storage: Pick<Storage, "getItem">): QualityTier | null {
  try {
    return parseTier(storage.getItem(QUALITY_STORAGE_KEY));
  } catch {
    return null;
  }
}

/** Persist a tier; a failed write is not fatal. */
export function writeStoredTier(storage: Pick<Storage, "setItem">, tier: QualityTier): void {
  try {
    storage.setItem(QUALITY_STORAGE_KEY, tier);
  } catch {
    // Private mode or a full quota: the session still runs, it just re-detects.
  }
}

/** Map the boot benchmark's average frame time to a tier. */
export function tierForFrameTime(avgMs: number): QualityTier {
  if (!Number.isFinite(avgMs) || avgMs <= 0) {
    return "MED";
  }
  if (avgMs > 25) {
    return "LOW";
  }
  if (avgMs > 18) {
    return "MED";
  }
  return "HIGH";
}
