/**
 * Typed views over the wasm render buffer (plan §8.4, task T-030).
 *
 * The bridge writes one `#[repr(C)]` buffer: a 52-byte header followed by the
 * entity arrays. JS never parses the buffer into objects on the frame path; it
 * holds a `DataView` per array and reads fields by offset. The offsets are the
 * ones pinned by `crates/client-wasm/src/render_buffer.rs`.
 *
 * ## The `memory.grow` trap
 *
 * Growing wasm memory detaches every existing `ArrayBuffer` view. A cached
 * `DataView` then reports `byteLength === 0`, and reading it throws or, worse,
 * silently reads nothing. The rule is:
 *
 * 1. `rebuildViews()` is called after **every** `cb_mem_alloc`.
 * 2. In dev builds, every frame checks that each view's buffer is still alive
 *    and that the header magic is intact.
 * 3. A failure names this bug, because "zero-length view" otherwise surfaces
 *    three layers away as corrupted geometry.
 */

export const RENDER_DESC_MAGIC = 0xcba7_e001;
export const RENDER_DESC_BYTES = 52;
export const GPU_PLAYER_BYTES = 32;
export const PARTICLE_SLOT_BYTES = 16;
export const EVENT_SLOT_BYTES = 16;

export const MAX_PLAYERS = 18;
export const MAX_BUDDIES = 64;
export const MAX_PROJECTILES = 128;
export const MAX_PARTICLES = 512;
export const MAX_EVENTS = 64;
export const MAX_DIRTY_CHUNKS = 256;

/** Offsets inside `RenderDesc`, in bytes. */
export const DESC_TICK = 4;
export const DESC_PLAYER_COUNT = 8;
export const DESC_BUDDY_COUNT = 12;
export const DESC_PROJ_COUNT = 16;
export const DESC_PARTICLE_COUNT = 20;
export const DESC_EVENT_COUNT = 24;
export const DESC_DIRTY_COUNT = 28;
export const DESC_PLAYER_PTR = 32;
export const DESC_BUDDY_PTR = 36;
export const DESC_PROJ_PTR = 40;
export const DESC_PARTICLE_PTR = 44;
export const DESC_EVENT_PTR = 48;

const INPUT_BYTES = 4;
const OK = 1;

/**
 * The wasm-bindgen exports. Kept structural so tests can pass a fake module;
 * the generated glue satisfies it.
 */
export interface ClientWasmExports {
  readonly memory: WebAssembly.Memory;
  cb_protocol_version(): number;
  cb_init(seed: number, tuningPtr: number, tuningLen: number, mode: number): number;
  cb_tick(dtMs: number, inputPtr: number): number;
  cb_on_snapshot(ptr: number, len: number): number;
  cb_local_edit(x: number, y: number, z: number, block: number, action: number): number;
  cb_ack_edit(seq: number, accepted: boolean): number;
  cb_dirty_chunks(): number;
  cb_mesh_chunk(cx: number, cz: number, outPtr: number, outCap: number): number;
  cb_release_mesh(generation: number): number;
  cb_render_desc(): number;
  cb_set_quality(level: number): number;
  cb_state_hash(): number;
  cb_mem_alloc(size: number): number;
}

/** Live views into one wasm memory generation. */
export interface RenderViews {
  readonly desc: DataView;
  readonly players: DataView;
  readonly buddies: DataView;
  readonly projectiles: DataView;
  readonly particles: DataView;
  readonly events: DataView;
  readonly dirty: DataView;
}

export interface ClientOptions {
  /** Enables the detach and magic checks on every frame. */
  dev?: boolean;
}

export interface PlayerView {
  id: number;
  team: number;
  flags: number;
  x: number;
  y: number;
  z: number;
  yaw: number;
  pitch: number;
  anim: number;
  hp: number;
  role: number;
  pad: number;
}

/**
 * Owns one wasm instance and the views over its render buffer.
 *
 * Construction does not initialise the bridge; call {@link init} once before
 * the first {@link tick}. The loader is deliberately a class: the views are
 * mutable state tied to a memory generation, and a free function would invite
 * two callers to hold different generations.
 */
export class ClientWasm {
  private readonly wasm: ClientWasmExports;
  private readonly dev: boolean;
  private views: RenderViews | null = null;
  private inputPtr = 0;
  private inputView: Uint8Array | null = null;
  private tickIndex = 0;
  private initialised = false;

  constructor(wasm: ClientWasmExports, options: ClientOptions = {}) {
    this.wasm = wasm;
    this.dev = options.dev === true;
  }

  /** Protocol version the bridge was built against. */
  get protocolVersion(): number {
    return this.wasm.cb_protocol_version() >>> 0;
  }

  /** The last tick the bridge reported. */
  get currentTick(): number {
    return this.tickIndex;
  }

  /**
   * Initialise the bridge.
   *
   * `tuning` is optional only because the Rust side embeds the same file; when
   * it is supplied the bytes must match that file exactly. A mismatch fails
   * `cb_init` rather than starting a subtly different game.
   */
  init(seed: number, tuning: Uint8Array | null = null, mode = 0): void {
    let tuningPtr = 0;
    let tuningLen = 0;
    if (tuning !== null && tuning.byteLength > 0) {
      tuningPtr = this.alloc(tuning.byteLength);
      new Uint8Array(this.wasm.memory.buffer, tuningPtr, tuning.byteLength).set(tuning);
      tuningLen = tuning.byteLength;
    }

    // One input frame lives in wasm memory for the whole session. Allocating it
    // once is what lets tick() avoid an alloc-and-rebuild on every frame.
    this.inputPtr = this.alloc(INPUT_BYTES);
    this.inputView = new Uint8Array(this.wasm.memory.buffer, this.inputPtr, INPUT_BYTES);

    const ok = this.wasm.cb_init(seed >>> 0, tuningPtr, tuningLen, mode) >>> 0;
    if (ok !== OK) {
      throw new Error("cb_init failed: tuning mismatch or invalid mode");
    }
    this.initialised = true;
    this.tickIndex = 0;
    this.rebuildViews();
  }

  /**
   * Recreate every view from the current `memory.buffer`.
   *
   * MUST be called after any `cb_mem_alloc`; {@link alloc} does it for every
   * internal caller so the only way to forget is to call the raw export.
   * Safe to call before `cb_init`: it clears the views instead of inventing
   * pointers.
   */
  rebuildViews(): void {
    const descPtr = this.wasm.cb_render_desc() >>> 0;
    if (descPtr === 0 || !this.initialised) {
      this.views = null;
      this.inputView = null;
      return;
    }

    const buffer = this.wasm.memory.buffer;
    const desc = new DataView(buffer, descPtr, RENDER_DESC_BYTES);
    const playerPtr = desc.getUint32(DESC_PLAYER_PTR, true);
    const buddyPtr = desc.getUint32(DESC_BUDDY_PTR, true);
    const projPtr = desc.getUint32(DESC_PROJ_PTR, true);
    const particlePtr = desc.getUint32(DESC_PARTICLE_PTR, true);
    const eventPtr = desc.getUint32(DESC_EVENT_PTR, true);
    const dirtyPtr = this.wasm.cb_dirty_chunks() >>> 0;

    const span = (ptr: number, bytes: number, name: string): DataView => {
      if (ptr === 0 || ptr + bytes > buffer.byteLength) {
        throw new Error(`${name} view is outside wasm memory (ptr ${ptr}, ${bytes} bytes)`);
      }
      return new DataView(buffer, ptr, bytes);
    };

    this.views = {
      desc,
      players: span(playerPtr, MAX_PLAYERS * GPU_PLAYER_BYTES, "player"),
      buddies: span(buddyPtr, MAX_BUDDIES * GPU_PLAYER_BYTES, "buddy"),
      projectiles: span(projPtr, MAX_PROJECTILES * GPU_PLAYER_BYTES, "projectile"),
      particles: span(particlePtr, MAX_PARTICLES * PARTICLE_SLOT_BYTES, "particle"),
      events: span(eventPtr, MAX_EVENTS * EVENT_SLOT_BYTES, "event"),
      dirty: span(dirtyPtr, MAX_DIRTY_CHUNKS * 16, "dirty"),
    };
    this.inputView =
      this.inputPtr === 0 ? null : new Uint8Array(buffer, this.inputPtr, INPUT_BYTES);
  }

  /**
   * Advance the bridge by one browser frame.
   *
   * `input` is the 4-byte wire frame: buttons, yaw, pitch, and two signed
   * 4-bit axes. The bridge accumulates `dtMs` and steps at 20 Hz, so a 60 fps
   * browser does not run the simulation three times too fast.
   */
  tick(dtMs: number, input: Uint8Array): RenderViews {
    if (!this.initialised) {
      throw new Error("ClientWasm.tick before init");
    }
    if (input.byteLength !== INPUT_BYTES) {
      throw new Error(`input must be ${INPUT_BYTES} bytes, got ${input.byteLength}`);
    }
    this.assertViews();
    if (this.inputView === null) {
      throw new Error("input buffer is not attached; call rebuildViews()");
    }
    this.inputView.set(input);

    const ptr = this.wasm.cb_tick(dtMs >>> 0, this.inputPtr) >>> 0;
    if (ptr === 0) {
      throw new Error("cb_tick failed: bridge is not initialised");
    }
    if (this.views === null) {
      this.rebuildViews();
    }
    this.assertViews();
    this.tickIndex = this.views!.desc.getUint32(DESC_TICK, true);
    return this.views!;
  }

  /** Player count in the last frame. */
  get playerCount(): number {
    return this.views?.desc.getUint32(DESC_PLAYER_COUNT, true) ?? 0;
  }

  /** Buddy count in the last frame. */
  get buddyCount(): number {
    return this.views?.desc.getUint32(DESC_BUDDY_COUNT, true) ?? 0;
  }

  /** Read one 32-byte player record by index. */
  readPlayer(index: number): PlayerView {
    if (this.views === null) {
      throw new Error("render views are not initialised");
    }
    const at = index * GPU_PLAYER_BYTES;
    const view = this.views.players;
    if (at + GPU_PLAYER_BYTES > view.byteLength) {
      throw new RangeError(`player ${index} is outside the render buffer`);
    }
    return {
      id: view.getUint16(at, true),
      team: view.getUint8(at + 2),
      flags: view.getUint8(at + 3),
      x: view.getFloat32(at + 4, true),
      y: view.getFloat32(at + 8, true),
      z: view.getFloat32(at + 12, true),
      yaw: view.getFloat32(at + 16, true),
      pitch: view.getFloat32(at + 20, true),
      anim: view.getFloat32(at + 24, true),
      hp: view.getUint8(at + 28),
      role: view.getUint8(at + 29),
      pad: view.getUint16(at + 30, true),
    };
  }

  /**
   * Run one mesh handoff (plan §8.4).
   *
   * The copy is the ownership boundary: three.js gets a `Uint8Array` that does
   * not alias wasm memory, so a later `memory.grow` cannot detach it.
   */
  meshChunk(cx: number, cz: number, capacity: number): Uint8Array | null {
    const ptr = this.alloc(capacity);
    const written = this.wasm.cb_mesh_chunk(cx, cz, ptr, capacity) >>> 0;
    if (written === 0) {
      this.wasm.cb_release_mesh(0);
      return null;
    }
    const copy = new Uint8Array(this.wasm.memory.buffer, ptr, written).slice();
    this.wasm.cb_release_mesh(0);
    return copy;
  }

  /** Queue one local edit; returns its sequence number. */
  localEdit(x: number, y: number, z: number, block: number, action: number): number {
    return this.wasm.cb_local_edit(x, y, z, block, action) >>> 0;
  }

  /** Record the server's answer to an edit. */
  ackEdit(seq: number, accepted: boolean): boolean {
    return (this.wasm.cb_ack_edit(seq, accepted) >>> 0) === OK;
  }

  /** Accept a snapshot blob. Decoding is T-043's job. */
  onSnapshot(ptr: number, len: number): boolean {
    return (this.wasm.cb_on_snapshot(ptr >>> 0, len >>> 0) >>> 0) === OK;
  }

  /** Select a quality tier (0..2). */
  setQuality(level: number): boolean {
    return (this.wasm.cb_set_quality(level) >>> 0) === OK;
  }

  /** FNV hash of the local state, for desync reports. */
  stateHash(): number {
    return this.wasm.cb_state_hash() >>> 0;
  }

  /** Reserve wasm staging space. Rebuilds views because memory may have grown. */
  alloc(size: number): number {
    const ptr = this.wasm.cb_mem_alloc(size >>> 0) >>> 0;
    this.rebuildViews();
    if (ptr === 0) {
      throw new Error(`cb_mem_alloc(${size}) failed`);
    }
    return ptr;
  }

  /**
   * Dev guard: every cached view must still point at live memory.
   *
   * A `memory.grow` detaches all old views, which shows up as a zero-length
   * buffer. The error names the cause so the fix is obvious.
   */
  private assertViews(): void {
    if (!this.dev) {
      return;
    }
    if (this.views === null) {
      throw new Error("render views are not initialised; call rebuildViews()");
    }
    for (const [name, view] of Object.entries(this.views)) {
      if (view.buffer.byteLength === 0) {
        throw new Error(
          `wasm memory.grow detached the ${name} view; call rebuildViews() after cb_mem_alloc`,
        );
      }
    }
    if (this.views.desc.getUint32(0, true) !== RENDER_DESC_MAGIC) {
      throw new Error("stale render buffer: header magic is missing");
    }
  }
}
