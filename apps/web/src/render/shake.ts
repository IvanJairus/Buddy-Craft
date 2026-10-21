/**
 * Camera shake (task T-044, plan §4.9).
 *
 * A deterministic damped oscillation, evaluated into a caller-owned vector so
 * the frame path allocates nothing. Juice decides whether shake is allowed;
 * this module only turns amplitude and time into an offset.
 */

export interface ShakeOffset {
  x: number;
  y: number;
  z: number;
}

export class CameraShake {
  private start = 0;
  private until = 0;
  private amplitude = 0;
  private phase = 0;

  /** Add or replace a shake. `amplitude` is metres at t = 0. */
  add(nowMs: number, amplitude: number, durationMs = 120): void {
    if (!(amplitude > 0) || durationMs <= 0) return;
    this.start = nowMs;
    this.until = nowMs + durationMs;
    this.amplitude = Math.min(0.06, amplitude);
    this.phase = (nowMs * 0.017) % (Math.PI * 2);
  }

  active(nowMs: number): boolean {
    return nowMs < this.until;
  }

  /** Write the current offset into `out`. Zero when inactive. */
  sample(nowMs: number, out: ShakeOffset): void {
    if (nowMs >= this.until || this.amplitude === 0) {
      out.x = 0;
      out.y = 0;
      out.z = 0;
      return;
    }
    const t = (nowMs - this.start) / 1000;
    const falloff = 1 - (nowMs - this.start) / (this.until - this.start);
    const a = this.amplitude * falloff;
    out.x = Math.sin(t * 91 + this.phase) * a;
    out.y = Math.sin(t * 137 + this.phase * 1.7) * a * 0.6;
    out.z = Math.sin(t * 73 + this.phase * 0.6) * a * 0.4;
  }

  clear(): void {
    this.until = 0;
    this.amplitude = 0;
  }
}
