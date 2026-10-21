/**
 * Prediction glue (task T-041, plan §6.4).
 *
 * The rollback is Rust: `client-wasm::predict` owns the 128-entry input ring,
 * replays unacknowledged inputs and decides whether a snapshot is within the
 * 5 cm threshold. This module is the browser half. It numbers outgoing inputs
 * so a snapshot can be matched to them, and it hides a correction behind the
 * exponential visual residual. Duplicating movement here would drift from the
 * server on the next tuning change, so this file never simulates a body.
 */

export const SMOOTHING_SECONDS = 0.15;
export const MAX_VISUAL_STEP_METERS = 0.05;

export interface Point {
  x: number;
  y: number;
  z: number;
}

export class VisualResidual {
  private x = 0;
  private y = 0;
  private z = 0;

  /** Start from the position already on screen and the corrected one. */
  add(rendered: Point, corrected: Point): void {
    this.x += rendered.x - corrected.x;
    this.y += rendered.y - corrected.y;
    this.z += rendered.z - corrected.z;
  }

  /** Apply the residual to an authoritative position. */
  apply(position: Point): Point {
    return {
      x: position.x + this.x,
      y: position.y + this.y,
      z: position.z + this.z,
    };
  }

  /**
   * Decay by `exp(-dt / 0.15)`, with the per-frame movement clamped so a one
   * metre correction glides instead of snapping. The clamp is what makes the
   * first frame after a correction visible as motion, not as a jump.
   */
  decay(dtSeconds: number): void {
    const length = Math.hypot(this.x, this.y, this.z);
    if (length <= 0.0001) {
      this.x = 0;
      this.y = 0;
      this.z = 0;
      return;
    }
    const factor = Math.exp(-Math.max(0, dtSeconds) / SMOOTHING_SECONDS);
    const reduction = length * (1 - factor);
    const keep =
      reduction > MAX_VISUAL_STEP_METERS
        ? 1 - MAX_VISUAL_STEP_METERS / length
        : factor;
    this.x *= keep;
    this.y *= keep;
    this.z *= keep;
  }

  get length(): number {
    return Math.hypot(this.x, this.y, this.z);
  }

  clear(): void {
    this.x = 0;
    this.y = 0;
    this.z = 0;
  }
}

/**
 * The local player's prediction state as the browser sees it. It owns the
 * sequence numbers that go on the wire, not the simulation or its input ring.
 */
export class PredictionGlue {
  private sequence = 0;
  private acknowledged = 0;
  private corrected = 0;
  private readonly residual = new VisualResidual();

  /** Number the next input frame; the returned seq goes on the wire. */
  beginInput(): number {
    this.sequence += 1;
    return this.sequence;
  }

  /** A snapshot acknowledged every input up to `seq`. */
  acknowledge(seq: number): void {
    if (seq > this.acknowledged) {
      this.acknowledged = Math.min(seq, this.sequence);
    }
  }

  /**
   * Rust decided this snapshot was more than 5 cm away. `rendered` is where
   * the camera was before the replay, `corrected` is the new body position.
   */
  correction(rendered: Point, corrected: Point): void {
    this.residual.add(rendered, corrected);
    this.corrected += 1;
  }

  /** Advance the smoothing by one frame. */
  update(dtSeconds: number): void {
    this.residual.decay(dtSeconds);
  }

  /** The position the camera should use: body position plus residual. */
  render(bodyPosition: Point): Point {
    return this.residual.apply(bodyPosition);
  }

  /** Inputs sent but not acknowledged yet. */
  get pending(): number {
    return this.sequence - this.acknowledged;
  }

  get corrections(): number {
    return this.corrected;
  }

  get residualLength(): number {
    return this.residual.length;
  }

  get lastSequence(): number {
    return this.sequence;
  }

  reset(): void {
    this.sequence = 0;
    this.acknowledged = 0;
    this.corrected = 0;
    this.residual.clear();
  }
}
