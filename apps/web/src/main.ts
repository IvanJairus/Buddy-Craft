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
import {
  EntityRenderer,
  FLAG_ALIVE,
  FLAG_ATTACKING,
  type EntityInfo,
  type EntityValue,
} from "./render/entities";
import { ParticleSystem, type ParticleInfo } from "./render/particles";
import { Juice, EVENT_HIT, type JuiceStats } from "./render/juice";
import { AudioEngine, type AudioStats } from "./audio/engine";
import { Interaction, type PendingEdit, type RayHit, type VisualState } from "./game/interaction";
import {
  DesktopInput,
  loadKeyMap,
  type InputAction,
} from "./game/inputDesktop";
import { TouchInput, type TouchState } from "./game/inputTouch";
import { errorMessage, showFatalScreen, showRttOverlay } from "./game/net/client";
import { ChunkSync } from "./game/net/chunkSync";
import { SnapshotDecoder } from "./game/net/snapshot";
import { Interpolator } from "./game/interp";
import { OfflineSession } from "./game/offline";
import { Hud, type HudStats } from "./ui/hud";
import { RecipeBookUi, type RecipeBookInfo } from "./ui/recipeBook";
import { LobbyScreen } from "./ui/lobby";
import {
  ScreenManager,
  buildBoot,
  buildMainMenu,
  buildResults,
  type ScreenId,
} from "./ui/screens";
import { SettingsScreen } from "./ui/settings";
import { Store, initialHudState, type HudState, type HudStance } from "./ui/store";
import { GhostOverlay, type GhostInfo } from "./render/ghostOverlay";
import type { RecipeRow } from "./wasm/loader";
import { DESC_EVENT_COUNT } from "./wasm/loader";
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
  entities(): EntityInfo | null;
  entityBenchmark(frames?: number): (EntityInfo & { averageMs: number }) | null;
  particles(): ParticleInfo | null;
  particleBenchmark(): { spawned: ParticleInfo; after: ParticleInfo } | null;
  surface(x: number, z: number): number;
  aim(origin: [number, number, number], direction: [number, number, number]): {
    hit: RayHit | null;
    visuals: VisualState | null;
  };
  placeBlock(block: number): number;
  breakBlock(): number;
  pendingEdits(): PendingEdit[];
  ackEdit(seq: number, accepted: boolean): boolean;
  input(): {
    polls: number;
    ticks: number;
    frame: number[];
    yaw: number;
    pitch: number;
    slot: number;
    pointerLocked: boolean;
    sensitivity: number;
    invertY: boolean;
    lastAction: InputAction | null;
  };
  setSensitivity(value: number): void;
  setInvertY(value: boolean): void;
  touch(): TouchState | null;
  hudUpdate(patch: Partial<HudState>): void;
  hudState(): HudState;
  hudStats(): HudStats;
  hudToast(text: string): void;
  recipeRows(): RecipeRow[];
  recipesInfo(): RecipeBookInfo;
  toggleRecipes(elapsed?: number): void;
  showNearMiss(
    blocks: Array<[number, number, number]>,
    elapsed: number,
  ): { recipeId: number; name: string; missing: number } | null;
  clearNearMiss(): void;
  ghostShow(recipeId: number, origin: [number, number, number]): GhostInfo | null;
  ghostHide(): GhostInfo;
  screen(): ScreenId;
  showScreen(id: ScreenId): void;
  netFatal(code: number, detail?: string): string;
  netRtt(ms: number): void;
  applyChunkDiff(frame: number[]): {
    ok: boolean;
    applied: number;
    refreshed: number;
    chunks: ChunkInfo | null;
  };
  snapshotView(
    frame: number[],
    nowMs: number,
  ): { decoded: number; players: number; buddies: number; entity: EntityInfo | null };
  juiceStats(): JuiceStats;
  juiceEvent(kind: number, a: number, b: number, c: number): JuiceStats;
  juiceHitstop(ms?: number): JuiceStats;
  juiceDamage(amount: number, x: number, y: number, z: number): JuiceStats;
  juiceBenchmark(frames?: number): { averageMs: number; stats: JuiceStats };
  serverMessage(frame: number[]): boolean;
  audioStats(): AudioStats;
  audioUnlock(): Promise<boolean>;
  audioCue(cue: string, x?: number, y?: number, z?: number): number;
  audioRumble(level: number): AudioStats;
  audioPanner(): { x: number; y: number; z: number } | null;
  audioEvent(kind: number, a: number, b: number): void;
  offlineStart(mode?: number, teams?: number): boolean;  offlineStats(): {
    active: boolean;
    snapshot: boolean;
    state: string;
    players: number;
    buddies: number;
    decoded: number;
    hash: number;
    ended: boolean;
    x: number;
    y: number;
    z: number;
  };
  offlineAdvance(ticks: number): void;
  offlineInput(frame: number[]): void;
  offlineStop(): void;
}

declare global {
  interface Window {
    __cbReady?: boolean;
    __cbTest?: TestApi;
  }
}

const foundCanvas = document.querySelector<HTMLCanvasElement>("#game");
if (foundCanvas === null) {
  throw new Error("canvas #game is missing from index.html");
}
const canvas: HTMLCanvasElement = foundCanvas;

const query = new URLSearchParams(window.location.search);
const queryTier = parseTier(query.get("quality"));
const clearScene = query.get("scene") === "clear";
const chunksEnabled = !clearScene && query.get("chunks") !== "off";
let tier: QualityTier | null = queryTier ?? readStoredTier(window.localStorage);
const renderer = new GameRenderer({ canvas, tier: tier ?? "MED" });
let client: ClientWasm | null = null;
let chunks: ChunkManager | null = null;
let entities: EntityRenderer | null = null;
let particles: ParticleSystem | null = null;
let offline: OfflineSession | null = null;
let offlineActive = false;
let offlineSnapshot = false;
let lastOfflineInput = 0;
let lastAdvanced = { ended: false, hash: 0 };
const ZERO_FRAME = new Uint8Array(4);
let interaction: Interaction | null = null;
let desktop: DesktopInput | null = null;
let touch: TouchInput | null = null;
let screens: ScreenManager | null = null;
let tickCalls = 0;
let elapsedSeconds = 0;
const stanceOrder: HudStance[] = ["follow", "hold", "assault"];

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
  const chunkSync = new ChunkSync();
  const snapshotDecoder = new SnapshotDecoder();
  const interpolator = new Interpolator(snapshotDecoder.playerTeam, snapshotDecoder.buddyRole);
  entities = clearScene ? null : new EntityRenderer(renderer.scene);
  particles = clearScene ? null : new ParticleSystem(renderer.scene, 256);
  const juice = new Juice({
    camera: renderer.camera,
    scene: renderer.scene,
    entities,
    particles,
    reducedMotion: () => document.body.classList.contains("cb-reduced-motion"),
    surfaceAt: (x, z) => chunks?.surfaceAt(x, z) ?? 40,
  });
  const tierName = renderer.currentTier.toLowerCase();
  const audio = new AudioEngine(tierName === "low" ? "low" : tierName === "high" ? "high" : "med");
  window.addEventListener(
    "pointerdown",
    () => {
      void audio.unlock();
    },
    { once: true },
  );
  if (chunks !== null) {
    interaction = new Interaction(renderer.scene, chunks.voxelSource(), {
      localEdit: (x, y, z, block, action) =>
        offlineActive && offline !== null
          ? offline.sendEdit(x, y, z, block, action)
          : wasmClient.localEdit(x, y, z, block, action),
    });
  }

  const recipes = wasmClient.recipes();
  const recipeBook = new RecipeBookUi(document.body, recipes);
  const ghost = new GhostOverlay(renderer.scene);
  const hudStore = new Store<HudState>(initialHudState());
  const hud = new Hud(document.body, hudStore);
  let stanceIndex = 0;
  const advanceStance = (): HudStance => {
    stanceIndex = (stanceIndex + 1) % stanceOrder.length;
    const next = stanceOrder[stanceIndex];
    hudStore.set({ stance: next });
    return next;
  };

  const touchMode =
    query.get("touch") === "1" || window.matchMedia("(pointer: coarse)").matches;
  if (touchMode) {
    touch = new TouchInput();
    touch.onStance = (stance) => {
      stanceIndex = stanceOrder.indexOf(stance);
      hudStore.set({ stance });
    };
    if (interaction !== null) {
      touch.onBreak = () => {
        interaction?.breakBlock();
      };
      touch.onPlace = (block) => {
        interaction?.place(offlineActive ? 11 : block);
      };
    }
  } else {
    desktop = new DesktopInput({ canvas, keyMap: loadKeyMap(window.localStorage) });
    desktop.onAction((action) => {
      if (action === "stance") {
        advanceStance();
      } else if (action === "recipes") {
        if (screens?.current() === "match") {
          recipeBook.toggle(elapsedSeconds);
        }
      } else if (action === "menu") {
        recipeBook.close();
        screens?.show("main");
      }
    });
  }

  screens = new ScreenManager(document.body, () => screens?.show("results"));
  screens.register("boot", buildBoot(() => screens?.show("main")));
  screens.register(
    "main",
    buildMainMenu({
      onOnline: () => screens?.show("lobby"),
      onOffline: () => startOffline(),
      onSettings: () => screens?.show("settings"),
    }),
  );
  const lobby = new LobbyScreen({
    onBack: () => screens?.show("main"),
    onStart: () => screens?.show("match"),
  });
  screens.register("lobby", lobby.element);
  const settings = new SettingsScreen({
    onQuality: (tier) => renderer.setTier(tier),
    onSensitivity: (value) => desktop?.setSensitivity(value),
    onInvertY: (value) => desktop?.setInvertY(value),
    onKeyMap: (map) => desktop?.setKeyMap(map),
    onBack: () => screens?.show("main"),
  });
  screens.register("settings", settings.element);
  screens.register(
    "results",
    buildResults({
      onMain: () => {
        offline?.stop();
        offline = null;
        offlineActive = false;
        screens?.show("main");
      },
      onRematch: () => startOffline(),
    }),
  );
  const initialScreen: ScreenId = query.get("screen") === "match" ? "match" : "boot";
  screens.show(initialScreen);

  function startOffline(mode = 1, teams = 2): void {
    offline?.stop();
    offline = null;
    snapshotDecoder.reset();
    interpolator.reset();
    chunkSync.reset();
    offlineActive = true;
    offlineSnapshot = false;
    lastOfflineInput = 0;
    lastAdvanced = { ended: false, hash: 0 };
    offline = new OfflineSession({
      seed: DEMO_SEED,
      mode,
      teams,
      onWelcome: () => {
        hudStore.set({ hp: 100, maxHp: 100, crates: 0, carryCap: 12 });
      },
      onFrame: (bytes) => {
        if (client === null) return;
        client.onServerMessage(bytes);
        if (bytes[0] === 0x87) {
          chunkSync.enqueueFrame(bytes);
          chunkSync.flush(
            renderer.camera.position.x,
            renderer.camera.position.z,
            (patch) => {
              chunks?.refreshAround(patch.cx, patch.cz);
            },
          );
        } else if (bytes[0] === 0x84) {
          const frame = snapshotDecoder.decode(bytes);
          if (frame !== null) {
            interpolator.push(frame, performance.now());
            offlineSnapshot = true;
          }
        } else if (bytes[0] === 0x85) {
          const ack = editAck(bytes);
          if (ack !== null) interaction?.ack(ack.seq, ack.accepted);
        }
      },
      onEnd: () => {
        offlineActive = false;
        offlineSnapshot = false;
        offline?.stop();
        offline = null;
        screens?.show("results");
      },
      onAdvanced: (ended, hash) => {
        lastAdvanced = { ended, hash };
        if (ended) {
          offlineActive = false;
          offlineSnapshot = false;
          offline?.stop();
          offline = null;
          screens?.show("results");
        }
      },
    });
    screens?.show("match");
  }

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
    entities: () => entities?.info() ?? null,
    entityBenchmark: (frames = 60) => {
      if (entities === null) {
        return null;
      }
      const players: EntityValue[] = [];
      const buddies: EntityValue[] = [];
      for (let i = 0; i < 6; i += 1) {
        players.push({
          id: i,
          team: i % 2,
          flags: FLAG_ALIVE | (i === 0 ? FLAG_ATTACKING : 0),
          x: 128 + i,
          y: 40,
          z: 128,
          yaw: i * 0.5,
          pitch: i === 0 ? 0.4 : 0,
          anim: i / 6,
          hp: 100,
          role: 0,
        });
      }
      for (let i = 0; i < 12; i += 1) {
        buddies.push({
          id: 18 + i,
          team: i % 2,
          flags: i === 11 ? 0 : FLAG_ALIVE,
          x: 128 - i,
          y: 40,
          z: 126,
          yaw: 0,
          pitch: 0,
          anim: i === 11 ? 0.5 : i / 12,
          hp: i === 11 ? 0 : 80,
          role: i % 6,
        });
      }
      let total = 0;
      let last = entities.updateFromValues(players, buddies, true);
      for (let frame = 0; frame < frames; frame += 1) {
        last = entities.updateFromValues(players, buddies, true);
        total += last.lastUpdateMs;
      }
      return { ...last, averageMs: total / Math.max(1, frames) };
    },
    particles: () => particles?.info() ?? null,
    particleBenchmark: () => {
      if (particles === null) {
        return null;
      }
      particles.update(0);
      const origin = new THREE.Vector3(128, 42, 128);
      const velocity = new THREE.Vector3(0, 2, 0);
      for (let i = 0; i < 256; i += 1) {
        particles.spawn(origin, velocity, 1, 0.12, 0xffffff);
      }
      const spawned = particles.info();
      for (let frame = 0; frame < 120; frame += 1) {
        particles.update(frame / 60);
      }
      return { spawned, after: particles.info() };
    },
    surface: (x, z) => chunks?.surfaceAt(x, z) ?? 0,
    aim: (origin, direction) => {
      if (interaction === null) {
        return { hit: null, visuals: null };
      }
      const hit = interaction.aim(
        { x: origin[0], y: origin[1], z: origin[2] },
        { x: direction[0], y: direction[1], z: direction[2] },
      );
      return { hit, visuals: interaction.visuals() };
    },
    placeBlock: (block) => interaction?.place(block) ?? 0,
    breakBlock: () => interaction?.breakBlock() ?? 0,
    pendingEdits: () => interaction?.pending() ?? [],
    ackEdit: (seq, accepted) => {
      const known = interaction?.ack(seq, accepted) ?? false;
      client?.ackEdit(seq, accepted);
      return known;
    },
    input: () => {
      if (desktop !== null) {
        return {
          polls: desktop.polls,
          ticks: tickCalls,
          frame: desktop.frameBytes(),
          yaw: desktop.yawValue,
          pitch: desktop.pitchValue,
          slot: desktop.selectedSlot,
          pointerLocked: desktop.pointerLock,
          sensitivity: desktop.sensitivityValue,
          invertY: desktop.invertYValue,
          lastAction: desktop.lastActionValue,
        };
      }
      const state = touch?.state() ?? null;
      return {
        polls: state?.polls ?? 0,
        ticks: tickCalls,
        frame: state?.frame ?? [0, 0, 0, 0],
        yaw: state?.yaw ?? 0,
        pitch: state?.pitch ?? 0,
        slot: state?.selectedSlot ?? 0,
        pointerLocked: false,
        sensitivity: 0,
        invertY: false,
        lastAction: null,
      };
    },
    setSensitivity: (value) => desktop?.setSensitivity(value),
    setInvertY: (value) => desktop?.setInvertY(value),
    touch: () => touch?.state() ?? null,
    hudUpdate: (patch) => hudStore.set(patch),
    hudState: () => ({ ...hudStore.get() }),
    hudStats: () => hud.stats(),
    hudToast: (text) => hud.toast(text),
    recipeRows: () => recipes,
    recipesInfo: () => recipeBook.info(),
    toggleRecipes: (seconds = elapsedSeconds) => recipeBook.toggle(seconds),
    showNearMiss: (blocks, elapsed) => {
      const near = wasmClient.nearMiss(blocks, elapsed);
      if (near === null) {
        recipeBook.clearNearMiss();
        return null;
      }
      const row = recipes.find((candidate) => candidate.id === near.recipeId);
      if (row === undefined) {
        recipeBook.clearNearMiss();
        return null;
      }
      recipeBook.showNearMiss(row, near.missing);
      return { recipeId: row.id, name: row.name, missing: near.missing };
    },
    clearNearMiss: () => recipeBook.clearNearMiss(),
    ghostShow: (recipeId, origin) => {
      const row = recipeBook.row(recipeId);
      if (row === undefined) {
        return null;
      }
      return ghost.show(row.blocks, origin);
    },
    ghostHide: () => {
      ghost.hide();
      return ghost.info();
    },
    screen: () => screens?.current() ?? "match",
    showScreen: (id) => screens?.show(id),
    netFatal: (code, detail = "") => {
      const message = errorMessage(code, detail);
      showFatalScreen(message);
      return message;
    },
    netRtt: (ms) => {
      showRttOverlay(ms);
    },
    applyChunkDiff: (frame) => {
      if (client === null) {
        throw new Error("wasm is not initialised");
      }
      chunkSync.enqueueFrame(Uint8Array.from(frame));
      let ok = true;
      let refreshed = 0;
      const applied = chunkSync.flush(
        renderer.camera.position.x,
        renderer.camera.position.z,
        (patch, raw) => {
          if (raw === null || !client!.onServerMessage(raw)) {
            ok = false;
            return;
          }
          if (chunks !== null) {
            refreshed += chunks.refreshAround(patch.cx, patch.cz);
          }
        },
      );
      return { ok, applied: applied.length, refreshed, chunks: chunks?.info() ?? null };
    },
    snapshotView: (frame, nowMs) => {
      const decoded = snapshotDecoder.decode(Uint8Array.from(frame));
      if (decoded === null) {
        return {
          decoded: snapshotDecoder.decoded,
          players: 0,
          buddies: 0,
          entity: null,
        };
      }
      interpolator.push(decoded, nowMs);
      interpolator.sample(nowMs);
      const entity =
        entities?.updateFromInterp(interpolator, renderer.quality.entityAnimation) ?? null;
      return {
        decoded: snapshotDecoder.decoded,
        players: interpolator.playerCount,
        buddies: interpolator.buddyCount,
        entity,
      };
    },
    juiceStats: () => juice.stats(performance.now()),
    juiceEvent: (kind, a, b, c) => {
      juice.handleEvent(kind, a, b, c, performance.now());
      return juice.stats(performance.now());
    },
    juiceHitstop: (ms = 60) => {
      juice.hitstop(performance.now(), ms);
      return juice.stats(performance.now());
    },
    juiceDamage: (amount, x, y, z) => {
      juice.addDamage(amount, x, y, z, performance.now());
      return juice.stats(performance.now());
    },
    juiceBenchmark: (frames = 60) => {
      const now = performance.now();
      for (let i = 0; i < 18; i += 1) {
        juice.handleEvent(EVENT_HIT, i, 20, 0, now);
      }
      for (let i = 0; i < 40; i += 1) {
        juice.addDamage(20, 128 + (i % 8), 42, 128 + Math.floor(i / 8), now);
      }
      renderer.camera.updateMatrixWorld();
      let total = 0;
      let stats = juice.stats(now);
      for (let frame = 0; frame < frames; frame += 1) {
        stats = juice.update(now + frame * 16.7, 16.7);
        total += stats.lastUpdateMs;
      }
      return { averageMs: total / Math.max(1, frames), stats };
    },
    serverMessage: (frame) => {
      if (client === null) {
        throw new Error("wasm is not initialised");
      }
      return client.onServerMessage(Uint8Array.from(frame));
    },
    audioStats: () => audio.stats(),
    audioUnlock: () => audio.unlock(),
    audioCue: (cue, x, y, z) =>
      audio.play(
        cue,
        x === undefined || y === undefined || z === undefined ? undefined : { x, y, z },
      ),
    audioRumble: (level) => {
      audio.rumble(level);
      return audio.stats();
    },
    audioPanner: () => audio.lastPanner(),
    audioEvent: (kind, a, b) => audio.handleEvent(kind, a, b),
    offlineStart: (mode = 1, teams = 2) => {
      startOffline(mode, teams);
      return true;
    },
    offlineStats: () => {
      let x = 0;
      let y = 0;
      let z = 0;
      for (let i = 0; i < interpolator.playerCount; i += 1) {
        if (interpolator.playerIds[i] === 0) {
          x = interpolator.playerX[i];
          y = interpolator.playerY[i];
          z = interpolator.playerZ[i];
          break;
        }
      }
      return {
        active: offlineActive,
        snapshot: offlineSnapshot,
        state: offline?.state ?? "idle",
        players: interpolator.playerCount,
        buddies: interpolator.buddyCount,
        decoded: snapshotDecoder.decoded,
        hash: lastAdvanced.hash,
        ended: lastAdvanced.ended,
        x,
        y,
        z,
      };
    },
    offlineAdvance: (ticks) => offline?.advance(ticks),
    offlineInput: (frame) => offline?.sendInput(Uint8Array.from(frame)),
    offlineStop: () => {
      offline?.stop();
      offline = null;
      offlineActive = false;
      offlineSnapshot = false;
    },
  };
  window.__cbReady = true;

  let lastJuiceAt = 0;
  const loop = (now: number): void => {
    const frame =
      desktop !== null
        ? desktop.poll()
        : touch !== null
          ? touch.poll()
          : new Uint8Array(4);
    if (offlineActive && offline !== null && now - lastOfflineInput >= 66) {
      offline.sendInput(frame);
      lastOfflineInput = now;
    }
    if (client !== null) {
      client.tick(16, offlineActive ? ZERO_FRAME : frame);
      tickCalls += 1;
    }
    const seconds = Math.floor(now / 1000);
    elapsedSeconds = seconds;
    recipeBook.setElapsed(seconds);
    const cycle = seconds % 20;
    hudStore.set({
      matchSeconds: offlineActive
        ? Math.floor((snapshotDecoder.latestFrame?.tick ?? 0) / 20)
        : seconds,
      // Demo countdown until the airdrop stream is wired in T-045/T-048.
      dropSeconds: cycle <= 10 ? 10 - cycle : 20 - cycle,
    });
    if (interaction !== null) {
      if (desktop !== null && desktop.pointerLock) {
        interaction.aimFromCamera(renderer.camera);
        if (desktop.consumePrimary()) {
          interaction.breakBlock();
        }
        if (desktop.consumeSecondary()) {
          interaction.place(offlineActive ? 11 : 1);
        }
      } else if (touch !== null) {
        interaction.aimFromCamera(renderer.camera);
      }
    }
    const views = client?.renderViews ?? null;
    if (views !== null) {
      const eventCount = views.desc.getUint32(DESC_EVENT_COUNT, true);
      juice.consumeEvents(views.events, eventCount, now);
      audio.consumeEvents(views.events, eventCount);
    }
    const dt = lastJuiceAt === 0 ? 16 : Math.min(100, Math.max(0, now - lastJuiceAt));
    lastJuiceAt = now;
    juice.update(now, dt);
    audio.update(dt / 1_000);
    if (offlineActive && offlineSnapshot) {
      interpolator.sample(now);
      entities?.updateFromInterp(
        interpolator,
        renderer.quality.entityAnimation && juice.timeScale > 0,
      );
      followOfflineCamera();
    } else if (views !== null) {
      entities?.update(views, renderer.quality.entityAnimation && juice.timeScale > 0);
    }
    particles?.update(juice.presentationSeconds);
    if (chunks !== null) {
      chunks.update(renderer.camera.position.x, renderer.camera.position.z, 1);
    }
    juice.applyCamera();
    renderer.frame(now);
    juice.restoreCamera();
    window.requestAnimationFrame(loop);
  };

  function followOfflineCamera(): void {
    for (let i = 0; i < interpolator.playerCount; i += 1) {
      if (interpolator.playerIds[i] !== 0) continue;
      const yaw = interpolator.playerYaw[i];
      const x = interpolator.playerX[i];
      const y = interpolator.playerY[i];
      const z = interpolator.playerZ[i];
      renderer.camera.position.set(
        x - Math.sin(yaw) * 4.2,
        y + 2.6,
        z - Math.cos(yaw) * 4.2,
      );
      renderer.camera.lookAt(x, y + 1.1, z);
      hudStore.set({ hp: interpolator.playerHp[i], maxHp: 100 });
      return;
    }
  }
  window.requestAnimationFrame(loop);
}

void boot();

function editAck(bytes: Uint8Array): { seq: number; accepted: boolean } | null {
  let at = 1;
  let seq = 0;
  let shift = 0;
  for (;;) {
    if (at >= bytes.length || shift > 35) return null;
    const byte = bytes[at];
    at += 1;
    seq += (byte & 0x7f) * 2 ** shift;
    if ((byte & 0x80) === 0) break;
    shift += 7;
  }
  if (at >= bytes.length) return null;
  return { seq: seq >>> 0, accepted: bytes[at] === 1 };
}
