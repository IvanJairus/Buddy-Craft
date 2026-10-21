/**
 * Client entry point (tasks T-031 and T-032).
 *
 * Boot order: resolve a tier from the URL, then storage, otherwise run the
 * 2-second benchmark. Then load the wasm bridge, initialise it, and start
 * streaming terrain around the camera. The benchmark happens before the play
 * loop and before `window.__cbReady`, so the tests observe a fully configured
 * renderer rather than a moving target.
 *
 * The `?quality=low|med|high` override exists for tests and for a player who
 * wants to pin a tier. It is persisted, because "user-overridable" in §8.5
 * means the choice should survive a reload. `?chunks=off` keeps the T-031
 * renderer specs measuring a clear frame instead of a streaming world.
 */

import * as THREE from "three";
import wasmUrl from "./wasm/client_wasm.wasm?url";
import { ClientWasm, loadClientWasm } from "./wasm/loader";
import { ChunkManager, clientMeshSource, type ChunkInfo } from "./render/chunkManager";
import { GameRenderer } from "./render/renderer";
import type { RendererInfo } from "./render/renderer";
import {
  parseTier,
  readStoredTier,
  tierForFrameTime,
  writeStoredTier,
  type QualityTier,
} from "./render/quality";

/** Demo seed until the room handshake supplies one. */
const DEMO_SEED = 0x00c0_ffee;

interface GrowResult {
  previousPages: number;
  bytes: number;
  viewsAlive: boolean;
}

interface TestApi {
  info(): RendererInfo;
  chunks(): ChunkInfo | null;
  stream(x: number, z: number, budget?: number): ChunkInfo | null;
  setFrameTime(ms: number | null): void;
  setTier(tier: QualityTier): void;
  growWasm(pages: number): GrowResult;
  leakCycle(cycles: number): void;
  wasm(): { viewsAlive: boolean; memoryBytes: number };
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
const chunksEnabled = query.get("chunks") !== "off";
let tier: QualityTier | null = queryTier ?? readStoredTier(window.localStorage);
const renderer = new GameRenderer({ canvas, tier: tier ?? "MED" });
let client: ClientWasm | null = null;
let chunks: ChunkManager | null = null;

async function boot(): Promise<void> {
  if (tier === null) {
    const averageFrameMs = await renderer.benchmark(2000);
    tier = tierForFrameTime(averageFrameMs);
    writeStoredTier(window.localStorage, tier);
  } else if (queryTier !== null) {
    writeStoredTier(window.localStorage, queryTier);
  }
  renderer.setTier(tier);

  const exports = await loadClientWasm(wasmUrl);
  const wasmClient = new ClientWasm(exports, { dev: true });
  wasmClient.init(DEMO_SEED, null, 0);
  client = wasmClient;

  // The world is 16 x 16 columns; (128, 128) is the middle of column (8, 8).
  renderer.camera.position.set(128, 78, 128);
  renderer.camera.lookAt(128, 48, 96);

  if (chunksEnabled) {
    chunks = new ChunkManager(renderer.scene, clientMeshSource(wasmClient), {
      loadRadius: 6,
      keepRadius: 7,
    });
  }

  const input = new Uint8Array(4);
  window.__cbTest = {
    info: () => renderer.info(),
    chunks: () => chunks?.info() ?? null,
    stream: (x, z, budget = 1024) => {
      chunks?.update(x, z, budget);
      return chunks?.info() ?? null;
    },
    setFrameTime: (ms) => renderer.setForcedFrameTime(ms),
    setTier: (next) => {
      renderer.setTier(next);
      writeStoredTier(window.localStorage, next);
    },
    growWasm: (pages) => {
      if (client === null) {
        throw new Error("wasm is not initialised");
      }
      const grown = client.growMemory(pages);
      return { ...grown, viewsAlive: client.viewsAlive() };
    },
    leakCycle: (cycles) => {
      if (client === null) {
        throw new Error("wasm is not initialised");
      }
      const parent = new THREE.Group();
      const temporary = new ChunkManager(parent, clientMeshSource(client), {
        loadRadius: 0,
        keepRadius: 0,
      });
      for (let i = 0; i < cycles; i += 1) {
        const cx = i % 2 === 0 ? 8 : 9;
        temporary.update(cx * 16 + 8, 8 * 16 + 8, 4);
      }
      temporary.dispose();
    },
    wasm: () => ({
      viewsAlive: client?.viewsAlive() ?? false,
      memoryBytes: client?.memoryBytes() ?? 0,
    }),
  };
  window.__cbReady = true;

  const loop = (now: number): void => {
    client?.tick(16, input);
    if (chunks !== null) {
      chunks.update(renderer.camera.position.x, renderer.camera.position.z, 1);
    }
    renderer.frame(now);
    window.requestAnimationFrame(loop);
  };
  window.requestAnimationFrame(loop);
}

void boot();
