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
import { loadCharacterModels, type CharacterModels } from "./render/characters";
import { Juice, EVENT_HIT, type JuiceStats } from "./render/juice";
import { AudioEngine, type AudioStats } from "./audio/engine";
import { CrashReporter, type CrashReport } from "./telemetry";
import { EconomyTracker, reportEconomy, type EconomySummary } from "./telemetry/economy";
import { Interaction, type PendingEdit, type RayHit, type VisualState } from "./game/interaction";
import {
  DesktopInput,
  FIRE,
  SPRINT,
  loadKeyMap,
  type InputAction,
} from "./game/inputDesktop";
import { TouchInput, type TouchState } from "./game/inputTouch";
import { errorMessage, showFatalScreen, showRttOverlay, type NetState } from "./game/net/client";
import { ChunkSync } from "./game/net/chunkSync";
import { SnapshotDecoder } from "./game/net/snapshot";
import { Interpolator } from "./game/interp";
import { OfflineSession } from "./game/offline";
import { OnlineSession } from "./game/online";
import { EYE_HEIGHT, eyePose, pitchRadians, yawRadians } from "./game/view";
import { Hud, type HudStats } from "./ui/hud";
import { Hotbar } from "./ui/hotbar";
import { ViewModel, type ViewModelInfo } from "./render/viewModel";
import { RecipeBookUi, type RecipeBookInfo } from "./ui/recipeBook";
import { LobbyScreen } from "./ui/lobby";
import {
  ScreenManager,
  buildBoot,
  buildMainMenu,
  buildPause,
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
  viewModel(): ViewModelInfo;
  pixel(x: number, y: number): [number, number, number, number];
  resetScale(): void;
  chunks(): ChunkInfo | null;
  stream(x: number, z: number, budget?: number): ChunkInfo | null;
  setFrameTime(ms: number | null): void;
  setTier(tier: QualityTier): void;
  growWasm(pages: number): GrowResult;
  leakCycle(cycles: number): void;
  wasm(): { viewsAlive: boolean; memoryBytes: number };
  camera(): { x: number; y: number; z: number; yaw: number; pitch: number; fov: number };
  cameraPose(x: number, y: number, z: number, pitch: number): void;
  characters(): { loaded: boolean; players: number; buddies: number };
  entities(): EntityInfo | null;
  entityBenchmark(frames?: number): (EntityInfo & { averageMs: number }) | null;
  particles(): ParticleInfo | null;
  particleBenchmark(): { spawned: ParticleInfo; after: ParticleInfo } | null;
  surface(x: number, z: number): number;
  aim(origin: [number, number, number], direction: [number, number, number]): {
    hit: RayHit | null;
    visuals: VisualState | null;
  };
  interactionVisuals(): VisualState | null;
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
    rawInput: boolean;
    lastAction: InputAction | null;
  };
  setSensitivity(value: number): void;
  setInvertY(value: boolean): void;
  touch(): TouchState | null;
  hudUpdate(patch: Partial<HudState>): void;
  hudState(): HudState;
  hudStats(): HudStats;
  hudFeed(): string[];
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
  wasmTickBenchmark(frames?: number): number;
  serverMessage(frame: number[]): boolean;
  audioStats(): AudioStats;
  audioUnlock(): Promise<boolean>;
  audioCue(cue: string, x?: number, y?: number, z?: number): number;
  audioRumble(level: number): AudioStats;
  audioPanner(): { x: number; y: number; z: number } | null;
  audioEvent(kind: number, a: number, b: number): void;
  telemetryReport(message: string): CrashReport;
  telemetryLast(): CrashReport | null;
  economyLast(): EconomySummary | null;
  offlineStart(mode?: number, teams?: number): boolean;  offlineStats(): {
    active: boolean;
    snapshot: boolean;
    state: string;
    players: number;
    buddies: number;
    decoded: number;
    tick: number;
    hash: number;
    ended: boolean;
    x: number;
    y: number;
    z: number;
    positions: Array<{ id: number; x: number; y: number; z: number }>;
  };
  offlineAdvance(ticks: number, post?: boolean): void;
  offlineInput(frame: number[]): void;
  offlineStop(): void;
  offlineGrant(player: number, crates: number): void;
  offlineDamageCore(team: number, amount: number): void;
  offlinePlaceRecipe(team: number, recipe: number): void;
  offlinePlaceBlocks(team: number, blocks: Array<[number, number, number]>): void;
  onlineStart(roomCode: string, server?: string): boolean;
  sessionEvents(): Array<{ kind: number; a: number; b: number; c: number; tick: number }>;
  sessionTeam(): number;
  onlineStats(): {
    active: boolean;
    state: NetState;
    playerId: number;
    snapshot: boolean;
    players: Array<{ id: number; x: number; y: number; z: number }>;
    decoded: number;
    tick: number;
    rtt: number | null;
    frames: { snapshots: number; chunkDiffs: number };
    ended: boolean;
    x: number;
    y: number;
    z: number;
  };
  onlineInput(frame: number[]): void;
  onlineHold(frame: number[] | null): void;
  onlineDrop(): void;
  onlineStop(): void;
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
let loadedCharacters: { players: number; buddies: number } | null = null;
let particles: ParticleSystem | null = null;
let offline: OfflineSession | null = null;
let offlineActive = false;
let offlineSnapshot = false;
let lastOfflineInput = 0;
let lastCameraAt = 0;
let cameraSettled = false;
let online: OnlineSession | null = null;
let onlineActive = false;
let onlineSnapshot = false;
let onlinePlayerId = 0;
let lastOnlineInput = 0;
let onlineEnded = false;
let onlineOverride: Uint8Array | null = null;
let sessionTeam = 0;
let localCarried = 0;
let sessionEvents: Array<{ kind: number; a: number; b: number; c: number; tick: number }> = [];
let lastAdvanced = { ended: false, hash: 0 };
const ZERO_FRAME = new Uint8Array(4);
const telemetryEndpoint = (import.meta.env.VITE_TELEMETRY_URL as string | undefined) ?? null;
let economy = new EconomyTracker(6);
let lastEconomy: EconomySummary | null = null;
const crashReporter = new CrashReporter(telemetryEndpoint, "dev");
crashReporter.install();
let interaction: Interaction | null = null;
let desktop: DesktopInput | null = null;
let touch: TouchInput | null = null;
let screens: ScreenManager | null = null;
let settingsReturn: "main" | "pause" = "main";
let tickCalls = 0;
let elapsedSeconds = 0;
const stanceOrder: HudStance[] = ["follow", "hold", "assault"];

async function boot(): Promise<void> {
  const characterModelsPromise: Promise<CharacterModels | null> = clearScene
    ? Promise.resolve(null)
    : loadCharacterModels().catch((error: unknown) => {
        console.warn(
          `character models unavailable: ${error instanceof Error ? error.message : String(error)}`,
        );
        return null;
      });

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
  renderer.camera.rotation.order = "YXZ";

  if (chunksEnabled) {
    chunks = new ChunkManager(renderer.scene, clientMeshSource(wasmClient), {
      loadRadius: 6,
      keepRadius: 7,
    });
  }
  const chunkSync = new ChunkSync();
  const snapshotDecoder = new SnapshotDecoder();
  const interpolator = new Interpolator(snapshotDecoder.playerTeam, snapshotDecoder.buddyRole);
  if (!clearScene) {
    const characterModels = await characterModelsPromise;
    if (characterModels !== null) {
      loadedCharacters = {
        players: characterModels.players.length,
        buddies: characterModels.buddies.length,
      };
    }
    entities = new EntityRenderer(renderer.scene, characterModels);
  }
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
          : onlineActive && online !== null
            ? online.sendEdit(x, y, z, block, action)
            : wasmClient.localEdit(x, y, z, block, action),
    });
  }

  const recipes = wasmClient.recipes();
  const recipeBook = new RecipeBookUi(document.body, recipes);
  const ghost = new GhostOverlay(renderer.scene);
  const hudStore = new Store<HudState>(initialHudState());
  const hud = new Hud(document.body, hudStore);
  const viewModel = new ViewModel();
  const burstOrigin = new THREE.Vector3();
  const burstVelocity = new THREE.Vector3();
  let stanceIndex = 0;

  function blockBurst(
    cell: [number, number, number] | null,
    color: number,
  ): void {
    if (particles === null || cell === null) return;
    burstOrigin.set(cell[0] + 0.5, cell[1] + 0.5, cell[2] + 0.5);
    for (let i = 0; i < 10; i += 1) {
      const angle = (i / 10) * Math.PI * 2;
      burstVelocity.set(Math.cos(angle) * 1.3, 1.2 + (i % 3) * 0.35, Math.sin(angle) * 1.3);
      particles.spawn(burstOrigin, burstVelocity, 0.32, 0.07, color);
    }
  }

  function localBreak(): void {
    if (interaction === null) return;
    const cell = interaction.visuals().highlight;
    if (interaction.breakBlock() > 0) {
      viewModel.swing();
      blockBurst(cell, 0x8a6b4d);
      audio.play("sword_crate");
    }
  }

  function localPlace(block: number): void {
    if (interaction === null) return;
    const cell = interaction.visuals().ghost;
    if (interaction.place(block) > 0) {
      viewModel.swing();
      blockBurst(cell, 0x9ad1ff);
      audio.play("block_place");
    }
  }
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
        localBreak();
      };
      touch.onPlace = (block) => {
        localPlace(offlineActive || onlineActive ? 11 : block);
      };
    }
  } else {
    desktop = new DesktopInput({ canvas, keyMap: loadKeyMap(window.localStorage) });
    const bar = new Hotbar(
      document.body,
      (slot) => {
        desktop?.setSlot(slot);
        viewModel.setSelected(slot);
      },
      desktop.selectedSlot,
    );
    desktop.onAction((action, value) => {
      if (action === "stance") {
        advanceStance();
      } else if (action === "slot") {
        bar.setSelected(value ?? 0);
        viewModel.setSelected(value ?? 0);
      } else if (action === "recipes") {
        if (screens?.current() === "match") {
          recipeBook.toggle(elapsedSeconds);
        }
      } else if (action === "menu") {
        recipeBook.close();
        const current = screens?.current();
        if (current === "match") {
          openPause();
        } else if (current === "pause") {
          resumeMatch();
        } else if (current === "settings" && settingsReturn === "pause") {
          screens?.show("pause");
        } else {
          screens?.show("main");
        }
      }
    });
  }

  screens = new ScreenManager(document.body, () => openPause());
  screens.register("boot", buildBoot(() => screens?.show("main")));
  screens.register(
    "main",
    buildMainMenu({
      onOnline: () => screens?.show("lobby"),
      onOffline: () => startOffline(),
      onSettings: () => openSettings("main"),
    }),
  );
  screens.register(
    "pause",
    buildPause({
      onResume: () => resumeMatch(),
      onSettings: () => openSettings("pause"),
      onLeave: () => leaveMatch(),
    }),
  );
  const lobby = new LobbyScreen({
    onBack: () => screens?.show("main"),
    onStart: (code) => startOnline(code),
  });
  screens.register("lobby", lobby.element);
  const settings = new SettingsScreen({
    onQuality: (tier) => renderer.setTier(tier),
    onSensitivity: (value) => desktop?.setSensitivity(value),
    onInvertY: (value) => desktop?.setInvertY(value),
    onRawInput: (value) => desktop?.setRawInput(value),
    onKeyMap: (map) => desktop?.setKeyMap(map),
    onBack: () => screens?.show(settingsReturn),
  });
  screens.register("settings", settings.element);
  screens.register(
    "results",
    buildResults({
      onMain: () => {
        stopSessions();
        screens?.show("main");
      },
      onRematch: () => startOffline(),
    }),
  );
  const initialScreen: ScreenId = query.get("screen") === "match" ? "match" : "boot";
  screens.show(initialScreen);

  function stopSessions(): void {
    offline?.stop();
    offline = null;
    offlineActive = false;
    offlineSnapshot = false;
    online?.stop();
    online = null;
    onlineActive = false;
    onlineSnapshot = false;
    onlineEnded = false;
    onlineOverride = null;
    entities?.hideLocalPlayer(-1);
    cameraSettled = false;
    lastCameraAt = 0;
  }

  function openSettings(returnTo: "main" | "pause"): void {
    settingsReturn = returnTo;
    screens?.show("settings");
  }

  function openPause(): void {
    if (screens?.current() !== "match") return;
    recipeBook.close();
    offline?.pause();
    screens.show("pause");
  }

  function resumeMatch(): void {
    if (screens?.current() !== "pause") return;
    offline?.resume();
    screens.show("match");
  }

  function leaveMatch(): void {
    stopSessions();
    screens?.show("results");
  }

  function showResults(bytes: Uint8Array): void {
    if (bytes[0] !== 0x89 || bytes.length < 3) return;
    const winner = bytes[1] > 127 ? bytes[1] - 256 : bytes[1];
    const reason = bytes[2];
    const title = document.querySelector<HTMLElement>('[data-results="title"]');
    const detail = document.querySelector<HTMLElement>('[data-results="detail"]');
    if (title === null || detail === null) return;
    if (reason === 1) {
      title.textContent = "NO HUMAN SURVIVOR";
      detail.textContent = "The last human fell. Bots do not win.";
    } else if (reason === 3) {
      title.textContent = "DRAW";
      detail.textContent = "Every core fell on the same tick.";
    } else if (winner === sessionTeam) {
      title.textContent = "VICTORY";
      detail.textContent = `Team ${winner + 1} wins.`;
    } else {
      title.textContent = "DEFEAT";
      detail.textContent = `Team ${winner + 1} wins.`;
    }
  }

  function processSnapshotEvents(frame: {
    eventCount: number;
    eventKind: Uint8Array;
    eventA: Int32Array;
    eventB: Int32Array;
    eventC: Int32Array;
    tick: number;
  }): void {
    const localId = offlineActive ? 0 : onlinePlayerId;
    for (let i = 0; i < frame.eventCount; i += 1) {
      const kind = frame.eventKind[i];
      const a = frame.eventA[i];
      const b = frame.eventB[i];
      const c = frame.eventC[i];
      sessionEvents.push({ kind, a, b, c, tick: frame.tick });
      if (sessionEvents.length > 128) sessionEvents.shift();
      economy.observe(kind, a, b, c, frame.tick);
      if (kind === 6 && a === localId) {
        localCarried += b;
        hudStore.set({ crates: localCarried });
        hud.toast(`+${b} crates`);
      } else if (kind === 0) {
        const recipe = recipes.find((row) => row.id === b);
        hud.toast(`${recipe?.name ?? `Recipe ${b}`} crafted`);
        // output_kind 8 is a buddy. Play the spawn cue only for the local team;
        // the global craft toast still reports every team.
        if (recipe?.output === 8 && a === sessionTeam) {
          audio.play("buddy_spawn");
        }
      } else if (kind === 10) {
        const recipe = recipes.find((row) => row.id === b);
        hud.toast(`${recipe?.name ?? `Recipe ${b}`}: buddy cap reached`);
      } else if (kind === 8) {
        hud.toast("Airdrop incoming");
      } else if (kind === 4) {
        hud.toast(`Airdrop landed: ${c} crates`);
      } else if (kind === 3 && a === sessionTeam) {
        hud.toast("Your core is down");
      }
    }
  }

  function finalizeEconomy(): void {
    lastEconomy = economy.summary(snapshotDecoder.latestFrame?.tick ?? 0);
    reportEconomy(telemetryEndpoint, lastEconomy);
  }

  function startOffline(mode = 1, teams = 2): void {
    online?.stop();
    online = null;
    onlineActive = false;
    onlineSnapshot = false;
    offline?.stop();
    offline = null;
    snapshotDecoder.reset();
    interpolator.reset();
    chunkSync.reset();
    offlineActive = true;
    offlineSnapshot = false;
    entities?.hideLocalPlayer(0);
    lastOfflineInput = 0;
    cameraSettled = false;
    lastCameraAt = 0;
    sessionTeam = 0;
    localCarried = 0;
    sessionEvents = [];
    economy = new EconomyTracker(6);
    lastEconomy = null;
    lastAdvanced = { ended: false, hash: 0 };
    offline = new OfflineSession({
      seed: DEMO_SEED,
      mode,
      teams,
      onWelcome: () => {
        sessionTeam = 0;
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
            processSnapshotEvents(frame);
            interpolator.push(frame, performance.now());
            offlineSnapshot = true;
          }
        } else if (bytes[0] === 0x85) {
          const ack = editAck(bytes);
          if (ack !== null) interaction?.ack(ack.seq, ack.accepted);
        }
      },
      onEnd: (bytes) => {
        finalizeEconomy();
        showResults(bytes);
        offlineActive = false;
        offlineSnapshot = false;
        entities?.hideLocalPlayer(-1);
        offline?.stop();
        offline = null;
        screens?.show("results");
      },
      onAdvanced: (ended, hash) => {
        lastAdvanced = { ended, hash };
        if (ended) {
          offlineActive = false;
          offlineSnapshot = false;
          entities?.hideLocalPlayer(-1);
          offline?.stop();
          offline = null;
          screens?.show("results");
        }
      },
    });
    screens?.show("match");
  }

  function startOnline(roomCode: string, serverOverride?: string): void {
    stopSessions();
    snapshotDecoder.reset();
    interpolator.reset();
    const server =
      serverOverride ??
      query.get("server") ??
      (import.meta.env.VITE_WS_URL as string | undefined) ??
      `ws://${window.location.hostname}:8080`;
    onlineActive = true;
    onlineSnapshot = false;
    onlinePlayerId = 0;
    onlineEnded = false;
    onlineOverride = null;
    lastOnlineInput = 0;
    cameraSettled = false;
    lastCameraAt = 0;
    sessionTeam = 0;
    localCarried = 0;
    sessionEvents = [];
    online = new OnlineSession({
      server,
      roomCode,
      name: query.get("name") ?? "PLAYER",
      onWelcome: (message) => {
        onlinePlayerId = message.playerId;
        entities?.hideLocalPlayer(message.playerId);
        sessionTeam = message.team;
        hudStore.set({ hp: 100, maxHp: 100, crates: 0, carryCap: 12 });
      },
      onFrame: (bytes) => {
        if (client === null) return;
        if (bytes[0] === 0x83) {
          snapshotDecoder.reset();
          interpolator.reset();
          onlineSnapshot = false;
        }
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
            processSnapshotEvents(frame);
            interpolator.push(frame, performance.now());
            onlineSnapshot = true;
          }
        } else if (bytes[0] === 0x85) {
          const ack = editAck(bytes);
          if (ack !== null) interaction?.ack(ack.seq, ack.accepted);
        }
      },
      onEnd: (bytes) => {
        finalizeEconomy();
        showResults(bytes);
        onlineEnded = true;
        onlineActive = false;
        onlineSnapshot = false;
        entities?.hideLocalPlayer(-1);
        online?.stop();
        online = null;
        screens?.show("results");
      },
      onFatal: () => {
        onlineActive = false;
        onlineSnapshot = false;
        entities?.hideLocalPlayer(-1);
      },
    });
    screens?.show("match");
  }

  window.__cbTest = {
    info: () => renderer.info(),
    viewModel: () => viewModel.info(),
    pixel: (x, y) => renderer.readPixel(x, y),
    resetScale: () => renderer.resetScale(),
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
    camera: () => ({
      x: renderer.camera.position.x,
      y: renderer.camera.position.y,
      z: renderer.camera.position.z,
      yaw: renderer.camera.rotation.y,
      pitch: renderer.camera.rotation.x,
      fov: renderer.camera.fov,
    }),
    cameraPose: (x, y, z, pitch) => {
      renderer.camera.position.set(x, y, z);
      renderer.camera.rotation.x = pitch;
    },
    characters: () => ({
      loaded: loadedCharacters !== null,
      players: loadedCharacters?.players ?? 0,
      buddies: loadedCharacters?.buddies ?? 0,
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
    interactionVisuals: () => interaction?.visuals() ?? null,
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
          rawInput: desktop.rawInputValue,
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
        rawInput: false,
        lastAction: null,
      };
    },
    setSensitivity: (value) => desktop?.setSensitivity(value),
    setInvertY: (value) => desktop?.setInvertY(value),
    touch: () => touch?.state() ?? null,
    hudUpdate: (patch) => hudStore.set(patch),
    hudState: () => ({ ...hudStore.get() }),
    hudStats: () => hud.stats(),
    hudFeed: () => Array.from(hud.feedText()),
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
    wasmTickBenchmark: (frames = 600) => {
      if (client === null) {
        throw new Error("wasm is not initialised");
      }
      const times: number[] = [];
      for (let i = 0; i < frames; i += 1) {
        const begin = performance.now();
        client.tick(16, ZERO_FRAME);
        times.push(performance.now() - begin);
      }
      times.sort((a, b) => a - b);
      return times[Math.floor(times.length * 0.99)];
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
    telemetryReport: (message) => crashReporter.capture("wasm", new Error(message)),
    telemetryLast: () => crashReporter.lastReport,
    economyLast: () => lastEconomy,
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
        tick: snapshotDecoder.latestFrame?.tick ?? 0,
        hash: lastAdvanced.hash,
        ended: lastAdvanced.ended,
        x,
        y,
        z,
        positions: Array.from({ length: interpolator.playerCount }, (_, i) => ({
          id: interpolator.playerIds[i],
          x: interpolator.playerX[i],
          y: interpolator.playerY[i],
          z: interpolator.playerZ[i],
        })),
      };
    },
    offlineAdvance: (ticks, post = false) => offline?.advance(ticks, post),
    offlineInput: (frame) => offline?.sendInput(Uint8Array.from(frame)),
    offlineGrant: (player, crates) => offline?.debugGrant(player, crates),
    offlineDamageCore: (team, amount) => offline?.debugDamageCore(team, amount),
    offlinePlaceRecipe: (team, recipe) => offline?.debugPlaceRecipe(team, recipe),
    offlinePlaceBlocks: (team, blocks) => offline?.debugPlaceBlocks(team, blocks),
    offlineStop: () => {
      offline?.stop();
      offline = null;
      offlineActive = false;
      offlineSnapshot = false;
    },
    onlineStart: (roomCode, server) => {
      startOnline(roomCode, server);
      return true;
    },
    sessionEvents: () => sessionEvents.map((event) => ({ ...event })),
    sessionTeam: () => sessionTeam,
    onlineStats: () => {
      const players: Array<{ id: number; x: number; y: number; z: number }> = [];
      let x = 0;
      let y = 0;
      let z = 0;
      for (let i = 0; i < interpolator.playerCount; i += 1) {
        const id = interpolator.playerIds[i];
        const px = interpolator.playerX[i];
        const py = interpolator.playerY[i];
        const pz = interpolator.playerZ[i];
        players.push({ id, x: px, y: py, z: pz });
        if (id === onlinePlayerId) {
          x = px;
          y = py;
          z = pz;
        }
      }
      return {
        active: onlineActive,
        state: online?.state ?? "idle",
        playerId: onlinePlayerId,
        snapshot: onlineSnapshot,
        players,
        decoded: snapshotDecoder.decoded,
        tick: snapshotDecoder.latestFrame?.tick ?? 0,
        rtt: online?.rttMs ?? null,
        frames: online?.frames ?? { snapshots: 0, chunkDiffs: 0 },
        ended: onlineEnded,
        x,
        y,
        z,
      };
    },
    onlineInput: (frame) => online?.sendInput(Uint8Array.from(frame)),
    onlineHold: (frame) => {
      onlineOverride = frame === null ? null : Uint8Array.from(frame);
    },
    onlineDrop: () => online?.client.simulateDrop(),
    onlineStop: () => {
      online?.stop();
      online = null;
      onlineActive = false;
      onlineSnapshot = false;
    },
  };
  window.__cbReady = true;

  let lastJuiceAt = 0;
  let mineBroken = false;
  const loop = (now: number): void => {
    const frame =
      desktop !== null
        ? desktop.poll()
        : touch !== null
          ? touch.poll()
          : new Uint8Array(4);
    // Offline has no socket budget, so sample faster than the 50 ms tick and
    // let each tick see fresh input. Online stays at 20/s, under the 30/s
    // server message budget.
    if (offlineActive && offline !== null && now - lastOfflineInput >= 25) {
      offline.sendInput(frame);
      lastOfflineInput = now;
    }
    if (onlineActive && online !== null && now - lastOnlineInput >= 50) {
      online.sendInput(onlineOverride ?? frame);
      lastOnlineInput = now;
    }
    if (client !== null) {
      client.tick(16, offlineActive || onlineActive ? ZERO_FRAME : frame);
      tickCalls += 1;
    }
    const seconds = Math.floor(now / 1000);
    elapsedSeconds = seconds;
    recipeBook.setElapsed(seconds);
    const cycle = seconds % 20;
    hudStore.set({
      matchSeconds:
        offlineActive || onlineActive
          ? Math.floor((snapshotDecoder.latestFrame?.tick ?? 0) / 20)
          : seconds,
      // Demo countdown until the airdrop stream is wired in T-045/T-048.
      dropSeconds: cycle <= 10 ? 10 - cycle : 20 - cycle,
    });
    if (interaction !== null) {
      if (desktop !== null && desktop.pointerLock) {
        interaction.aimFromCamera(renderer.camera);
        const held = (frame[0] & FIRE) !== 0;
        if (!held) mineBroken = false;
        const progress = interaction.mine(held, now);
        if (held && !mineBroken && progress >= 1) {
          localBreak();
          mineBroken = true;
          interaction.resetMining();
        }
        if (desktop.consumeSecondary()) {
          localPlace(offlineActive || onlineActive ? 11 : 1);
        }
      } else {
        if (touch !== null) interaction.aimFromCamera(renderer.camera);
        interaction.resetMining();
        mineBroken = false;
      }
    }
    const views = client?.renderViews ?? null;
    if (views !== null) {
      const eventCount = views.desc.getUint32(DESC_EVENT_COUNT, true);
      juice.consumeEvents(views.events, eventCount, now);
      audio.consumeEvents(views.events, eventCount, {
        x: renderer.camera.position.x,
        y: renderer.camera.position.y,
        z: renderer.camera.position.z,
      });
    }
    const dt = lastJuiceAt === 0 ? 16 : Math.min(100, Math.max(0, now - lastJuiceAt));
    lastJuiceAt = now;
    const sprinting =
      screens?.current() === "match" &&
      (frame[0] & SPRINT) !== 0 &&
      (frame[3] >> 4) > 0 &&
      !document.body.classList.contains("cb-reduced-motion");
    const targetFov = sprinting ? 76 : 70;
    const nextFov =
      renderer.camera.fov + (targetFov - renderer.camera.fov) * (1 - Math.exp(-dt / 90));
    if (Math.abs(nextFov - renderer.camera.fov) > 0.01) {
      renderer.camera.fov = nextFov;
      renderer.camera.updateProjectionMatrix();
    }
    juice.update(now, dt);
    audio.update(dt / 1_000);
    entities?.setViewOrigin(
      renderer.camera.position.x,
      renderer.camera.position.z,
    );
    if ((offlineActive || onlineActive) && (offlineSnapshot || onlineSnapshot)) {
      interpolator.sample(now);
      entities?.updateFromInterp(
        interpolator,
        renderer.quality.entityAnimation && juice.timeScale > 0,
      );
      followSessionCamera(offlineActive ? 0 : onlinePlayerId, now);
    } else if (views !== null) {
      entities?.update(views, renderer.quality.entityAnimation && juice.timeScale > 0);
    }
    particles?.update(juice.presentationSeconds);
    if (chunks !== null) {
      chunks.update(renderer.camera.position.x, renderer.camera.position.z, 1);
    }
    viewModel.setActive(
      (offlineActive || onlineActive) &&
        desktop !== null &&
        screens?.current() === "match",
    );
    viewModel.setSelected(desktop?.selectedSlot ?? 0);
    viewModel.update(now, frame[3] !== 0, renderer.camera.aspect);
    juice.applyCamera();
    renderer.frame(now);
    viewModel.draw(renderer.renderer);
    juice.restoreCamera();
    window.requestAnimationFrame(loop);
  };

  function followSessionCamera(playerId: number, now: number): void {
    // The local player must not wait for the two-tick entity interpolation.
    // Follow the newest snapshot and damp the visual position across the
    // 50 ms tick; remote bodies keep the 100 ms interpolation in `entities`.
    const latest = snapshotDecoder.latestFrame;
    let latestIndex = -1;
    if (latest !== null) {
      for (let i = 0; i < latest.playerCount; i += 1) {
        if (latest.playerIds[i] === playerId) {
          latestIndex = i;
          break;
        }
      }
    }
    if (latest !== null && latestIndex >= 0) {
      const targetX = latest.playerX[latestIndex] / 65_536;
      const targetY = latest.playerY[latestIndex] / 65_536 + EYE_HEIGHT;
      const targetZ = latest.playerZ[latestIndex] / 65_536;
      const yaw = desktop?.yawValue ?? touch?.yawValue ?? latest.playerYaw[latestIndex];
      const pitch = desktop?.pitchValue ?? touch?.pitchValue ?? 0;
      if (
        !cameraSettled ||
        Math.hypot(
          renderer.camera.position.x - targetX,
          renderer.camera.position.z - targetZ,
        ) > 8
      ) {
        renderer.camera.position.set(targetX, targetY, targetZ);
        cameraSettled = true;
      } else {
        const dt = lastCameraAt > 0 ? Math.min(50, Math.max(1, now - lastCameraAt)) : 16;
        const alpha = 1 - Math.exp(-dt / 20);
        renderer.camera.position.x += (targetX - renderer.camera.position.x) * alpha;
        renderer.camera.position.y += (targetY - renderer.camera.position.y) * alpha;
        renderer.camera.position.z += (targetZ - renderer.camera.position.z) * alpha;
      }
      lastCameraAt = now;
      renderer.camera.rotation.set(pitchRadians(pitch), yawRadians(yaw) + Math.PI, 0);
      hudStore.set({ hp: latest.playerHp[latestIndex], maxHp: 100 });
      return;
    }

    for (let i = 0; i < interpolator.playerCount; i += 1) {
      if (interpolator.playerIds[i] !== playerId) continue;
      const snapshotYaw = (interpolator.playerYaw[i] * 256) / (Math.PI * 2);
      const yaw = desktop?.yawValue ?? touch?.yawValue ?? snapshotYaw;
      const pitch = desktop?.pitchValue ?? touch?.pitchValue ?? 0;
      const pose = eyePose(
        interpolator.playerX[i],
        interpolator.playerY[i],
        interpolator.playerZ[i],
        yaw,
        pitch,
      );
      renderer.camera.position.set(pose.x, pose.y, pose.z);
      renderer.camera.rotation.set(pose.rotationX, pose.rotationY, 0);
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
