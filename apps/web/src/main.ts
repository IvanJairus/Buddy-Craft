/**
 * Client entry point (task T-031).
 *
 * Boot order: resolve a tier from the URL, then storage, otherwise run the
 * 2-second benchmark. The benchmark happens before the play loop and before
 * `window.__cbReady`, so the tests observe a fully configured renderer rather
 * than a moving target.
 *
 * The `?quality=low|med|high` override exists for tests and for a player who
 * wants to pin a tier. It is persisted, because "user-overridable" in §8.5
 * means the choice should survive a reload.
 */

import { GameRenderer } from "./render/renderer";
import type { RendererInfo } from "./render/renderer";
import {
  parseTier,
  readStoredTier,
  tierForFrameTime,
  writeStoredTier,
  type QualityTier,
} from "./render/quality";

interface TestApi {
  info(): RendererInfo;
  setFrameTime(ms: number | null): void;
  setTier(tier: QualityTier): void;
}

declare global {
  interface Window {
    __cbReady?: boolean;
    __cbTest?: TestApi;
  }
}

const canvas = document.querySelector<HTMLCanvasElement>("#game");
if (canvas === null) {
  throw new Error("canvas #game is missing from index.html");
}

const query = new URLSearchParams(window.location.search);
const queryTier = parseTier(query.get("quality"));
let tier: QualityTier | null = queryTier ?? readStoredTier(window.localStorage);
const renderer = new GameRenderer({ canvas, tier: tier ?? "MED" });

async function boot(): Promise<void> {
  if (tier === null) {
    const averageFrameMs = await renderer.benchmark(2000);
    tier = tierForFrameTime(averageFrameMs);
    writeStoredTier(window.localStorage, tier);
  } else if (queryTier !== null) {
    writeStoredTier(window.localStorage, queryTier);
  }

  renderer.setTier(tier);
  window.__cbTest = {
    info: () => renderer.info(),
    setFrameTime: (ms) => renderer.setForcedFrameTime(ms),
    setTier: (next) => {
      renderer.setTier(next);
      writeStoredTier(window.localStorage, next);
    },
  };
  window.__cbReady = true;

  const loop = (now: number): void => {
    renderer.frame(now);
    window.requestAnimationFrame(loop);
  };
  window.requestAnimationFrame(loop);
}

void boot();
