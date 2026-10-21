/**
 * The three.js renderer, its resolution scaler and the boot benchmark
 * (plan §8.5, task T-031).
 *
 * The renderer owns exactly three things: a WebGL2 context, a scene with the
 * tier's fog and camera far plane, and a resolution scale in `[0.55, 1]`. The
 * scale is driven by an exponential average of frame time, smoothed toward
 * `16.7 / avg` with a hard floor, so a slow device loses pixels instead of
 * frames and never falls below 55% of its starting resolution.
 *
 * `GameRenderer` is also where the DPR clamp lives: the drawing buffer is at
 * most 2x CSS pixels, regardless of what `devicePixelRatio` reports. A phone
 * that claims 3 or 4 would otherwise spend its frame budget on pixels a
 * six-inch screen cannot resolve.
 */

import * as THREE from "three";
import { QUALITY, type QualitySettings, type QualityTier } from "./quality";

const TARGET_FRAME_MS = 1000 / 60;
const MIN_SCALE = 0.55;
const MAX_SCALE = 1;
const SCALE_SMOOTHING = 0.15;
const FRAME_EMA = 0.1;
const SKY_COLOR = 0x8fb8d8;

export interface RendererInfo {
  readonly webgl2: boolean;
  readonly pixelRatio: number;
  readonly pixelRatioAtBoot: number;
  readonly basePixelRatio: number;
  readonly scale: number;
  readonly tier: QualityTier;
  readonly settings: QualitySettings;
  readonly frames: number;
  readonly frameMsAvg: number;
  readonly fogNear: number;
  readonly fogFar: number;
  readonly cameraFar: number;
  readonly drawCalls: number;
  readonly triangles: number;
}

export interface RendererOptions {
  readonly canvas: HTMLCanvasElement;
  readonly tier: QualityTier;
}

export class GameRenderer {
  readonly renderer: THREE.WebGLRenderer;
  readonly scene: THREE.Scene;
  readonly camera: THREE.PerspectiveCamera;
  readonly basePixelRatio: number;
  readonly pixelRatioAtBoot: number;

  private readonly canvas: HTMLCanvasElement;
  private readonly onResize = (): void => this.resize();
  private tier: QualityTier;
  private settings: QualitySettings;
  private scale = 1;
  private adaptive = true;
  private frameMsAvg = TARGET_FRAME_MS;
  private forcedFrameMs: number | null = null;
  private frames = 0;
  private lastFrameMs = 0;

  constructor(options: RendererOptions) {
    this.canvas = options.canvas;
    this.tier = options.tier;
    this.settings = QUALITY[options.tier];

    this.renderer = new THREE.WebGLRenderer({
      canvas: options.canvas,
      antialias: false,
      powerPreference: "high-performance",
    });
    this.renderer.setClearColor(SKY_COLOR, 1);

    this.basePixelRatio = Math.min(window.devicePixelRatio || 1, 2);
    this.renderer.setPixelRatio(this.basePixelRatio);
    this.pixelRatioAtBoot = this.renderer.getPixelRatio();

    this.scene = new THREE.Scene();
    this.scene.background = new THREE.Color(SKY_COLOR);
    this.camera = new THREE.PerspectiveCamera(70, 1, 0.1, this.settings.fogEnd);
    this.camera.position.set(0, 2, 0);

    this.applyQuality();
    this.resize();
    window.addEventListener("resize", this.onResize);
  }

  /** The tier whose numbers are currently applied. */
  get currentTier(): QualityTier {
    return this.tier;
  }

  /** The live quality settings. Later systems read particleCap and entityAnimation. */
  get quality(): QualitySettings {
    return this.settings;
  }

  /** Drawing-buffer pixels per CSS pixel, including the resolution scale. */
  get pixelRatio(): number {
    return this.renderer.getPixelRatio();
  }

  /** The adaptive scale in `[0.55, 1]`. */
  get resolutionScale(): number {
    return this.scale;
  }

  /** Frames rendered since construction, for the 60 fps check. */
  get frameCount(): number {
    return this.frames;
  }

  setTier(tier: QualityTier): void {
    this.tier = tier;
    this.settings = QUALITY[tier];
    this.applyQuality();
  }

  /** Turn the adaptive scaler off for a benchmark, or back on for play. */
  setAdaptive(enabled: boolean): void {
    this.adaptive = enabled;
    if (enabled) {
      this.frameMsAvg = TARGET_FRAME_MS;
      this.lastFrameMs = 0;
    }
  }

  /**
   * Replace the measured frame time with a synthetic value, or `null` to go
   * back to real time. The Playwright dynamic-resolution spec uses this: it is
   * the only way to test a 25 ms device on a 60 Hz runner.
   */
  setForcedFrameTime(ms: number | null): void {
    this.forcedFrameMs = ms;
  }

  /** Advance one browser frame. Called from the rAF loop. */
  frame(nowMs: number): void {
    const delta =
      this.lastFrameMs === 0 ? TARGET_FRAME_MS : Math.max(1, nowMs - this.lastFrameMs);
    this.lastFrameMs = nowMs;

    const sample = this.forcedFrameMs ?? delta;
    if (this.adaptive) {
      this.updateScale(sample);
    }
    this.renderer.render(this.scene, this.camera);
    this.frames += 1;
  }

  /**
   * Run the boot benchmark: a fixed 2-second loop over a lit, instanced scene,
   * returning the average frame time in milliseconds.
   *
   * The scene is deliberately not a clear colour. A device that can clear the
   * screen at 60 Hz has told us nothing about whether it can draw the game;
   * 256 instanced boxes are a small but real raster load.
   */
  async benchmark(durationMs = 2000): Promise<number> {
    const wasAdaptive = this.adaptive;
    this.setAdaptive(false);

    const scene = new THREE.Scene();
    scene.background = new THREE.Color(SKY_COLOR);
    const geometry = new THREE.BoxGeometry(1, 1, 1);
    const material = new THREE.MeshBasicMaterial({ color: 0x7fa863 });
    const mesh = new THREE.InstancedMesh(geometry, material, 256);
    const matrix = new THREE.Matrix4();
    for (let i = 0; i < 256; i += 1) {
      matrix.makeTranslation((i % 16) - 8, Math.floor(i / 16) * 0.35, (i % 7) - 3);
      mesh.setMatrixAt(i, matrix);
    }
    mesh.instanceMatrix.needsUpdate = true;
    scene.add(mesh);

    const camera = new THREE.PerspectiveCamera(
      70,
      this.canvas.clientWidth / Math.max(1, this.canvas.clientHeight),
      0.1,
      96,
    );
    camera.position.set(0, 10, 16);
    camera.lookAt(0, 2, 0);

    const start = performance.now();
    let last = start;
    let frames = 0;
    let total = 0;

    await new Promise<void>((resolve) => {
      const step = (now: number): void => {
        total += Math.max(0, now - last);
        last = now;
        frames += 1;
        this.renderer.render(scene, camera);
        if (now - start >= durationMs) {
          resolve();
        } else {
          window.requestAnimationFrame(step);
        }
      };
      window.requestAnimationFrame(step);
    });

    scene.remove(mesh);
    geometry.dispose();
    material.dispose();
    this.setAdaptive(wasAdaptive);
    this.lastFrameMs = 0;
    return frames > 0 ? total / frames : TARGET_FRAME_MS;
  }

  dispose(): void {
    window.removeEventListener("resize", this.onResize);
    this.renderer.dispose();
  }

  info(): RendererInfo {
    const fog = this.scene.fog;
    return {
      webgl2:
        typeof WebGL2RenderingContext !== "undefined" &&
        this.renderer.getContext() instanceof WebGL2RenderingContext,
      pixelRatio: this.renderer.getPixelRatio(),
      pixelRatioAtBoot: this.pixelRatioAtBoot,
      basePixelRatio: this.basePixelRatio,
      scale: this.scale,
      tier: this.tier,
      settings: this.settings,
      frames: this.frames,
      frameMsAvg: this.frameMsAvg,
      fogNear: fog instanceof THREE.Fog ? fog.near : 0,
      fogFar: fog instanceof THREE.Fog ? fog.far : 0,
      cameraFar: this.camera.far,
      drawCalls: this.renderer.info.render.calls,
      triangles: this.renderer.info.render.triangles,
    };
  }

  private applyQuality(): void {
    this.scene.fog = new THREE.Fog(SKY_COLOR, this.settings.fogStart, this.settings.fogEnd);
    this.camera.far = this.settings.fogEnd;
    this.camera.updateProjectionMatrix();
    this.applyResolution();
  }

  private applyResolution(): void {
    this.renderer.setPixelRatio(this.basePixelRatio * this.scale);
  }

  private updateScale(frameMs: number): void {
    const sample = Math.min(100, Math.max(1, frameMs));
    this.frameMsAvg = this.frameMsAvg * (1 - FRAME_EMA) + sample * FRAME_EMA;

    const target = Math.min(MAX_SCALE, Math.max(MIN_SCALE, TARGET_FRAME_MS / this.frameMsAvg));
    const next = this.scale + (target - this.scale) * SCALE_SMOOTHING;
    if (Math.abs(next - this.scale) < 0.0005) {
      return;
    }
    this.scale = next;
    this.applyResolution();
  }

  private resize(): void {
    const width = Math.max(1, window.innerWidth);
    const height = Math.max(1, window.innerHeight);
    this.renderer.setSize(width, height, false);
    this.camera.aspect = width / height;
    this.camera.updateProjectionMatrix();
  }
}
