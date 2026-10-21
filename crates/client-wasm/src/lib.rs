//! `client-wasm` — the browser-facing wasm boundary (plan §8.4, T-030).
//!
//! # The contract
//!
//! Thirteen exports, all returning a `u32` pointer-or-status. No `JsValue`
//! crosses the boundary, because every crossing is a JS object allocation and
//! the render path is supposed to allocate nothing. The render buffer is one
//! `#[repr(C)]` allocation: the header is at offset 0 and the entity arrays
//! follow it, so JS holds a `DataView` over wasm memory and reads fields by
//! offset.
//!
//! # What this task owns, and what it does not
//!
//! T-030 is the ABI and the local input path: the bridge simulates one local
//! body against a flat ground, writes a `GpuPlayer`, and exposes the buffer.
//! Terrain streaming (T-032), interpolation (T-043) and snapshot
//! reconciliation (T-041) attach to the same exports without moving the
//! header. `cb_on_snapshot`, `cb_dirty_chunks` and `cb_mesh_chunk` accept and
//! return the right shapes but leave their payloads to those tasks; the bridge
//! is deliberately not a second match loop.
//!
//! # Zero allocations in `cb_tick`
//!
//! Every buffer is allocated in [`cb_init`]. The tick path touches fixed-size
//! arrays and stack values only, and
//! [`tick_allocates_nothing_after_warmup`] proves it with a counting allocator
//! rather than a comment.

use std::cell::RefCell;
use std::collections::BTreeMap;

use sim::fixed::yaw_to_rad;
use sim::input::InputFrame;
use sim::movement::{MoveConfig, MoveState};
use sim::player::PlayerBody;
use sim::time::{Tick, TICK_MS};
use sim::vec::Vec3;
use sim::world_query::VoxelQuery;
use wasm_bindgen::prelude::*;
use world::chunk::{Column, CHUNK_Y};
use world::gen::{WorldGen, MAP_COLS};
use world::mesh::mesh_with_neighbours;

pub mod predict;
mod render_buffer;

pub use render_buffer::{
    GpuPlayer, RenderBuffer, RenderDesc, EVENT_SLOT_BYTES, GPU_PLAYER_BYTES, MAX_DIRTY_CHUNKS,
    MAX_RENDER_BUDDIES, MAX_RENDER_EVENTS, MAX_RENDER_PARTICLES, MAX_RENDER_PLAYERS,
    MAX_RENDER_PROJECTILES, PARTICLE_SLOT_BYTES, RENDER_DESC_BYTES, RENDER_DESC_MAGIC,
};

/// Status code for success. Every `u32`-returning export uses 0 for failure.
pub const OK: u32 = 1;

/// Tuning is compared byte-for-byte when supplied. This is the v1 boundary:
/// the client ships the same `data/tuning.ron`, and a mismatch is a desync that
/// should fail at boot instead of producing two different games.
const ARENA_BYTES: usize = 4 * 1024 * 1024;

/// A tick is 50 ms; catch up at most this many ticks in one frame.
const MAX_CATCH_UP_TICKS: u32 = 4;

/// One local edit message, kept for T-034's rollback path.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LocalEdit {
    /// Block cell.
    pub x: i16,
    /// Block cell.
    pub y: i16,
    /// Block cell.
    pub z: i16,
    /// Block id.
    pub block: u8,
    /// Place or break.
    pub action: u8,
    /// Sequence number returned to JS.
    pub seq: u16,
}

struct Bridge {
    seed: u32,
    mode: u8,
    quality: u8,
    tick: u32,
    accum_ms: u32,
    body: PlayerBody,
    cfg: MoveConfig,
    buffer: Box<RenderBuffer>,
    desc_ptr: u32,
    arena: Vec<u8>,
    arena_offset: usize,
    last_edit: Option<LocalEdit>,
    next_edit_seq: u16,
    ack: Option<(u16, bool)>,
    last_snapshot: (u32, u32),
    buttons: u8,
    gen: WorldGen,
    columns: BTreeMap<(i32, i32), Column>,
}

impl Bridge {
    fn new(seed: u32, mode: u8) -> Bridge {
        let mut buffer = Box::new(RenderBuffer::new());
        buffer.stamp_pointers();
        buffer.desc.magic = RENDER_DESC_MAGIC;
        let desc_ptr = buffer.desc_ptr();

        let mut body = PlayerBody::spawned(Vec3::from_ints(0, 0, 0));
        // Spawn on the flat demo floor and let the first tick settle the body.
        body.state = MoveState::at(Vec3::from_ints(0, 0, 0));

        Bridge {
            seed,
            mode,
            quality: 1,
            tick: 0,
            accum_ms: 0,
            body,
            cfg: MoveConfig::from_tuning(sim::tuning::embedded()),
            buffer,
            desc_ptr,
            arena: vec![0u8; ARENA_BYTES],
            arena_offset: 0,
            last_edit: None,
            next_edit_seq: 1,
            ack: None,
            last_snapshot: (0, 0),
            buttons: 0,
            // The client's terrain must match the server's. T-032 has no room
            // handshake yet, so the bridge uses the two-base map the milestone
            // is built around; `cb_init` can grow a layout argument later
            // without changing any of the 13 signatures.
            gen: WorldGen::new(u64::from(seed), 2),
            columns: BTreeMap::new(),
        }
    }

    /// Generate a column once and keep it. The whole map is 16 x 16 columns,
    /// so the cache is bounded by the world size, not by how far a player walks.
    fn ensure_column(&mut self, cx: i32, cz: i32) {
        if self.columns.contains_key(&(cx, cz)) {
            return;
        }
        let column = self.gen.column(cx, cz);
        self.columns.insert((cx, cz), column);
    }

    /// Mesh one column with its four neighbours, as the §8.4 payload.
    ///
    /// Layout: `u32 vertex_bytes`, `u32 index_bytes`, `u32 height_bytes`,
    /// the 12-byte vertices, the `u32` triangle indices, then one `u8` top
    /// boundary per local column (16 x 16, x-major). The header exists because
    /// the mesher chooses the AO-safe diagonal per quad; regenerating indices
    /// in JS from vertex order would lose that choice. The height map is
    /// computed from the column itself, not from the vertices, so greedy
    /// merging cannot leave a local cell without a height.
    fn mesh_payload(&mut self, cx: i32, cz: i32) -> Vec<u8> {
        if !(0..MAP_COLS).contains(&cx) || !(0..MAP_COLS).contains(&cz) {
            return Vec::new();
        }
        for (nx, nz) in [(cx - 1, cz), (cx + 1, cz), (cx, cz - 1), (cx, cz + 1)] {
            if (0..MAP_COLS).contains(&nx) && (0..MAP_COLS).contains(&nz) {
                self.ensure_column(nx, nz);
            }
        }
        self.ensure_column(cx, cz);

        let columns = &self.columns;
        let Some(center) = columns.get(&(cx, cz)) else {
            return Vec::new();
        };
        let mesh = mesh_with_neighbours(
            center,
            columns.get(&(cx - 1, cz)),
            columns.get(&(cx + 1, cz)),
            columns.get(&(cx, cz - 1)),
            columns.get(&(cx, cz + 1)),
        );
        if mesh.is_empty() {
            return Vec::new();
        }

        let vertices = mesh.as_bytes();
        let index_bytes = mesh.indices.len() * 4;
        let mut heights = [0u8; 16 * 16];
        for lz in 0..16usize {
            for lx in 0..16usize {
                for y in (0..CHUNK_Y).rev() {
                    if center.get(lx, y, lz).0 != 0 {
                        heights[lz * 16 + lx] = (y + 1) as u8;
                        break;
                    }
                }
            }
        }

        let mut payload = Vec::with_capacity(12 + vertices.len() + index_bytes + heights.len());
        payload.extend_from_slice(&(vertices.len() as u32).to_le_bytes());
        payload.extend_from_slice(&(index_bytes as u32).to_le_bytes());
        payload.extend_from_slice(&(heights.len() as u32).to_le_bytes());
        payload.extend_from_slice(&vertices);
        for index in mesh.indices {
            payload.extend_from_slice(&index.to_le_bytes());
        }
        payload.extend_from_slice(&heights);
        payload
    }

    fn mesh_into(&mut self, cx: i32, cz: i32, out_ptr: u32, out_cap: u32) -> u32 {
        if out_ptr == 0 {
            return 0;
        }
        let payload = self.mesh_payload(cx, cz);
        if payload.is_empty() || payload.len() > out_cap as usize {
            return 0;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(payload.as_ptr(), out_ptr as *mut u8, payload.len());
        }
        payload.len() as u32
    }

    /// Advance at most four 50 ms steps and refresh the render buffer.
    fn tick(&mut self, dt_ms: u32, wire: Option<[u8; 4]>) -> u32 {
        let frame = match wire {
            Some(bytes) => decode_wire(bytes, self.tick),
            None => InputFrame {
                tick: Tick(self.tick),
                ..InputFrame::default()
            },
        };
        self.buttons = frame.buttons;

        self.accum_ms = self.accum_ms.saturating_add(dt_ms.min(TICK_MS * 5));
        let mut steps = 0;
        while self.accum_ms >= TICK_MS && steps < MAX_CATCH_UP_TICKS {
            let previous = self.body.prev_buttons;
            sim::player::apply(&mut self.body, &frame, previous, &FlatGround, &self.cfg);
            self.tick = self.tick.wrapping_add(1);
            self.accum_ms -= TICK_MS;
            steps += 1;
        }
        if steps == MAX_CATCH_UP_TICKS {
            // Drop the backlog rather than spiral. The next frame samples fresh
            // input; replaying a second of stale frames is worse than a hitch.
            self.accum_ms %= TICK_MS;
        }

        self.write_render();
        self.desc_ptr
    }

    fn write_render(&mut self) {
        let desc = &mut self.buffer.desc;
        desc.magic = RENDER_DESC_MAGIC;
        desc.tick = self.tick;
        desc.player_count = 1;
        desc.buddy_count = 0;
        desc.proj_count = 0;
        desc.particle_count = 0;
        desc.event_count = 0;
        desc.dirty_count = 0;

        let body = &self.body;
        let mut flags = 0u8;
        if body.hp > 0 {
            flags |= GpuPlayer::FLAG_ALIVE;
        }
        if body.state.on_ground {
            flags |= GpuPlayer::FLAG_ON_GROUND;
        }
        if self.buttons & sim::input::buttons::FIRE != 0 {
            flags |= GpuPlayer::FLAG_ATTACKING;
        }
        let player = &mut self.buffer.players[0];
        player.id = 0;
        player.team = 0;
        player.flags = flags;
        player.x = body.state.pos.x.to_f32();
        player.y = body.state.pos.y.to_f32();
        player.z = body.state.pos.z.to_f32();
        player.yaw = yaw_to_rad(body.yaw).to_f32();
        player.pitch = pitch_radians(body.pitch);
        player.anim = (self.tick % 256) as f32 / 256.0;
        player.hp = body.hp.clamp(0, 255) as u8;
        player.role = 0;
        player.pad = 0;
    }

    /// Bump allocation for JS-owned staging buffers.
    ///
    /// JS copies the bytes out of wasm memory before the next staging call, so
    /// wrapping at the end of the arena is safe. The allocation itself never
    /// grows the heap: T-030 reserves the arena once, which is what keeps
    /// `cb_mem_alloc` out of the frame path.
    fn alloc(&mut self, size: u32) -> u32 {
        let size = (size as usize + 7) & !7;
        if size == 0 || size > self.arena.len() {
            return 0;
        }
        if self.arena_offset + size > self.arena.len() {
            self.arena_offset = 0;
        }
        let ptr = self.arena.as_ptr() as usize + self.arena_offset;
        self.arena_offset += size;
        ptr as u32
    }

    fn state_hash(&self) -> u32 {
        let mut h = 0x811c_9dc5u32;
        let mut mix = |value: u32| {
            h ^= value;
            h = h.wrapping_mul(0x0100_0193);
        };
        mix(self.seed);
        mix(u32::from(self.mode));
        mix(u32::from(self.quality));
        mix(self.tick);
        mix(self.body.state.pos.x.to_raw() as u32);
        mix(self.body.state.pos.y.to_raw() as u32);
        mix(self.body.state.pos.z.to_raw() as u32);
        mix(self.body.hp as u32);
        mix(u32::from(self.body.yaw));
        mix(u32::from(self.body.pitch));
        mix(self.last_edit.map_or(0, |e| u32::from(e.seq)));
        h
    }
}

/// A floor at y = 0. T-030's local body only needs gravity and a surface; the
/// real voxel field arrives with the chunk pipeline.
struct FlatGround;

impl VoxelQuery for FlatGround {
    #[inline]
    fn solid(&self, _x: i32, y: i32, _z: i32) -> bool {
        y < 0
    }
}

thread_local! {
    static BRIDGE: RefCell<Option<Bridge>> = const { RefCell::new(None) };
}

fn with_bridge<R>(f: impl FnOnce(&mut Bridge) -> R) -> Option<R> {
    BRIDGE.with(|slot| slot.borrow_mut().as_mut().map(f))
}

fn decode_wire(bytes: [u8; 4], tick: u32) -> InputFrame {
    let axis = |nibble: u8| -> i8 {
        let nibble = (nibble & 0x0F) as i8;
        let signed = if nibble >= 8 { nibble - 16 } else { nibble };
        signed.saturating_mul(16)
    };
    InputFrame {
        tick: Tick(tick),
        move_x: axis(bytes[3] & 0x0F),
        move_z: axis(bytes[3] >> 4),
        yaw: bytes[1],
        pitch: bytes[2],
        buttons: bytes[0],
    }
}

fn pitch_radians(pitch: u8) -> f32 {
    let signed = pitch as i8 as f32;
    signed * (core::f32::consts::TAU / 256.0)
}

fn tuning_matches(ptr: u32, len: u32) -> bool {
    if len == 0 {
        return true;
    }
    if ptr == 0 {
        return false;
    }
    #[cfg(target_arch = "wasm32")]
    {
        let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) };
        bytes == sim::tuning::EMBEDDED_RON.as_bytes()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = ptr;
        false
    }
}

#[cfg(target_arch = "wasm32")]
fn read_wire(ptr: u32) -> Option<[u8; 4]> {
    if ptr == 0 {
        return None;
    }
    let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, 4) };
    Some([bytes[0], bytes[1], bytes[2], bytes[3]])
}

#[cfg(not(target_arch = "wasm32"))]
fn read_wire(ptr: u32) -> Option<[u8; 4]> {
    // A 64-bit native pointer does not fit the u32 ABI. Native tests exercise
    // the same path through `tick_wire_for_test`; the export itself only sees
    // the no-input case here.
    let _ = ptr;
    None
}

/// Protocol version the bridge speaks (§7).
#[wasm_bindgen]
pub fn cb_protocol_version() -> u32 {
    u32::from(protocol::PROTOCOL_VERSION)
}

/// Initialise the bridge. Returns [`OK`] or 0.
///
/// `tuning_ptr`/`tuning_len` may be `0/0`, meaning "use the embedded table".
/// A non-empty blob must match the embedded RON byte for byte.
#[wasm_bindgen]
pub fn cb_init(seed: u32, tuning_ptr: u32, tuning_len: u32, mode: u8) -> u32 {
    if mode > 1 || !tuning_matches(tuning_ptr, tuning_len) {
        return 0;
    }
    let bridge = Bridge::new(seed, mode);
    BRIDGE.with(|slot| *slot.borrow_mut() = Some(bridge));
    OK
}

/// Advance the local simulation and return the render header pointer.
///
/// `input_ptr` points at the 4-byte wire frame of §7.1, or is 0 for no input.
#[wasm_bindgen]
pub fn cb_tick(dt_ms: u32, input_ptr: u32) -> u32 {
    let wire = read_wire(input_ptr);
    with_bridge(|bridge| bridge.tick(dt_ms, wire)).unwrap_or(0)
}

/// Accept a snapshot from the server. Decoding lands with T-043; T-030 records
/// its size so a host can assert the 4 KB cap at the boundary.
#[wasm_bindgen]
pub fn cb_on_snapshot(ptr: u32, len: u32) -> u32 {
    if ptr == 0 || len == 0 || len > 4096 {
        return 0;
    }
    with_bridge(|bridge| {
        bridge.last_snapshot = (ptr, len);
        OK
    })
    .unwrap_or(0)
}

/// Queue one local edit and return its sequence number (0 when uninitialised).
#[wasm_bindgen]
pub fn cb_local_edit(x: i16, y: i16, z: i16, block: u8, action: u8) -> u32 {
    with_bridge(|bridge| {
        let seq = bridge.next_edit_seq;
        bridge.next_edit_seq = bridge.next_edit_seq.wrapping_add(1).max(1);
        bridge.last_edit = Some(LocalEdit {
            x,
            y,
            z,
            block,
            action,
            seq,
        });
        u32::from(seq)
    })
    .unwrap_or(0)
}

/// Record the server's answer to an edit. Returns [`OK`] or 0.
#[wasm_bindgen]
pub fn cb_ack_edit(seq: u16, accepted: bool) -> u32 {
    with_bridge(|bridge| {
        bridge.ack = Some((seq, accepted));
        OK
    })
    .unwrap_or(0)
}

/// Pointer to the dirty-chunk list; `desc.dirty_count` says how many entries.
#[wasm_bindgen]
pub fn cb_dirty_chunks() -> u32 {
    with_bridge(|bridge| bridge.buffer.dirty_ptr()).unwrap_or(0)
}

/// Write one chunk mesh into caller-provided wasm memory.
///
/// Returns bytes written, or 0 for an empty column, an out-of-map column, or a
/// buffer that is too small. The payload format is documented on
/// `Bridge::mesh_payload`: an 8-byte header, 12-byte vertices, then u32 indices.
#[wasm_bindgen]
pub fn cb_mesh_chunk(cx: i16, cz: i16, out_ptr: u32, out_cap: u32) -> u32 {
    with_bridge(|bridge| bridge.mesh_into(i32::from(cx), i32::from(cz), out_ptr, out_cap))
        .unwrap_or(0)
}

/// Release a mesh generation. T-032 owns generation lifetime; the arena reuses
/// space at wrap, so this is a no-op today and still returns success.
#[wasm_bindgen]
pub fn cb_release_mesh(generation: u32) -> u32 {
    let _ = generation;
    OK
}

/// Pointer to the render header.
#[wasm_bindgen]
pub fn cb_render_desc() -> u32 {
    with_bridge(|bridge| bridge.desc_ptr).unwrap_or(0)
}

/// Select a quality tier (0 low, 1 medium, 2 high). Returns [`OK`] or 0.
#[wasm_bindgen]
pub fn cb_set_quality(level: u8) -> u32 {
    if level > 2 {
        return 0;
    }
    with_bridge(|bridge| {
        bridge.quality = level;
        OK
    })
    .unwrap_or(0)
}

/// Serialise the embedded recipe table for the recipe book.
///
/// Layout: `u16 count`, then per recipe `u16 id`, `u16 unlock_s`, `u16
/// cooldown_s`, `u8 output_kind`, `u8 buddy_kind`, `u16 cost`, `u8 name_len`,
/// name bytes, `u16 block_count`, and `block_count` triples of `i8`.
///
/// The table is Rust's `RecipeBook::embedded()`, so the UI renders the same
/// rows the matcher uses. No shape data is duplicated in TypeScript.
#[wasm_bindgen]
pub fn cb_recipes(out_ptr: u32, out_cap: u32) -> u32 {
    if out_ptr == 0 {
        return 0;
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        // A 64-bit native pointer does not fit the u32 ABI; the serializer is
        // covered through `recipe_table_bytes` in the unit tests.
        let _ = (out_ptr, out_cap);
        0
    }
    #[cfg(target_arch = "wasm32")]
    {
        let bytes = recipe_table_bytes();
        if bytes.len() > out_cap as usize {
            return 0;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), out_ptr as *mut u8, bytes.len());
        }
        bytes.len() as u32
    }
}

/// Pack the nearest unlocked recipe within two missing blocks.
///
/// Returns `0xFFFF_FFFF` for no prediction, otherwise `recipe_id << 8 |
/// missing`. The cluster is `count` triples of `i8` at `ptr`.
#[wasm_bindgen]
pub fn cb_near_miss(ptr: u32, count: u32, elapsed_s: u32) -> u32 {
    if ptr == 0 || count == 0 || count as usize > sim::recipe::MAX_CLUSTER {
        return u32::MAX;
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = elapsed_s;
        u32::MAX
    }
    #[cfg(target_arch = "wasm32")]
    {
        let raw = unsafe { core::slice::from_raw_parts(ptr as *const i8, count as usize * 3) };
        let mut blocks = Vec::with_capacity(count as usize);
        for i in 0..count as usize {
            blocks.push([
                i32::from(raw[i * 3]),
                i32::from(raw[i * 3 + 1]),
                i32::from(raw[i * 3 + 2]),
            ]);
        }
        match sim::recipe::near_miss(&blocks, sim::recipe::RecipeBook::embedded(), elapsed_s) {
            Some(near) => (u32::from(near.recipe.0) << 8) | near.missing(),
            None => u32::MAX,
        }
    }
}

#[cfg(any(target_arch = "wasm32", test))]
fn recipe_table_bytes() -> Vec<u8> {
    use sim::recipe::RecipeOutput;
    let book = sim::recipe::RecipeBook::embedded();
    let mut out = Vec::new();
    out.extend_from_slice(&(book.len() as u16).to_le_bytes());
    for recipe in book.recipes() {
        let (kind, buddy) = match recipe.output {
            RecipeOutput::Sword => (0u8, 0u8),
            RecipeOutput::Bow => (1, 0),
            RecipeOutput::Bandage => (2, 0),
            RecipeOutput::Spear => (3, 0),
            RecipeOutput::Tower => (4, 0),
            RecipeOutput::Ram => (5, 0),
            RecipeOutput::Catapult => (6, 0),
            RecipeOutput::Ward => (7, 0),
            RecipeOutput::Buddy(kind) => (8, kind as u8),
        };
        out.extend_from_slice(&recipe.id.0.to_le_bytes());
        out.extend_from_slice(&(recipe.unlock_s as u16).to_le_bytes());
        out.extend_from_slice(&(recipe.cooldown_s as u16).to_le_bytes());
        out.push(kind);
        out.push(buddy);
        out.extend_from_slice(&(recipe.cost() as u16).to_le_bytes());
        let name = recipe.name.as_bytes();
        out.push(name.len() as u8);
        out.extend_from_slice(name);
        out.extend_from_slice(&(recipe.blocks.len() as u16).to_le_bytes());
        for block in &recipe.blocks {
            out.push(block[0] as i8 as u8);
            out.push(block[1] as i8 as u8);
            out.push(block[2] as i8 as u8);
        }
    }
    out
}

/// Cheap FNV hash of the local state, for desync diagnostics.
#[wasm_bindgen]
pub fn cb_state_hash() -> u32 {
    with_bridge(|bridge| bridge.state_hash()).unwrap_or(0)
}

/// Reserve `size` bytes of wasm memory for a JS-owned staging buffer.
#[wasm_bindgen]
pub fn cb_mem_alloc(size: u32) -> u32 {
    with_bridge(|bridge| bridge.alloc(size)).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim::input::buttons;

    #[test]
    fn exports_exist_and_are_callable() {
        assert_eq!(cb_protocol_version(), u32::from(protocol::PROTOCOL_VERSION));
        assert_eq!(cb_init(7, 0, 0, 0), OK);
        let _ = cb_render_desc();
        let _ = cb_tick(50, 0);
        assert_eq!(cb_on_snapshot(0, 0), 0);
        assert_eq!(cb_on_snapshot(0x1000, 16), OK);
        assert_ne!(cb_local_edit(0, 0, 0, 1, 0), 0);
        assert_eq!(cb_ack_edit(1, true), OK);
        let _ = cb_dirty_chunks();
        assert_eq!(cb_mesh_chunk(0, 0, 0, 0), 0);
        assert_eq!(cb_release_mesh(1), OK);
        assert_eq!(cb_set_quality(3), 0);
        assert_eq!(cb_set_quality(2), OK);
        let _ = cb_state_hash();
        let _ = cb_mem_alloc(64);
    }

    #[test]
    fn render_desc_magic_detects_stale_buffer() {
        assert_eq!(cb_init(1, 0, 0, 0), OK);
        cb_tick(50, 0);
        assert!(bridge_desc().expect("bridge initialised").is_valid());

        // A zeroed or stale header must fail the guard rather than be read as
        // a render frame with zero entities at tick 0.
        with_bridge(|bridge| bridge.buffer.desc.magic = 0).expect("bridge initialised");
        assert!(!bridge_desc().expect("bridge initialised").is_valid());

        cb_tick(50, 0);
        assert!(bridge_desc().expect("bridge initialised").is_valid());
    }

    #[test]
    fn recipe_table_has_fourteen_rows() {
        let bytes = recipe_table_bytes();
        assert!(bytes.len() > 2);
        let count = u16::from_le_bytes([bytes[0], bytes[1]]);
        assert_eq!(count, 14, "section 4.5.3 lists 14 recipes");
    }

    #[test]
    fn near_miss_export_rejects_an_empty_cluster() {
        assert_eq!(cb_near_miss(0, 0, 0), u32::MAX);
    }

    #[test]
    fn mesh_payload_is_well_formed_and_deterministic() {
        assert_eq!(cb_init(0x00C0_FFEE, 0, 0, 0), OK);
        let first = with_bridge(|bridge| bridge.mesh_payload(8, 8)).expect("bridge initialised");
        assert!(first.len() > 8, "the centre column must mesh to something");

        let vertex_bytes = u32::from_le_bytes(first[0..4].try_into().unwrap()) as usize;
        let index_bytes = u32::from_le_bytes(first[4..8].try_into().unwrap()) as usize;
        let height_bytes = u32::from_le_bytes(first[8..12].try_into().unwrap()) as usize;
        assert_eq!(vertex_bytes % 12, 0, "vertices are 12 bytes each");
        assert_eq!(index_bytes % 4, 0, "indices are u32");
        assert_eq!(height_bytes, 16 * 16);
        assert_eq!(first.len(), 12 + vertex_bytes + index_bytes + height_bytes);
        assert!(index_bytes > 0);
        assert!(
            first[12 + vertex_bytes + index_bytes..]
                .iter()
                .any(|h| *h > 0),
            "the centre column has a height map"
        );

        let second = with_bridge(|bridge| bridge.mesh_payload(8, 8)).expect("bridge initialised");
        assert_eq!(first, second, "meshing one column twice must be identical");
    }

    #[test]
    fn mesh_outside_the_map_is_empty() {
        assert_eq!(cb_init(5, 0, 0, 0), OK);
        for (cx, cz) in [(-1, 0), (0, -1), (MAP_COLS, 0), (0, MAP_COLS)] {
            assert!(
                with_bridge(|bridge| bridge.mesh_payload(cx, cz))
                    .expect("bridge initialised")
                    .is_empty(),
                "column ({cx}, {cz}) is outside the map"
            );
        }
    }

    #[test]
    fn mesh_refuses_a_short_buffer() {
        assert_eq!(cb_init(6, 0, 0, 0), OK);
        assert_eq!(cb_mesh_chunk(8, 8, 1, 8), 0);
    }

    #[test]
    fn wire_input_moves_the_body() {
        assert_eq!(cb_init(2, 0, 0, 0), OK);
        let before = with_bridge(|bridge| bridge.body.state.pos).expect("bridge initialised");
        // Forward is +Z; the high nibble carries move_z.
        for _ in 0..10 {
            tick_wire_for_test([0, 0, 0, 0x70]);
        }
        let after = with_bridge(|bridge| bridge.body.state.pos).expect("bridge initialised");
        assert!(
            after.z > before.z,
            "forward input did not move the body: {before:?} -> {after:?}"
        );
    }

    #[test]
    fn tick_allocates_nothing_after_warmup() {
        assert_eq!(cb_init(3, 0, 0, 0), OK);

        // Prove the counter is wired before trusting a zero. A counting
        // allocator that is not installed reports zero allocations for every
        // program, which is the one failure mode this test must not have.
        allocations::reset();
        let probe = vec![0u8; 64];
        assert!(
            allocations::count() > 0,
            "the counting allocator is not installed; the zero result would be meaningless"
        );
        drop(probe);

        for _ in 0..10 {
            cb_tick(TICK_MS, 0);
        }
        allocations::reset();
        for _ in 0..600 {
            cb_tick(TICK_MS, 0);
        }
        assert_eq!(
            allocations::count(),
            0,
            "cb_tick allocated after warm-up; the render path must not touch the heap"
        );
    }

    fn bridge_desc() -> Option<RenderDesc> {
        with_bridge(|bridge| bridge.buffer.desc)
    }

    fn tick_wire_for_test(bytes: [u8; 4]) -> u32 {
        with_bridge(|bridge| bridge.tick(TICK_MS, Some(bytes))).unwrap_or(0)
    }

    #[test]
    fn wire_nibbles_are_signed() {
        assert_eq!(decode_wire([0, 0, 0, 0x00], 0).move_z, 0);
        assert_eq!(decode_wire([0, 0, 0, 0x70], 0).move_z, 112);
        assert_eq!(decode_wire([0, 0, 0, 0x80], 0).move_z, -128);
        assert_eq!(decode_wire([0, 0, 0, 0x07], 0).move_x, 112);
    }

    #[test]
    fn buttons_pass_through_the_wire_frame() {
        let frame = decode_wire([buttons::JUMP | buttons::FIRE, 64, 192, 0], 9);
        assert!(frame.held(buttons::JUMP));
        assert!(frame.held(buttons::FIRE));
        assert_eq!(frame.yaw, 64);
        assert_eq!(frame.pitch, 192);
        assert_eq!(frame.tick, Tick(9));
    }

    mod allocations {
        extern crate std;

        use std::alloc::{GlobalAlloc, Layout, System};
        use std::cell::Cell;

        thread_local! {
            static COUNT: Cell<usize> = const { Cell::new(0) };
        }

        pub struct Counting;

        fn bump() {
            let _ = COUNT.try_with(|count| count.set(count.get() + 1));
        }

        unsafe impl GlobalAlloc for Counting {
            unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
                bump();
                System.alloc(layout)
            }

            unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
                bump();
                System.alloc_zeroed(layout)
            }

            unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
                System.dealloc(ptr, layout)
            }

            unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
                bump();
                System.realloc(ptr, layout, new_size)
            }
        }

        #[global_allocator]
        static GLOBAL: Counting = Counting;

        pub fn reset() {
            let _ = COUNT.try_with(|count| count.set(0));
        }

        pub fn count() -> usize {
            COUNT.try_with(|count| count.get()).unwrap_or(0)
        }
    }
}
