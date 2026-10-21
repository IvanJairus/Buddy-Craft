//! The byte-stable render buffer the JS side reads directly.
//!
//! Every type here is `#[repr(C)]` and every field is a fixed-width integer or
//! an `f32`. JS creates a `DataView` over the buffer once and reads offsets; a
//! field reorder or a `usize` sneaking in would silently shift every later
//! field, so [`layout_is_stable`](tests::render_desc_layout_is_stable) pins
//! sizes and offsets to the numbers in plan §8.4.
//!
//! # Why the header and arrays are one allocation
//!
//! `cb_render_desc()` returns a pointer to the header and the entity arrays
//! follow immediately, so one pointer arithmetic in JS finds all of them. That
//! only holds if the arrays are fields of the same `#[repr(C)]` struct, in
//! order, which is what [`RenderBuffer`] is.

use core::mem::size_of;

/// Plan §8.4: the header magic. A zero or stale buffer fails this check.
pub const RENDER_DESC_MAGIC: u32 = 0xCBA7_E001;

/// Players the render buffer can describe (§8.4).
pub const MAX_RENDER_PLAYERS: usize = 18;
/// Buddies the render buffer can describe (§8.4).
pub const MAX_RENDER_BUDDIES: usize = 64;
/// Projectiles the render buffer can describe (§8.4).
pub const MAX_RENDER_PROJECTILES: usize = 128;
/// Particles the render buffer can describe (§8.4).
pub const MAX_RENDER_PARTICLES: usize = 512;
/// Events per frame. The plan does not pin a count; 64 is above the worst
/// observed burst (a wipe plus a craft) and keeps the header arrays small.
pub const MAX_RENDER_EVENTS: usize = 64;
/// Dirty chunk entries. The world has 256 columns, so this is every column.
pub const MAX_DIRTY_CHUNKS: usize = 256;

/// Bytes per particle slot until T-033 pins the layout.
pub const PARTICLE_SLOT_BYTES: usize = 16;
/// Bytes per event slot until T-044 pins the layout.
pub const EVENT_SLOT_BYTES: usize = 16;

/// Header at the front of the render buffer (plan §8.4, 52 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderDesc {
    /// [`RENDER_DESC_MAGIC`], or 0 for a buffer that has never been written.
    pub magic: u32,
    /// Authoritative tick the frame describes.
    pub tick: u32,
    /// Number of valid entries in the player array.
    pub player_count: u32,
    /// Number of valid entries in the buddy array.
    pub buddy_count: u32,
    /// Number of valid entries in the projectile array.
    pub proj_count: u32,
    /// Number of valid entries in the particle array.
    pub particle_count: u32,
    /// Number of valid entries in the event array.
    pub event_count: u32,
    /// Number of valid 4-word entries in the dirty-chunk array.
    pub dirty_count: u32,
    /// Absolute wasm address of the player array.
    pub player_ptr: u32,
    /// Absolute wasm address of the buddy array.
    pub buddy_ptr: u32,
    /// Absolute wasm address of the projectile array.
    pub proj_ptr: u32,
    /// Absolute wasm address of the particle array.
    pub particle_ptr: u32,
    /// Absolute wasm address of the event array.
    pub event_ptr: u32,
}

impl RenderDesc {
    /// True when the magic word says this buffer was written by the bridge.
    #[inline]
    pub fn is_valid(&self) -> bool {
        self.magic == RENDER_DESC_MAGIC
    }
}

/// One player as the GPU sees it (plan §8.4, 32 bytes).
///
/// Positions and angles are `f32` because the renderer reads floats; the
/// authoritative state stays fixed-point. Conversion happens once per entity
/// per frame, at this boundary.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GpuPlayer {
    /// Network entity id, low 16 bits.
    pub id: u16,
    /// Team index.
    pub team: u8,
    /// Bit 0 alive, bit 1 on ground. The remaining bits are reserved.
    pub flags: u8,
    /// Feet position, metres.
    pub x: f32,
    /// Feet position, metres.
    pub y: f32,
    /// Feet position, metres.
    pub z: f32,
    /// Facing, radians.
    pub yaw: f32,
    /// Pitch, radians.
    pub pitch: f32,
    /// Animation phase in turns, wrapped by the renderer.
    pub anim: f32,
    /// Hit points, clamped to a byte.
    pub hp: u8,
    /// 0 for a player; buddy roles use their roster index.
    pub role: u8,
    /// Pad or owner index. Unused for players in v1.
    pub pad: u16,
}

impl GpuPlayer {
    /// Bit 0 of [`GpuPlayer::flags`].
    pub const FLAG_ALIVE: u8 = 1 << 0;
    /// Bit 1 of [`GpuPlayer::flags`].
    pub const FLAG_ON_GROUND: u8 = 1 << 1;
}

/// The whole render buffer: one header followed by every array it points at.
///
/// Particles, events and dirty entries are raw 4-word slots. Only the player
/// and buddy layouts are pinned by §8.4; T-033 and T-044 own the other two and
/// will replace the slots with named structs without moving the header.
#[repr(C)]
pub struct RenderBuffer {
    /// The header JS reads first.
    pub desc: RenderDesc,
    /// Player entities, `player_count` valid.
    pub players: [GpuPlayer; MAX_RENDER_PLAYERS],
    /// Buddy entities, `buddy_count` valid. Same 32-byte layout as a player.
    pub buddies: [GpuPlayer; MAX_RENDER_BUDDIES],
    /// Projectiles, `proj_count` valid. Same 32-byte layout as a player.
    pub projectiles: [GpuPlayer; MAX_RENDER_PROJECTILES],
    /// Particle slots, `particle_count` valid, 16 bytes each.
    pub particles: [[u32; 4]; MAX_RENDER_PARTICLES],
    /// Event slots, `event_count` valid, 16 bytes each.
    pub events: [[u32; 4]; MAX_RENDER_EVENTS],
    /// Dirty chunk entries: `[cx, cz, kind, reserved]`.
    pub dirty: [[u32; 4]; MAX_DIRTY_CHUNKS],
}

impl Default for RenderBuffer {
    fn default() -> Self {
        RenderBuffer::new()
    }
}

impl RenderBuffer {
    /// A zeroed buffer. The magic stays 0 until the first tick writes it.
    pub fn new() -> RenderBuffer {
        RenderBuffer {
            desc: RenderDesc::default(),
            players: [GpuPlayer::default(); MAX_RENDER_PLAYERS],
            buddies: [GpuPlayer::default(); MAX_RENDER_BUDDIES],
            projectiles: [GpuPlayer::default(); MAX_RENDER_PROJECTILES],
            particles: [[0; 4]; MAX_RENDER_PARTICLES],
            events: [[0; 4]; MAX_RENDER_EVENTS],
            dirty: [[0; 4]; MAX_DIRTY_CHUNKS],
        }
    }

    /// Fill the header pointers from the arrays' own addresses.
    ///
    /// Called once, after the buffer is boxed, so every pointer is the final
    /// address. Recomputing them per tick would also be correct but pointless:
    /// the arrays never move.
    pub fn stamp_pointers(&mut self) {
        let players = self.players.as_ptr() as u32;
        let buddies = self.buddies.as_ptr() as u32;
        let projectiles = self.projectiles.as_ptr() as u32;
        let particles = self.particles.as_ptr() as u32;
        let events = self.events.as_ptr() as u32;
        self.desc.player_ptr = players;
        self.desc.buddy_ptr = buddies;
        self.desc.proj_ptr = projectiles;
        self.desc.particle_ptr = particles;
        self.desc.event_ptr = events;
    }

    /// Absolute address of the header, which is also the buffer address.
    pub fn desc_ptr(&self) -> u32 {
        self as *const RenderBuffer as u32
    }

    /// Absolute address of the dirty-chunk array.
    pub fn dirty_ptr(&self) -> u32 {
        self.dirty.as_ptr() as u32
    }
}

/// Header size asserted against §8.4.
pub const RENDER_DESC_BYTES: usize = size_of::<RenderDesc>();
/// Player stride asserted against §8.4.
pub const GPU_PLAYER_BYTES: usize = size_of::<GpuPlayer>();

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::offset_of;

    #[test]
    fn render_desc_layout_is_stable() {
        assert_eq!(size_of::<RenderDesc>(), 52);
        assert_eq!(offset_of!(RenderDesc, magic), 0);
        assert_eq!(offset_of!(RenderDesc, tick), 4);
        assert_eq!(offset_of!(RenderDesc, player_count), 8);
        assert_eq!(offset_of!(RenderDesc, buddy_count), 12);
        assert_eq!(offset_of!(RenderDesc, proj_count), 16);
        assert_eq!(offset_of!(RenderDesc, particle_count), 20);
        assert_eq!(offset_of!(RenderDesc, event_count), 24);
        assert_eq!(offset_of!(RenderDesc, dirty_count), 28);
        assert_eq!(offset_of!(RenderDesc, player_ptr), 32);
        assert_eq!(offset_of!(RenderDesc, buddy_ptr), 36);
        assert_eq!(offset_of!(RenderDesc, proj_ptr), 40);
        assert_eq!(offset_of!(RenderDesc, particle_ptr), 44);
        assert_eq!(offset_of!(RenderDesc, event_ptr), 48);
    }

    #[test]
    fn gpu_player_layout_is_stable() {
        assert_eq!(size_of::<GpuPlayer>(), 32);
        assert_eq!(offset_of!(GpuPlayer, id), 0);
        assert_eq!(offset_of!(GpuPlayer, team), 2);
        assert_eq!(offset_of!(GpuPlayer, flags), 3);
        assert_eq!(offset_of!(GpuPlayer, x), 4);
        assert_eq!(offset_of!(GpuPlayer, y), 8);
        assert_eq!(offset_of!(GpuPlayer, z), 12);
        assert_eq!(offset_of!(GpuPlayer, yaw), 16);
        assert_eq!(offset_of!(GpuPlayer, pitch), 20);
        assert_eq!(offset_of!(GpuPlayer, anim), 24);
        assert_eq!(offset_of!(GpuPlayer, hp), 28);
        assert_eq!(offset_of!(GpuPlayer, role), 29);
        assert_eq!(offset_of!(GpuPlayer, pad), 30);
    }

    #[test]
    fn arrays_follow_the_header_without_a_gap() {
        assert_eq!(offset_of!(RenderBuffer, desc), 0);
        assert_eq!(offset_of!(RenderBuffer, players), 52);
        assert_eq!(
            offset_of!(RenderBuffer, buddies),
            52 + MAX_RENDER_PLAYERS * 32
        );
        assert_eq!(
            offset_of!(RenderBuffer, projectiles),
            52 + (MAX_RENDER_PLAYERS + MAX_RENDER_BUDDIES) * 32
        );
    }

    #[test]
    fn zeroed_header_fails_the_magic_guard() {
        assert!(!RenderDesc::default().is_valid());
        let desc = RenderDesc {
            magic: RENDER_DESC_MAGIC,
            ..RenderDesc::default()
        };
        assert!(desc.is_valid());
    }
}
