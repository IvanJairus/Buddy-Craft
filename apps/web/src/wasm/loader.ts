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

type RawFunction = (...args: number[]) => number;

/**
 * Instantiate the bridge from the raw `.wasm`.
 *
 * The bridge's exports are all numbers, so the wasm-bindgen JS glue is not
 * needed. Its only imports are four internal helpers (`describe`, a throw
 * shim, and two externref-table hooks); none is on a numeric path, and the
 * loader stubs them. This keeps the browser build free of a code-generation
 * step and makes the wasm artifact the single source of truth.
 *
 * Export names carry wasm-bindgen's hash suffix (`cb_tick_ab12...`), so each
 * function is found by base name. `cb_ack_edit` is the one wrapper: its raw
 * ABI takes an i32, while the typed interface takes a boolean.
 */
export async function loadClientWasm(url: string): Promise<ClientWasmExports> {
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`failed to fetch wasm from ${url}: HTTP ${response.status}`);
  }
  const bytes = await response.arrayBuffer();
  const module = await WebAssembly.compile(bytes);

  let memory: WebAssembly.Memory | null = null;
  const imports: WebAssembly.Imports = {};
  for (const entry of WebAssembly.Module.imports(module)) {
    if (entry.kind !== "function") {
      throw new Error(`unsupported wasm import ${entry.kind}: ${entry.module}.${entry.name}`);
    }
    const namespace = (imports[entry.module] ??= {}) as Record<
      string,
      (...args: number[]) => number
    >;
    namespace[entry.name] = (...args: number[]): number => {
      if (memory !== null && entry.name.includes("throw")) {
        const ptr = args[0] >>> 0;
        const len = args[1] >>> 0;
        const text = new TextDecoder().decode(new Uint8Array(memory.buffer, ptr, len));
        throw new Error(`wasm trap: ${text}`);
      }
      // `describe` and the externref-table hooks are only used by generated
      // glue or by closures; this bridge has neither.
      return 0;
    };
  }

  const instance = await WebAssembly.instantiate(module, imports);
  const raw = instance.exports;
  if (!(raw.memory instanceof WebAssembly.Memory)) {
    throw new Error("wasm bridge does not export memory");
  }
  memory = raw.memory;

  const lookup = (name: string): RawFunction => {
    for (const [key, value] of Object.entries(raw)) {
      if (typeof value === "function" && (key === name || key.startsWith(`${name}_`))) {
        return value as RawFunction;
      }
    }
    throw new Error(`wasm bridge is missing export ${name}`);
  };
  const ackEdit = lookup("cb_ack_edit");

  return {
    memory: raw.memory,
    cb_protocol_version: lookup("cb_protocol_version"),
    cb_init: lookup("cb_init"),
    cb_tick: lookup("cb_tick"),
    cb_on_snapshot: lookup("cb_on_snapshot"),
    cb_local_edit: lookup("cb_local_edit"),
    cb_ack_edit: (seq: number, accepted: boolean) => ackEdit(seq, accepted ? 1 : 0),
    cb_dirty_chunks: lookup("cb_dirty_chunks"),
    cb_mesh_chunk: lookup("cb_mesh_chunk"),
    cb_release_mesh: lookup("cb_release_mesh"),
    cb_render_desc: lookup("cb_render_desc"),
    cb_set_quality: lookup("cb_set_quality"),
    cb_state_hash: lookup("cb_state_hash"),
    cb_mem_alloc: lookup("cb_mem_alloc"),
    cb_recipes: lookup("cb_recipes"),
    cb_near_miss: lookup("cb_near_miss"),
  };
}

function parseRecipeTable(bytes: Uint8Array): RecipeRow[] {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const decoder = new TextDecoder();
  let at = 0;
  const need = (count: number): void => {
    if (at + count > bytes.byteLength) {
      throw new Error("recipe table is truncated");
    }
  };
  need(2);
  const count = view.getUint16(at, true);
  at += 2;
  const rows: RecipeRow[] = [];
  for (let i = 0; i < count; i += 1) {
    need(11);
    const id = view.getUint16(at, true);
    const unlockSeconds = view.getUint16(at + 2, true);
    const cooldownSeconds = view.getUint16(at + 4, true);
    const output = view.getUint8(at + 6);
    const buddy = view.getUint8(at + 7);
    const cost = view.getUint16(at + 8, true);
    const nameLength = view.getUint8(at + 10);
    at += 11;
    need(nameLength + 2);
    const name = decoder.decode(bytes.subarray(at, at + nameLength));
    at += nameLength;
    const blockCount = view.getUint16(at, true);
    at += 2;
    need(blockCount * 3);
    const blocks: Array<[number, number, number]> = [];
    for (let b = 0; b < blockCount; b += 1) {
      blocks.push([
        view.getInt8(at) as number,
        view.getInt8(at + 1) as number,
        view.getInt8(at + 2) as number,
      ]);
      at += 3;
    }
    rows.push({ id, name, output, buddy, unlockSeconds, cooldownSeconds, cost, blocks });
  }
  return rows;
}

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
  cb_recipes(outPtr: number, outCap: number): number;
  cb_near_miss(ptr: number, count: number, elapsedSeconds: number): number;
}

/** One recipe row, decoded from the wasm table. */
export interface RecipeRow {
  id: number;
  name: string;
  output: number;
  buddy: number;
  unlockSeconds: number;
  cooldownSeconds: number;
  cost: number;
  blocks: Array<[number, number, number]>;
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
    if (tuning !== null && tuning.byteLength > 0) {
      // Before cb_init there is no bridge and no arena to copy into. The
      // embedded table is the only tuning the raw loader can use today; a
      // caller that really needs an override has to stage it through a future
      // pre-init allocation export.
      throw new Error("tuning overrides are not supported by the raw loader; pass null");
    }

    const ok = this.wasm.cb_init(seed >>> 0, 0, 0, mode) >>> 0;
    if (ok !== OK) {
      throw new Error("cb_init failed: tuning mismatch or invalid mode");
    }
    this.initialised = true;

    // One input frame lives in wasm memory for the whole session. Allocating it
    // once is what lets tick() avoid an alloc-and-rebuild on every frame. The
    // arena exists only after cb_init, which is why this comes second.
    this.inputPtr = this.alloc(INPUT_BYTES);
    this.inputView = new Uint8Array(this.wasm.memory.buffer, this.inputPtr, INPUT_BYTES);
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
      this.rebuildViews();
      return null;
    }
    const copy = new Uint8Array(this.wasm.memory.buffer, ptr, written).slice();
    this.wasm.cb_release_mesh(0);
    // Meshing builds a column and a mesh Vec on the Rust heap. That is an
    // allocation path outside cb_mem_alloc, and it can grow wasm memory just
    // the same, detaching every view. The copy is already out, so rebuilding
    // here is safe and keeps the next frame's dev assert honest.
    this.rebuildViews();
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

  /**
   * Hand one complete server frame to the bridge.
   *
   * `cb_on_snapshot` applies `S_CHUNK_DIFF` to the cached voxel columns and
   * records the rest for T-043. Chunk meshing and diff rows allocate on the
   * Rust heap, so rebuild the views after the call even though the staging
   * arena itself does not grow.
   */
  onServerMessage(bytes: Uint8Array): boolean {
    if (!this.initialised) {
      throw new Error("ClientWasm.onServerMessage before init");
    }
    if (bytes.byteLength === 0 || bytes.byteLength > 4096) {
      return false;
    }
    const ptr = this.alloc(bytes.byteLength);
    new Uint8Array(this.wasm.memory.buffer, ptr, bytes.byteLength).set(bytes);
    const ok = (this.wasm.cb_on_snapshot(ptr, bytes.byteLength) >>> 0) === OK;
    this.rebuildViews();
    return ok;
  }

  /** Select a quality tier (0..2). */
  setQuality(level: number): boolean {
    return (this.wasm.cb_set_quality(level) >>> 0) === OK;
  }

  /** FNV hash of the local state, for desync reports. */
  stateHash(): number {
    return this.wasm.cb_state_hash() >>> 0;
  }

  /** True when every cached view still points at live memory. */
  viewsAlive(): boolean {
    if (this.views === null) {
      return false;
    }
    return Object.values(this.views).every((view) => view.buffer.byteLength > 0);
  }

  /** The current frame's typed views, or null before `init`. */
  get renderViews(): RenderViews | null {
    return this.views;
  }

  /** Decode the embedded recipe table once at boot. */
  recipes(): RecipeRow[] {
    let capacity = 16 * 1024;
    for (let attempt = 0; attempt < 3; attempt += 1) {
      const ptr = this.alloc(capacity);
      const length = this.wasm.cb_recipes(ptr, capacity) >>> 0;
      if (length > 0) {
        const copy = new Uint8Array(this.wasm.memory.buffer, ptr, length).slice();
        this.rebuildViews();
        return parseRecipeTable(copy);
      }
      capacity *= 4;
    }
    throw new Error("cb_recipes did not fit its staging buffer");
  }

  /**
   * Ask Rust for the nearest unlocked recipe within two missing blocks.
   * Returns null when the cluster is exact or too far from any recipe.
   */
  nearMiss(
    blocks: ReadonlyArray<readonly [number, number, number]>,
    elapsedSeconds: number,
  ): { recipeId: number; missing: number } | null {
    if (blocks.length === 0 || blocks.length > 32) {
      return null;
    }
    const ptr = this.alloc(blocks.length * 3);
    const bytes = new Uint8Array(this.wasm.memory.buffer, ptr, blocks.length * 3);
    for (let i = 0; i < blocks.length; i += 1) {
      bytes[i * 3] = blocks[i][0] & 0xff;
      bytes[i * 3 + 1] = blocks[i][1] & 0xff;
      bytes[i * 3 + 2] = blocks[i][2] & 0xff;
    }
    const packed = this.wasm.cb_near_miss(ptr, blocks.length, elapsedSeconds) >>> 0;
    this.rebuildViews();
    if (packed === 0xffff_ffff) {
      return null;
    }
    return { recipeId: packed >>> 8, missing: packed & 0xff };
  }

  /** Bytes currently reserved by the wasm memory, including the arena. */
  memoryBytes(): number {
    return this.wasm.memory.buffer.byteLength;
  }

  /**
   * Grow wasm memory (in pages) and rebuild every view.
   *
   * The test hook uses this to reproduce the detach trap deliberately; the
   * rebuild is the same call every `cb_mem_alloc` makes.
   */
  growMemory(pages: number): { previousPages: number; bytes: number } {
    const previousPages = this.wasm.memory.grow(pages);
    this.rebuildViews();
    return { previousPages, bytes: this.memoryBytes() };
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
