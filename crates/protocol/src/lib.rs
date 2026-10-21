//! `protocol` — the wire contract (plan §7).
//!
//! # Framing
//!
//! One message per WebSocket frame. The first byte is the message tag; the rest
//! is a postcard-encoded payload. No length prefix — the WebSocket frame already
//! delimits the message — and no compression, because `permessage-deflate` costs
//! CPU per message on the server for payloads that are already compact.
//!
//! # Why postcard
//!
//! Postcard is a varint format with no schema and no field names on the wire.
//! That matters more than it sounds: `S_SNAPSHOT` has to fit 18 entities in
//! 2 KB at 20 Hz per client, and §7.3's whole layout is built on the assumption
//! that a position delta costs one to three bytes rather than four. A format
//! with fixed-width integers would spend 4 bytes on a coordinate that moved by
//! 3 cm.
//!
//! §7 says fixed-size structs use `#[repr(C)]` plus a hand-written `Serialize`
//! "where byte layout matters". **No message here does that**, and it is
//! deliberate: `#[repr(C)]` would *pin* fields to their fixed widths and defeat
//! the varints that make the snapshot budget reachable. The types below are
//! plain derived structs, so the layout is postcard's, and the size test in this
//! module is what keeps it honest.
//!
//! # What is not here
//!
//! Validation. [`C2s::decode`] only rejects an empty frame and an unknown tag;
//! everything in §7.4 — the 4 KB cap, the token bucket, chat and name
//! sanitisation, bounds checks — lives in `codec` (T-018) so that "what a
//! message is" and "what a server will accept" stay separable. A client and a
//! test harness want the first without the second.

use serde::de::{self, SeqAccess, Visitor};
use serde::ser::{SerializeStruct, SerializeTuple};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use sim::{InputFrame, Tick};

/// The protocol version (§7). A client whose version differs is rejected at
/// `C_HELLO` with `S_ERROR { code: VERSION_MISMATCH }` and a readable message,
/// never a silent disconnect.
pub const PROTOCOL_VERSION: u16 = 1;

pub mod codec;

// ---------------------------------------------------------------------------
// Limits
// ---------------------------------------------------------------------------
//
// These are *structural*: the types below cannot represent more than this, so
// the decoder cannot be made to allocate more than this before validation runs.
// A limit that only exists inside a validation function is a limit an attacker
// reaches the allocator before.

/// Largest accepted inbound frame (§7.4 rule 1).
pub const MAX_FRAME_BYTES: usize = 4096;

/// Players per room (§6.2).
pub const MAX_PLAYERS: usize = 6;

/// Teams per room. §4.7's largest shape is 2v2v2, and FFA at six players gives
/// six teams.
pub const MAX_TEAMS: usize = 6;

/// Buddies in one snapshot. §4.7 caps a team at five, but `sim::MAX_ENTITIES`
/// is 18, so 12 buddies alongside 6 players is the real ceiling.
pub const MAX_BUDDIES: usize = 12;

/// Projectiles in one snapshot (§7.3, §8.3).
pub const MAX_PROJECTILES: usize = 24;

/// Siege structures in one snapshot (§8.3).
pub const MAX_STRUCTURES: usize = 4;

/// Events in one snapshot. Craft, kill, base hit and elimination are the §6.3
/// "never culled" set; 16 is a comfortable ceiling for one 50 ms tick.
pub const MAX_EVENTS: usize = 16;

/// Base spawns in `S_START`. One per team.
pub const MAX_BASES: usize = MAX_TEAMS;

/// Entries in one `S_CHUNK_DIFF`. Derived from the 1.5 KB diff budget in §6.7:
/// 1,536 − header(9) leaves 1,527, at 3 bytes per `(u16, u8)` entry that is 509.
/// Rounded down to 500 so the header has slack.
pub const MAX_DIFF_ENTRIES: usize = 500;

/// Bytes in a player name (`C_HELLO`, `S_LOBBY`).
pub const NAME_LEN: usize = 16;

/// Bytes in a room code.
pub const ROOM_CODE_LEN: usize = 6;

/// Bytes in a reconnect token.
pub const TOKEN_LEN: usize = 16;

/// Bytes in a chat message (§7.1 `C_CHAT`).
pub const CHAT_LEN: usize = 120;

/// Bytes in an `S_ERROR` detail string.
pub const ERROR_DETAIL_LEN: usize = 64;

/// Input frames per `C_INPUT`. Three, so one lost packet costs nothing (§6.2).
pub const INPUT_FRAMES: usize = 3;

// ---------------------------------------------------------------------------
// Enumerated wire values
// ---------------------------------------------------------------------------
//
// These are `u8` fields rather than Rust enums on purpose: an unknown value from
// a newer or buggier peer must be *representable* so the server can answer with
// a reason instead of failing to decode the whole message. Validation rejects
// out-of-range values explicitly.

/// `RoomConfig.mode` and `RoomSettings.mode`.
pub mod room_mode {
    /// Every player is their own team.
    pub const FFA: u8 = 0;
    /// Players are grouped into two or three teams.
    pub const TEAMS: u8 = 1;
}

/// `Edit.action` (§7.1 `C_EDIT`).
pub mod edit_action {
    /// Remove a block.
    pub const BREAK: u8 = 0;
    /// Place a block.
    pub const PLACE: u8 = 1;
}

/// `EditAck.reason` (§7.2). `NONE` on acceptance.
pub mod edit_reject {
    /// Accepted.
    pub const NONE: u8 = 0;
    /// Outside the world bounds. Not a close: players aim at the sky.
    pub const OUT_OF_BOUNDS: u8 = 1;
    /// Further than the 6 m reach (§6.6).
    pub const TOO_FAR: u8 = 2;
    /// The cell is already occupied, or there is nothing to break.
    pub const OCCUPIED: u8 = 3;
    /// Inside a protected volume (a core, a pad floor).
    pub const PROTECTED: u8 = 4;
    /// The per-connection edit token bucket is empty.
    pub const RATE_LIMITED: u8 = 5;
    /// The block id is not in the registry.
    pub const UNKNOWN_BLOCK: u8 = 6;
}

/// `End.reason` (§7.2).
///
/// The spec lists three values. There are four, because `sim::EndReason` has
/// three variants and `Draw` is not one of the spec's three: a match where every
/// core falls on the same tick is neither "core destroyed" (which implies a
/// winner) nor "no humans" nor "overtime". Sending `OVERTIME` for it would tell
/// the results screen to say "time ran out" after a mutual kill.
pub mod end_reason {
    /// One team's core was the last one standing.
    pub const CORE_DESTROYED: u8 = 0;
    /// The last human was eliminated (D-05). `winner_team` is `-1`.
    pub const NO_HUMANS: u8 = 1;
    /// Reserved: decay decided the match. Never emitted in v1 — `Outcome`
    /// records the result, not the mechanism, so a decay-decided match ends as
    /// `CORE_DESTROYED` or `DRAW` depending on how many cores fell.
    pub const OVERTIME: u8 = 2;
    /// Every core fell on the same tick.
    pub const DRAW: u8 = 3;
}

/// `S_ERROR.code` (§7.2).
pub mod error_code {
    /// The client's `PROTOCOL_VERSION` is outside the accepted range.
    pub const VERSION_MISMATCH: u8 = 1;
    /// No room with that code.
    pub const ROOM_NOT_FOUND: u8 = 2;
    /// The room is full or already started.
    pub const ROOM_FULL: u8 = 3;
    /// The name is taken in this room.
    pub const NAME_TAKEN: u8 = 4;
    /// The message failed validation.
    pub const BAD_MESSAGE: u8 = 5;
    /// The token bucket is empty (§7.4 rule 6).
    pub const RATE_LIMITED: u8 = 6;
    /// The room hit an internal invariant. The client should not retry.
    pub const INTERNAL: u8 = 7;
}

/// `SnapshotPlayer.presence` bits (§7.3).
pub mod presence {
    /// Position fields changed.
    pub const POS: u16 = 1 << 0;
    /// Yaw changed.
    pub const YAW: u16 = 1 << 1;
    /// HP changed.
    pub const HP: u16 = 1 << 2;
    /// Flags changed.
    pub const FLAGS: u16 = 1 << 3;
    /// Animation phase changed.
    pub const ANIM: u16 = 1 << 4;
    /// Every field present, i.e. a keyframe entry.
    pub const ALL: u16 = 0x1F;
}

/// `SnapshotPlayer.flags`.
pub mod player_flags {
    /// The player is alive.
    pub const ALIVE: u8 = 1 << 0;
    /// The player is a bot (D-05: shown differently, and cannot win).
    pub const BOT: u8 = 1 << 1;
    /// Mid-swing this tick.
    pub const ATTACKING: u8 = 1 << 2;
    /// Carrying crates.
    pub const CARRYING: u8 = 1 << 3;
    /// Outside the 48 m interest radius of the receiving client, so its
    /// position is a last-known value rather than a live one.
    pub const STALE: u8 = 1 << 4;
}

/// `SnapshotEvent.kind` and `Event.kind`.
pub mod event_kind {
    /// A recipe fired. `a` = team, `b` = recipe id, `c` = output count.
    pub const CRAFT: u8 = 0;
    /// An entity died. `a` = entity id, `b` = killer id, `c` = damage kind.
    pub const KILL: u8 = 1;
    /// A core took damage. `a` = team, `b` = hp after, `c` = attacker id.
    pub const BASE_HIT: u8 = 2;
    /// A team was eliminated. `a` = team, `b` = tick, `c` = unused.
    pub const ELIMINATION: u8 = 3;
    /// An airdrop landed. `a` = x, `b` = z, `c` = crate count.
    pub const AIRDROP: u8 = 4;
}

// ---------------------------------------------------------------------------
// Wire primitives
// ---------------------------------------------------------------------------

/// A fixed-size byte string that costs exactly `N` bytes on the wire.
///
/// serde's own array impls stop at 32 elements, and the two arrays that matter
/// most here — chat text (120) and an error detail (64) — are past that. A `Vec`
/// would work but would put the length on the wire and let a peer ask the
/// decoder for an arbitrary buffer before any validation runs; `FixedBytes` is a
/// bound the *type* enforces, not a rule a validator has to remember.
///
/// Encoding is `serialize_tuple`, so there is no length prefix: 120 bytes of
/// chat cost 120 bytes, not 121.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct FixedBytes<const N: usize>(pub [u8; N]);

impl<const N: usize> Default for FixedBytes<N> {
    fn default() -> Self {
        FixedBytes([0u8; N])
    }
}

impl<const N: usize> FixedBytes<N> {
    /// A zeroed buffer.
    pub const fn new() -> Self {
        FixedBytes([0u8; N])
    }

    /// Copy `text` into a zeroed buffer, truncating at `N` bytes.
    ///
    /// No validation: sanitisation is `codec`'s job. This only makes tests and
    /// fixtures readable.
    pub fn from_str_lossy(text: &str) -> Self {
        let mut out = [0u8; N];
        let bytes = text.as_bytes();
        let n = bytes.len().min(N);
        out[..n].copy_from_slice(&bytes[..n]);
        FixedBytes(out)
    }

    /// The raw bytes.
    pub const fn as_array(&self) -> &[u8; N] {
        &self.0
    }

    /// The bytes up to the first NUL.
    pub fn as_slice(&self) -> &[u8] {
        match self.0.iter().position(|&b| b == 0) {
            Some(end) => &self.0[..end],
            None => &self.0[..],
        }
    }

    /// True when every byte is zero.
    pub fn is_empty(&self) -> bool {
        self.0.iter().all(|&b| b == 0)
    }
}

impl<const N: usize> Serialize for FixedBytes<N> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut tuple = serializer.serialize_tuple(N)?;
        for byte in &self.0 {
            tuple.serialize_element(byte)?;
        }
        tuple.end()
    }
}

impl<'de, const N: usize> Deserialize<'de> for FixedBytes<N> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct BytesVisitor<const N: usize>;

        impl<'de, const N: usize> Visitor<'de> for BytesVisitor<N> {
            type Value = FixedBytes<N>;

            fn expecting(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                write!(f, "{N} bytes")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut out = [0u8; N];
                for slot in out.iter_mut() {
                    *slot = seq
                        .next_element::<u8>()?
                        .ok_or_else(|| de::Error::invalid_length(N, &"a fixed byte array"))?;
                }
                Ok(FixedBytes(out))
            }
        }

        deserializer.deserialize_tuple(N, BytesVisitor::<N>)
    }
}

/// One tick of player intent, packed to 4 bytes (§7.1).
///
/// The axes are two 4-bit two's-complement nibbles rather than two `i8`s, which
/// is where §7.1's "30 B" for `C_INPUT` comes from: the table counted 8 bytes
/// per frame. There is no per-frame tick here. `Input.first_tick` already
/// implies every frame's tick, and shipping both would give a frame two sources
/// of truth for the tick it applies to — a client that sets `first_tick = 100`
/// and `frames[1].tick = 105` would be describing two different matches.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct WireInput {
    /// Packed stick axes: low nibble is strafe (right positive), high nibble is
    /// forward. Each nibble is a signed 4-bit value, `-8..=7`, and the
    /// simulation clamps it to `-1..=1`.
    pub axes: u8,
    /// Facing, in 1/256 of a turn.
    pub yaw: u8,
    /// Pitch, in 1/256 of a turn. 0 level, 64 up.
    pub pitch: u8,
    /// Bitfield of `sim::input::buttons`.
    pub buttons: u8,
}

/// Sign-extend the low four bits of `n`.
#[inline]
const fn sign4(n: u8) -> i8 {
    ((n << 4) as i8) >> 4
}

impl WireInput {
    /// Pack the two stick axes.
    #[inline]
    pub const fn pack_axes(move_x: i8, move_z: i8) -> u8 {
        ((move_z as u8 & 0x0F) << 4) | (move_x as u8 & 0x0F)
    }

    /// The strafe axis, `-8..=7`.
    #[inline]
    pub const fn move_x(self) -> i8 {
        sign4(self.axes & 0x0F)
    }

    /// The forward axis, `-8..=7`.
    #[inline]
    pub const fn move_z(self) -> i8 {
        sign4(self.axes >> 4)
    }

    /// Convert to the simulation's frame for `tick`.
    ///
    /// The axes are *not* clamped here: `sim::input::wish_direction` clamps
    /// them, and it is the only place that must, so clamping twice would be two
    /// places to disagree about what a malformed frame means.
    #[inline]
    pub const fn to_sim(self, tick: Tick) -> InputFrame {
        InputFrame {
            tick,
            move_x: self.move_x(),
            move_z: self.move_z(),
            yaw: self.yaw,
            pitch: self.pitch,
            buttons: self.buttons,
        }
    }

    /// Pack a simulation frame for the wire.
    #[inline]
    pub const fn from_sim(frame: &InputFrame) -> WireInput {
        WireInput {
            axes: Self::pack_axes(frame.move_x, frame.move_z),
            yaw: frame.yaw,
            pitch: frame.pitch,
            buttons: frame.buttons,
        }
    }
}

// ---------------------------------------------------------------------------
// Client → server (§7.1)
// ---------------------------------------------------------------------------

/// Join or rejoin a room. `token` present means reconnect.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Hello {
    /// The client's [`PROTOCOL_VERSION`].
    pub protocol: u16,
    /// Display name, NUL-padded.
    pub name: FixedBytes<NAME_LEN>,
    /// The room to join.
    pub room_code: FixedBytes<ROOM_CODE_LEN>,
    /// Reconnect token from a previous `S_WELCOME`.
    pub token: Option<FixedBytes<TOKEN_LEN>>,
}

/// Toggle the ready flag.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Ready {
    /// Ready to start.
    pub ready: bool,
}

/// Request a team in the lobby.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TeamChoice {
    /// The team to move to.
    pub team: u8,
}

/// Three ticks of input (§6.2: 15 Hz, three frames per packet).
///
/// # Why the snapshot acknowledgement rides here
///
/// §6.3 encodes each snapshot as a delta against "the last snapshot the client
/// **acknowledged** (`baseline_seq`)", and §6.3 also mentions a keyframe being
/// sent "immediately if the client NACKs a missing baseline". Neither an ack nor
/// a NACK appears in §7.1, so the mechanism §6.3 describes has nothing to carry
/// it.
///
/// It rides on `C_INPUT` rather than becoming a tenth message for two reasons.
/// A separate 20 Hz ack message plus the 15 Hz input stream is 35 messages a
/// second against §7.4's 30/s token bucket, so it would rate-limit the client
/// into a disconnect. And the input stream is exactly when the ack is useful:
/// every packet already proves the client is alive, so it may as well say how
/// far its world has caught up.
///
/// A missing baseline needs no NACK. The server keeps a bounded history of sent
/// snapshots; when the client's ack falls out of it, the next snapshot is a
/// keyframe. Losing a packet therefore self-heals in at most
/// `snapshot::BASELINE_HISTORY` ticks.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Input {
    /// Packet sequence, for the client's own acknowledgement bookkeeping.
    pub seq: u16,
    /// The newest snapshot sequence the client has received, or 0 for none.
    ///
    /// This is the delta baseline: the server encodes against it when it still
    /// has it, and sends a keyframe when it does not.
    pub acked_seq: u16,
    /// The tick `frames[0]` applies to.
    pub first_tick: Tick,
    /// Consecutive frames: `frames[i]` applies to `first_tick + i`.
    pub frames: [WireInput; INPUT_FRAMES],
}

/// A block edit (§6.6).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Edit {
    /// Client sequence, echoed in `S_EDIT_ACK`.
    pub seq: u16,
    /// Block position.
    pub x: i16,
    /// Block position.
    pub y: i16,
    /// Block position.
    pub z: i16,
    /// The block being placed; ignored for a break.
    pub block: u8,
    /// See [`edit_action`].
    pub action: u8,
}

/// A UX hint that the player thinks they are crafting here.
///
/// The server matches on its own (§4.5); this only lets it answer faster with a
/// reason instead of waiting for the next pad sweep.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct CraftIntent {
    /// Pad cell.
    pub x: i16,
    /// Pad cell.
    pub y: i16,
    /// Pad cell.
    pub z: i16,
}

/// A chat message.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Chat {
    /// Number of meaningful bytes in `text`.
    pub len: u8,
    /// The text, NUL-padded to [`CHAT_LEN`].
    ///
    /// A fixed array rather than a `Vec` so the decoder cannot be made to
    /// allocate an arbitrary buffer before sanitisation runs.
    pub text: FixedBytes<CHAT_LEN>,
}

impl Chat {
    /// The meaningful bytes of the message.
    #[inline]
    pub fn text(&self) -> &[u8] {
        &self.text.0[..(self.len as usize).min(CHAT_LEN)]
    }
}

/// RTT probe.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Ping {
    /// Client-chosen echo value.
    pub t: u32,
}

/// A desync report. Telemetry only; the server never acts on it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Report {
    /// Which check failed.
    pub kind: u8,
    /// The tick the client disagreed at.
    pub tick: Tick,
    /// The low 32 bits of the client's state hash.
    pub hash: u32,
}

// ---------------------------------------------------------------------------
// Server → client (§7.2)
// ---------------------------------------------------------------------------

/// The room's resolved configuration, sent once at join.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct RoomConfig {
    /// See [`room_mode`].
    pub mode: u8,
    /// Number of teams.
    pub teams: u8,
    /// Players per team.
    pub per_team: u8,
    /// Buddies allowed per team (§4.7).
    pub buddy_cap: u8,
    /// Nominal match length, informational.
    pub target_minutes: u8,
}

/// The host's lobby choices, which can still change.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct RoomSettings {
    /// See [`room_mode`].
    pub mode: u8,
    /// Number of teams.
    pub teams: u8,
    /// Player cap.
    pub max_players: u8,
}

/// The first message a client receives.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Welcome {
    /// The joining player's id.
    pub player_id: u8,
    /// The joining player's team.
    pub team: u8,
    /// The world seed. The client regenerates terrain from this (§6.7).
    pub seed: u32,
    /// The resolved room configuration.
    pub config: RoomConfig,
    /// The tick the room is at, so the client can align its clock.
    pub tick: Tick,
    /// The token to present in `C_HELLO` after a disconnect.
    pub your_token: FixedBytes<TOKEN_LEN>,
}

/// One row of the lobby list.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct LobbyPlayer {
    /// Player id.
    pub id: u8,
    /// Display name, NUL-padded.
    pub name: FixedBytes<NAME_LEN>,
    /// Current team.
    pub team: u8,
    /// Ready flag.
    pub ready: bool,
}

/// The lobby roster, sent whenever it changes.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Lobby {
    /// Everyone in the room, in join order.
    pub players: Vec<LobbyPlayer>,
    /// The host's settings.
    pub settings: RoomSettings,
}

/// One team's base position.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct BaseSpawn {
    /// Which team.
    pub team: u8,
    /// The core's origin.
    pub x: i16,
    /// The core's origin.
    pub y: i16,
    /// The core's origin.
    pub z: i16,
}

/// Match start.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Start {
    /// The first match tick.
    pub tick0: Tick,
    /// One row per team.
    pub bases: Vec<BaseSpawn>,
}

/// One player's delta (§7.3).
///
/// # Why the serde impls are hand-written
///
/// §7.3's square brackets are literal: `[pos_x][pos_y][pos_z][yaw][hp][flags][anim]`
/// means a field that is *absent from the byte stream* when its presence bit is
/// clear. Derived serde cannot express that — it writes every field of a struct
/// unconditionally — so a stationary entity would cost nine bytes instead of
/// three, and the 2 KB snapshot budget would be spent on the one thing deltas
/// exist to avoid sending.
///
/// postcard makes the conditional form possible without changing formats:
/// `serialize_struct` ignores the declared length, and `deserialize_struct`
/// hands the visitor a sequence accessor that only reads when asked. So the
/// length passed below is the *maximum* field count, and the visitor reads
/// exactly `2 + popcount(presence-expanded)` of them.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct SnapshotPlayer {
    /// Player id.
    pub id: u8,
    /// Which fields are present, see [`presence`].
    pub presence: u16,
    /// Position delta from the previous snapshot, fixed-point.
    pub x: i32,
    /// Position delta from the previous snapshot, fixed-point.
    pub y: i32,
    /// Position delta from the previous snapshot, fixed-point.
    pub z: i32,
    /// Facing.
    pub yaw: u8,
    /// Health.
    pub hp: u8,
    /// See [`player_flags`].
    pub flags: u8,
    /// Animation phase, 0..=255 over one cycle.
    pub anim: u8,
}

/// One buddy's delta (§7.3). Conditional encoding, same as
/// [`SnapshotPlayer`] and for the same reason.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct SnapshotBuddy {
    /// Buddy id.
    pub id: u8,
    /// Which fields are present, see [`presence`].
    pub presence: u16,
    /// Position delta, fixed-point.
    pub x: i32,
    /// Position delta, fixed-point.
    pub y: i32,
    /// Position delta, fixed-point.
    pub z: i32,
    /// Health.
    pub hp: u8,
}

/// One projectile (§7.3). Absolute, not delta: projectiles live for about half a
/// second, so a delta baseline would be established and discarded every time.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct SnapshotProjectile {
    /// Projectile id.
    pub id: u16,
    /// Position.
    pub x: i16,
    /// Position.
    pub y: i16,
    /// Position.
    pub z: i16,
    /// Velocity.
    pub vx: i8,
    /// Velocity.
    pub vy: i8,
    /// Velocity.
    pub vz: i8,
}

/// One siege structure (§7.3).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct SnapshotStructure {
    /// Structure id.
    pub id: u8,
    /// Health as a percentage, 0..=100.
    pub hp: u8,
    /// Position.
    pub x: i16,
    /// Position.
    pub y: i16,
    /// Position.
    pub z: i16,
}

/// One event inside a snapshot (§7.3). Never culled (§6.3): events are the
/// game's information layer.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct SnapshotEvent {
    /// See [`event_kind`].
    pub kind: u8,
    /// First operand.
    pub a: i32,
    /// Second operand.
    pub b: i32,
    /// Third operand.
    pub c: i32,
}

/// A snapshot (§7.3).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    /// The tick this snapshot describes.
    pub tick: Tick,
    /// The client's last acknowledged snapshot, or 0 for a keyframe.
    pub baseline_seq: u16,
    /// True when every field of every entity is present.
    pub keyframe: bool,
    /// Players in range.
    pub players: Vec<SnapshotPlayer>,
    /// Buddies in range.
    pub buddies: Vec<SnapshotBuddy>,
    /// Projectiles in range.
    pub projectiles: Vec<SnapshotProjectile>,
    /// Structures in range.
    pub structures: Vec<SnapshotStructure>,
    /// Events this tick.
    pub events: Vec<SnapshotEvent>,
}

/// The maximum number of fields a [`SnapshotPlayer`] can put on the wire.
const PLAYER_FIELDS: &[&str] = &[
    "id", "presence", "x", "y", "z", "yaw", "hp", "flags", "anim",
];

/// The maximum number of fields a [`SnapshotBuddy`] can put on the wire.
const BUDDY_FIELDS: &[&str] = &["id", "presence", "x", "y", "z", "hp"];

impl Serialize for SnapshotPlayer {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut out = serializer.serialize_struct("SnapshotPlayer", PLAYER_FIELDS.len())?;
        out.serialize_field("id", &self.id)?;
        out.serialize_field("presence", &self.presence)?;
        // `presence::POS` covers all three axes: a position is one decision, and
        // splitting it into three bits would let a sender claim two of them.
        if self.presence & presence::POS != 0 {
            out.serialize_field("x", &self.x)?;
            out.serialize_field("y", &self.y)?;
            out.serialize_field("z", &self.z)?;
        }
        if self.presence & presence::YAW != 0 {
            out.serialize_field("yaw", &self.yaw)?;
        }
        if self.presence & presence::HP != 0 {
            out.serialize_field("hp", &self.hp)?;
        }
        if self.presence & presence::FLAGS != 0 {
            out.serialize_field("flags", &self.flags)?;
        }
        if self.presence & presence::ANIM != 0 {
            out.serialize_field("anim", &self.anim)?;
        }
        out.end()
    }
}

impl<'de> Deserialize<'de> for SnapshotPlayer {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Fields;
        impl<'de> Visitor<'de> for Fields {
            type Value = SnapshotPlayer;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a snapshot player delta")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let short = || de::Error::invalid_length(0, &"a snapshot player delta");
                let mut out = SnapshotPlayer {
                    id: seq.next_element()?.ok_or_else(short)?,
                    presence: seq.next_element()?.ok_or_else(short)?,
                    ..Default::default()
                };
                if out.presence & presence::POS != 0 {
                    out.x = seq.next_element()?.ok_or_else(short)?;
                    out.y = seq.next_element()?.ok_or_else(short)?;
                    out.z = seq.next_element()?.ok_or_else(short)?;
                }
                if out.presence & presence::YAW != 0 {
                    out.yaw = seq.next_element()?.ok_or_else(short)?;
                }
                if out.presence & presence::HP != 0 {
                    out.hp = seq.next_element()?.ok_or_else(short)?;
                }
                if out.presence & presence::FLAGS != 0 {
                    out.flags = seq.next_element()?.ok_or_else(short)?;
                }
                if out.presence & presence::ANIM != 0 {
                    out.anim = seq.next_element()?.ok_or_else(short)?;
                }
                Ok(out)
            }
        }
        deserializer.deserialize_struct("SnapshotPlayer", PLAYER_FIELDS, Fields)
    }
}

impl Serialize for SnapshotBuddy {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut out = serializer.serialize_struct("SnapshotBuddy", BUDDY_FIELDS.len())?;
        out.serialize_field("id", &self.id)?;
        out.serialize_field("presence", &self.presence)?;
        if self.presence & presence::POS != 0 {
            out.serialize_field("x", &self.x)?;
            out.serialize_field("y", &self.y)?;
            out.serialize_field("z", &self.z)?;
        }
        if self.presence & presence::HP != 0 {
            out.serialize_field("hp", &self.hp)?;
        }
        out.end()
    }
}

impl<'de> Deserialize<'de> for SnapshotBuddy {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Fields;
        impl<'de> Visitor<'de> for Fields {
            type Value = SnapshotBuddy;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a snapshot buddy delta")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let short = || de::Error::invalid_length(0, &"a snapshot buddy delta");
                let mut out = SnapshotBuddy {
                    id: seq.next_element()?.ok_or_else(short)?,
                    presence: seq.next_element()?.ok_or_else(short)?,
                    ..Default::default()
                };
                if out.presence & presence::POS != 0 {
                    out.x = seq.next_element()?.ok_or_else(short)?;
                    out.y = seq.next_element()?.ok_or_else(short)?;
                    out.z = seq.next_element()?.ok_or_else(short)?;
                }
                if out.presence & presence::HP != 0 {
                    out.hp = seq.next_element()?.ok_or_else(short)?;
                }
                Ok(out)
            }
        }
        deserializer.deserialize_struct("SnapshotBuddy", BUDDY_FIELDS, Fields)
    }
}

/// The answer to one `C_EDIT` (§6.6).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct EditAck {
    /// The client's sequence number.
    pub seq: u16,
    /// Whether the edit was applied.
    pub accepted: bool,
    /// See [`edit_reject`]; `NONE` when accepted.
    pub reason: u8,
}

/// A broadcast of one applied edit (§6.6).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct EditBroadcast {
    /// Block position.
    pub x: i16,
    /// Block position.
    pub y: i16,
    /// Block position.
    pub z: i16,
    /// The new block id, or 0 for "empty".
    pub block: u8,
    /// Monotonic per room; clients apply only if newer than what they hold.
    pub server_seq: u32,
}

/// One entry in a chunk diff (§6.7).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct DiffEntry {
    /// Local cell index, packed as `x + 16 * (z + 16 * y)` for a 16×96×16
    /// column.
    ///
    /// That is `world::Column::index`, which is the only thing that ever
    /// produces these entries — X varies fastest so the mesher and the diff
    /// encoder both walk contiguous memory. An earlier version of this comment
    /// said `x | y << 4 | z << 12`, which is a different permutation and would
    /// have decoded every crate to the wrong cell; the encoder is the authority,
    /// and this comment now matches it.
    pub index: u16,
    /// The new block id.
    pub block: u8,
}

/// The cells of one column that differ from generated terrain (§6.7).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ChunkDiff {
    /// Column coordinate.
    pub cx: i16,
    /// Column coordinate.
    pub cz: i16,
    /// The server's edit count for this column.
    pub version: u32,
    /// The changed cells, at most [`MAX_DIFF_ENTRIES`].
    pub entries: Vec<DiffEntry>,
}

/// A standalone event, for the cases where an event must not wait for a
/// snapshot (or arrive inside one).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Event {
    /// See [`event_kind`].
    pub kind: u8,
    /// First operand.
    pub a: i32,
    /// Second operand.
    pub b: i32,
    /// Third operand.
    pub c: i32,
}

/// One player's end-of-match line.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct PlayerStats {
    /// Player id.
    pub id: u8,
    /// Buddies and players killed.
    pub kills: u16,
    /// Times this player died.
    pub deaths: u16,
    /// Effective damage dealt to cores.
    pub damage: u32,
    /// Crates carried and deposited.
    pub crates: u16,
}

/// The results screen payload.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct End {
    /// The winning team, or `-1` for no human winner (D-05).
    pub winner_team: i8,
    /// See [`end_reason`].
    pub reason: u8,
    /// One row per player.
    pub stats: Vec<PlayerStats>,
}

/// A recoverable or fatal error.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ErrorMsg {
    /// See [`error_code`].
    pub code: u8,
    /// Human-readable detail, NUL-padded.
    pub detail: FixedBytes<ERROR_DETAIL_LEN>,
}

/// The answer to `C_PING`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Pong {
    /// The value from the `C_PING` being answered.
    pub t: u32,
}

// ---------------------------------------------------------------------------
// The enums
// ---------------------------------------------------------------------------

/// Why a frame could not be decoded.
///
/// This is deliberately short. It answers "can I turn these bytes into a
/// message", not "should I accept this message" — the second question belongs to
/// `codec`, and it has its own error type.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DecodeError {
    /// The frame was zero bytes long.
    Empty,
    /// The first byte is not a tag this direction defines.
    UnknownTag(u8),
    /// The payload did not match the tag's type.
    Payload(postcard::Error),
}

impl core::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            DecodeError::Empty => write!(f, "empty frame"),
            DecodeError::UnknownTag(tag) => write!(f, "unknown tag {tag:#04x}"),
            DecodeError::Payload(e) => write!(f, "bad payload: {e}"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Writes bytes into a `Vec` without a per-message allocation.
///
/// `postcard::to_extend` wants `Extend<u8>` and `&mut Vec<u8>` does not
/// implement it, so this is the adapter. Encoding a snapshot at 20 Hz per client
/// is 120 calls a second; `to_allocvec` plus `extend_from_slice` would double the
/// allocations of the hottest path in the server for no reason.
struct Sink<'a>(&'a mut Vec<u8>);

impl Extend<u8> for Sink<'_> {
    fn extend<I: IntoIterator<Item = u8>>(&mut self, iter: I) {
        self.0.extend(iter);
    }
}

/// Generate a message enum plus its tag table and codec.
///
/// The macro exists so that the tag, the variant and the payload type are
/// declared *once*. Adding a variant therefore cannot forget a tag, cannot
/// forget a `decode` arm (the match is generated), and cannot forget a
/// roundtrip sample (the test iterates [`ALL_TAGS`] and fails on any tag it has
/// no sample for).
macro_rules! wire_enum {
    (
        $(#[$meta:meta])*
        $name:ident { $($variant:ident = $tag:literal => $payload:ty),* $(,)? }
    ) => {
        $(#[$meta])*
        #[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
        pub enum $name {
            $(
                #[doc = concat!("Tag `", stringify!($tag), "`.")]
                $variant($payload),
            )*
        }

        impl $name {
            /// Every tag this direction defines, in declaration order.
            pub const ALL_TAGS: &'static [u8] = &[$($tag),*];

            /// The wire tag for this message.
            pub const fn tag(&self) -> u8 {
                match self {
                    $( Self::$variant(_) => $tag, )*
                }
            }

            /// Append `tag || postcard(payload)` to `out`.
            pub fn encode_into(&self, out: &mut Vec<u8>) {
                out.push(self.tag());
                match self {
                    $(
                        Self::$variant(payload) => {
                            postcard::to_extend(payload, Sink(out))
                                .expect("serialising into a Vec cannot fail");
                        }
                    )*
                }
            }

            /// Encode to a fresh buffer.
            pub fn encode(&self) -> Vec<u8> {
                let mut out = Vec::with_capacity(64);
                self.encode_into(&mut out);
                out
            }

            /// Decode a frame: the first byte is the tag, the rest is the
            /// payload.
            pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
                let (tag, body) = bytes.split_first().ok_or(DecodeError::Empty)?;
                let message = match *tag {
                    $(
                        $tag => Self::$variant(
                            postcard::from_bytes(body).map_err(DecodeError::Payload)?,
                        ),
                    )*
                    other => return Err(DecodeError::UnknownTag(other)),
                };
                Ok(message)
            }
        }
    };
}

wire_enum! {
    /// Everything a client may send (§7.1).
    C2s {
        Hello = 0x01 => Hello,
        Ready = 0x02 => Ready,
        TeamChoice = 0x03 => TeamChoice,
        Input = 0x04 => Input,
        Edit = 0x05 => Edit,
        CraftIntent = 0x06 => CraftIntent,
        Chat = 0x07 => Chat,
        Ping = 0x08 => Ping,
        Report = 0x09 => Report,
    }
}

wire_enum! {
    /// Everything a server may send (§7.2).
    S2c {
        Welcome = 0x81 => Welcome,
        Lobby = 0x82 => Lobby,
        Start = 0x83 => Start,
        Snapshot = 0x84 => Snapshot,
        EditAck = 0x85 => EditAck,
        EditBroadcast = 0x86 => EditBroadcast,
        ChunkDiff = 0x87 => ChunkDiff,
        Event = 0x88 => Event,
        End = 0x89 => End,
        Error = 0x8A => ErrorMsg,
        Pong = 0x8B => Pong,
    }
}

/// Encode a client message.
pub fn encode_c2s(message: &C2s) -> Vec<u8> {
    message.encode()
}

/// Encode a server message.
pub fn encode_s2c(message: &S2c) -> Vec<u8> {
    message.encode()
}

/// Decode a client message.
pub fn decode_c2s(bytes: &[u8]) -> Result<C2s, DecodeError> {
    C2s::decode(bytes)
}

/// Decode a server message.
pub fn decode_s2c(bytes: &[u8]) -> Result<S2c, DecodeError> {
    S2c::decode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- worst cases --------------------------------------------------------
    //
    // One message per tag, at the largest field values the protocol can carry.
    // The same list serves two purposes: `roundtrip_all` checks the codec at its
    // bounds (where an off-by-one in a varint would show up) and
    // `sizes_match_spec` checks the encoded size against §7. Testing the extremes
    // for both is not laziness — a roundtrip at `seq = 77` proves less than a
    // roundtrip at `seq = 65535`.

    fn c2s_worst_cases() -> Vec<C2s> {
        let chat = Chat {
            len: CHAT_LEN as u8,
            text: FixedBytes([b'x'; CHAT_LEN]),
        };

        vec![
            C2s::Hello(Hello {
                protocol: PROTOCOL_VERSION,
                name: FixedBytes([b'x'; NAME_LEN]),
                room_code: FixedBytes([b'Z'; ROOM_CODE_LEN]),
                // The reconnect form is the larger one; the fresh-join form is
                // 16 bytes shorter and is covered by `hello_roundtrips_without_a_token`.
                token: Some(FixedBytes([0xFF; TOKEN_LEN])),
            }),
            C2s::Ready(Ready { ready: true }),
            C2s::TeamChoice(TeamChoice { team: 5 }),
            C2s::Input(Input {
                seq: u16::MAX,
                acked_seq: u16::MAX,
                first_tick: Tick(56_400),
                frames: [WireInput {
                    axes: 0xFF,
                    yaw: 255,
                    pitch: 255,
                    buttons: 0xFF,
                }; INPUT_FRAMES],
            }),
            C2s::Edit(Edit {
                seq: u16::MAX,
                x: -128,
                y: 128,
                z: -128,
                block: 255,
                action: edit_action::PLACE,
            }),
            C2s::CraftIntent(CraftIntent {
                x: -128,
                y: 128,
                z: -128,
            }),
            C2s::Chat(chat),
            C2s::Ping(Ping { t: 2_800_000 }),
            C2s::Report(Report {
                kind: 1,
                tick: Tick(56_400),
                hash: u32::MAX,
            }),
        ]
    }

    fn s2c_worst_cases() -> Vec<S2c> {
        vec![
            S2c::Welcome(Welcome {
                player_id: MAX_PLAYERS as u8 - 1,
                team: MAX_TEAMS as u8 - 1,
                seed: u32::MAX,
                config: RoomConfig {
                    mode: room_mode::TEAMS,
                    teams: MAX_TEAMS as u8,
                    per_team: MAX_PLAYERS as u8,
                    buddy_cap: 5,
                    target_minutes: 47,
                },
                tick: Tick(56_400),
                your_token: FixedBytes([0xFF; TOKEN_LEN]),
            }),
            S2c::Lobby(Lobby {
                players: (0..MAX_PLAYERS as u8)
                    .map(|id| LobbyPlayer {
                        id,
                        name: FixedBytes([b'x'; NAME_LEN]),
                        team: MAX_TEAMS as u8 - 1,
                        ready: true,
                    })
                    .collect(),
                settings: RoomSettings {
                    mode: room_mode::TEAMS,
                    teams: MAX_TEAMS as u8,
                    max_players: MAX_PLAYERS as u8,
                },
            }),
            S2c::Start(Start {
                tick0: Tick(56_400),
                bases: (0..MAX_BASES as u8)
                    .map(|team| BaseSpawn {
                        team,
                        x: -128,
                        y: 128,
                        z: -128,
                    })
                    .collect(),
            }),
            S2c::Snapshot(snapshot_worst_case()),
            // The server echoes the client's own edit counter, and §6.6 allows
            // 8 edits/s: a 47-minute match reaches 22,560, past the 16,383 that
            // a two-byte varint covers.
            S2c::EditAck(EditAck {
                seq: u16::MAX,
                accepted: false,
                reason: edit_reject::UNKNOWN_BLOCK,
            }),
            S2c::EditBroadcast(EditBroadcast {
                x: -128,
                y: 128,
                z: -128,
                block: 255,
                server_seq: u32::MAX,
            }),
            S2c::ChunkDiff(ChunkDiff {
                cx: -128,
                cz: 128,
                version: u32::MAX,
                entries: (0..MAX_DIFF_ENTRIES)
                    .map(|_| DiffEntry {
                        // The largest index a 16×96×16 column can produce.
                        index: 1_535,
                        block: 19,
                    })
                    .collect(),
            }),
            S2c::Event(Event {
                kind: event_kind::AIRDROP,
                a: 10_000,
                b: 10_000,
                c: 10_000,
            }),
            S2c::End(End {
                // D-05: no human winner.
                winner_team: -1,
                reason: end_reason::DRAW,
                stats: (0..MAX_PLAYERS as u8)
                    .map(|id| PlayerStats {
                        id,
                        kills: 1_000,
                        deaths: 1_000,
                        damage: 100_000,
                        crates: 1_000,
                    })
                    .collect(),
            }),
            S2c::Error(ErrorMsg {
                code: error_code::INTERNAL,
                detail: FixedBytes([b'x'; ERROR_DETAIL_LEN]),
            }),
            S2c::Pong(Pong { t: 2_800_000 }),
        ]
    }

    /// The worst snapshot §7.3 budgets for: 6 players, 12 buddies, 24
    /// projectiles, 4 structures, events — all moving, all fields present.
    fn snapshot_worst_case() -> Snapshot {
        Snapshot {
            tick: Tick(56_400),
            baseline_seq: 65_535,
            keyframe: true,
            players: (0..MAX_PLAYERS as u8)
                .map(|id| SnapshotPlayer {
                    id,
                    presence: presence::ALL,
                    // One tick of movement at 9 m/s is 0.45 m = 29,491 in Q16.16.
                    x: 29_491,
                    y: -29_491,
                    z: 29_491,
                    yaw: 255,
                    hp: 100,
                    flags: player_flags::ALIVE | player_flags::ATTACKING,
                    anim: 255,
                })
                .collect(),
            buddies: (0..MAX_BUDDIES as u8)
                .map(|id| SnapshotBuddy {
                    id,
                    presence: presence::ALL,
                    x: 29_491,
                    y: -29_491,
                    z: 29_491,
                    hp: 100,
                })
                .collect(),
            projectiles: (0..MAX_PROJECTILES as u16)
                .map(|id| SnapshotProjectile {
                    id,
                    x: 2_000,
                    y: -2_000,
                    z: 2_000,
                    vx: -40,
                    vy: 12,
                    vz: 40,
                })
                .collect(),
            structures: (0..MAX_STRUCTURES as u8)
                .map(|id| SnapshotStructure {
                    id,
                    hp: 100,
                    x: 2_000,
                    y: -2_000,
                    z: 2_000,
                })
                .collect(),
            events: (0..MAX_EVENTS)
                .map(|_| SnapshotEvent {
                    kind: event_kind::BASE_HIT,
                    a: 1,
                    b: 8_400,
                    c: 4,
                })
                .collect(),
        }
    }

    // -- roundtrip ----------------------------------------------------------

    #[test]
    fn roundtrip_all() {
        for message in c2s_worst_cases() {
            let bytes = message.encode();
            assert_eq!(bytes[0], message.tag(), "first byte must be the tag");
            let back = C2s::decode(&bytes).expect("decode");
            assert_eq!(
                back,
                message,
                "C2S roundtrip for tag {:#04x}",
                message.tag()
            );
        }
        for message in s2c_worst_cases() {
            let bytes = message.encode();
            assert_eq!(bytes[0], message.tag(), "first byte must be the tag");
            let back = S2c::decode(&bytes).expect("decode");
            assert_eq!(
                back,
                message,
                "S2C roundtrip for tag {:#04x}",
                message.tag()
            );
        }
    }

    #[test]
    fn hello_roundtrips_without_a_token() {
        // The fresh-join form: `Option::None` costs one byte, `Some` costs
        // seventeen. Both have to survive the codec, and the worst-case list
        // only covers the larger one.
        let hello = C2s::Hello(Hello {
            protocol: PROTOCOL_VERSION,
            name: FixedBytes::from_str_lossy("ivan"),
            room_code: FixedBytes(*b"AB12CD"),
            token: None,
        });
        let bytes = hello.encode();
        assert_eq!(bytes.len(), 25);
        assert_eq!(C2s::decode(&bytes), Ok(hello));
    }

    #[test]
    fn fixed_bytes_costs_exactly_its_length() {
        // No length prefix: 120 bytes of chat is 120 bytes, not 121. This is the
        // property that lets `C_CHAT`'s budget be stated as a constant rather
        // than a function of the message.
        assert_eq!(
            postcard::to_allocvec(&FixedBytes::<120>::default())
                .unwrap()
                .len(),
            120
        );
        assert_eq!(
            postcard::to_allocvec(&FixedBytes::<64>::default())
                .unwrap()
                .len(),
            64
        );
        assert_eq!(
            postcard::to_allocvec(&FixedBytes::<16>::default())
                .unwrap()
                .len(),
            16
        );
        assert_eq!(
            postcard::to_allocvec(&FixedBytes::<6>::default())
                .unwrap()
                .len(),
            6
        );
    }

    #[test]
    fn every_tag_has_a_sample() {
        let c2s: Vec<u8> = c2s_worst_cases().iter().map(|m| m.tag()).collect();
        assert_eq!(
            c2s,
            C2s::ALL_TAGS,
            "the C2S sample list must cover every tag, in order"
        );
        let s2c: Vec<u8> = s2c_worst_cases().iter().map(|m| m.tag()).collect();
        assert_eq!(
            s2c,
            S2c::ALL_TAGS,
            "the S2C sample list must cover every tag, in order"
        );
    }

    #[test]
    fn tags_are_unique_and_directions_do_not_collide() {
        let mut all: Vec<u8> = C2s::ALL_TAGS.to_vec();
        all.extend_from_slice(S2c::ALL_TAGS);
        let mut sorted = all.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), all.len(), "duplicate tag in {all:02x?}");

        // The high bit separates the directions, so a frame's direction is
        // readable without knowing which side sent it — useful in a log, a proxy
        // and a fuzz corpus.
        for tag in C2s::ALL_TAGS {
            assert_eq!(
                tag & 0x80,
                0,
                "C2S tag {tag:#04x} must have the high bit clear"
            );
        }
        for tag in S2c::ALL_TAGS {
            assert_eq!(
                tag & 0x80,
                0x80,
                "S2C tag {tag:#04x} must have the high bit set"
            );
        }
    }

    #[test]
    fn decode_rejects_bad_frames() {
        assert_eq!(C2s::decode(&[]), Err(DecodeError::Empty));
        assert_eq!(C2s::decode(&[0x00]), Err(DecodeError::UnknownTag(0x00)));
        assert_eq!(C2s::decode(&[0x42]), Err(DecodeError::UnknownTag(0x42)));
        // A server tag is not a client tag.
        assert_eq!(C2s::decode(&[0x84]), Err(DecodeError::UnknownTag(0x84)));
        // A known tag with a truncated payload is a payload error, not a panic.
        assert!(matches!(
            C2s::decode(&[0x04, 0x01]),
            Err(DecodeError::Payload(_))
        ));
    }

    // -- sizes --------------------------------------------------------------

    /// §7.1/§7.2 sizes, keyed by tag.
    ///
    /// These are treated as **ceilings**, not as estimates to be matched. A
    /// message that encodes smaller than its table entry is strictly better on a
    /// metered free tier, so failing the build for being too small would be
    /// backwards; failing it for growing past the ceiling is the property worth
    /// having. Three entries are overridden in [`budget`] because the table's
    /// own arithmetic does not close — see [`spec_size_is_impossible_for`].
    const SPEC_SIZES: [(u8, u16); 20] = [
        // §7.1
        (0x01, 44),  // C_HELLO, with the reconnect token
        (0x02, 2),   // C_READY
        (0x03, 2),   // C_TEAM
        (0x04, 30),  // C_INPUT
        (0x05, 14),  // C_EDIT
        (0x06, 7),   // C_CRAFT_INTENT
        (0x07, 122), // C_CHAT
        (0x08, 5),   // C_PING
        (0x09, 7),   // C_REPORT
        // §7.2
        (0x81, 120),  // S_WELCOME
        (0x82, 140),  // S_LOBBY
        (0x83, 40),   // S_START
        (0x84, 2048), // S_SNAPSHOT
        (0x85, 5),    // S_EDIT_ACK
        (0x86, 14),   // S_EDIT
        (0x87, 1536), // S_CHUNK_DIFF
        (0x88, 14),   // S_EVENT
        (0x89, 200),  // S_END
        (0x8A, 66),   // S_ERROR
        (0x8B, 5),    // S_PONG
    ];

    /// The budget for a tag: the spec size, or the corrected size where the
    /// spec's number is arithmetically impossible.
    fn budget(tag: u8) -> u16 {
        match tag {
            // §7.1 says 7 B for `kind u8, tick u32, hash u32`. Even fixed-width
            // that is 1 (tag) + 1 + 4 + 4 = 10; 7 matches a `u16` hash. The
            // hash stays u32 — a 16-bit desync hash collides once per 65,536
            // reports, which is not enough resolution to bisect a desync with.
            0x09 => 10,
            // §7.2 says ~40 B for `tick0 u32` plus one `(u8, i16, i16, i16)`
            // per team. Six teams is the §4.7 maximum (FFA, six players):
            // 1 (tag) + 3 (tick0) + 1 (count) + 6 × 7 = 47.
            0x83 => 48,
            // §7.2 says 5 B, which holds while `seq` fits in two varint bytes
            // (`seq < 16,384`). §6.6 allows 8 edits/s and a match can run 47
            // minutes, so `seq` reaches 22,560 and the field needs three bytes:
            // 1 (tag) + 3 + 1 (accepted) + 1 (reason) = 6.
            0x85 => 6,
            other => SPEC_SIZES
                .iter()
                .find(|(t, _)| *t == other)
                .map(|(_, size)| *size)
                .expect("every tag needs a budget"),
        }
    }

    /// Whether the spec's number for this tag is smaller than the payload can
    /// possibly be.
    ///
    /// This is what makes the two overrides corrections rather than excuses: if
    /// a future change brings the payload back inside the spec's number (a
    /// `u16` hash, fewer teams), this returns `false` and the test tells you to
    /// delete the override.
    fn spec_size_is_impossible_for(tag: u8) -> bool {
        let spec = SPEC_SIZES
            .iter()
            .find(|(t, _)| *t == tag)
            .map(|(_, size)| *size)
            .expect("tag has a spec size");
        match tag {
            // 1 tag + 1 kind + 4 tick + 4 hash, all fixed width.
            0x09 => 10 > spec,
            // 1 tag + 4 tick0 + 1 count + 6 × (1 id + 3 × 2 coord).
            0x83 => 46 > spec,
            // 1 tag + 3 seq (u16 max) + 1 accepted + 1 reason.
            0x85 => 6 > spec,
            _ => false,
        }
    }

    #[test]
    fn sizes_match_spec() {
        let mut failures = Vec::new();
        let mut measured = Vec::new();

        let messages = c2s_worst_cases()
            .into_iter()
            .map(|m| (m.tag(), m.encode().len()))
            .chain(
                s2c_worst_cases()
                    .into_iter()
                    .map(|m| (m.tag(), m.encode().len())),
            );

        for (tag, size) in messages {
            let limit = budget(tag) as usize;
            measured.push((tag, size, limit));
            if size > limit {
                failures.push(format!(
                    "tag {tag:#04x} encodes to {size} B, budget {limit} B"
                ));
            }
        }

        assert!(
            failures.is_empty(),
            "{} message(s) exceeded their §7 budget:\n{}\n\nall measurements:\n{}",
            failures.len(),
            failures.join("\n"),
            measured
                .iter()
                .map(|(tag, size, limit)| format!("  {tag:#04x}: {size:>5} B / {limit:>5} B"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    #[test]
    fn overridden_sizes_are_provably_impossible() {
        assert!(
            spec_size_is_impossible_for(0x09),
            "C_REPORT fits in the spec's 7 B again — delete the 0x09 override"
        );
        assert!(
            spec_size_is_impossible_for(0x83),
            "S_START fits in the spec's 40 B again — delete the 0x83 override"
        );
    }

    #[test]
    fn no_message_can_exceed_the_frame_cap() {
        // The cap is on inbound frames, but an outbound message larger than it
        // would be a message the server can produce and the client's own
        // validator would then reject.
        for (tag, size) in c2s_worst_cases()
            .into_iter()
            .map(|m| (m.tag(), m.encode().len()))
            .chain(
                s2c_worst_cases()
                    .into_iter()
                    .map(|m| (m.tag(), m.encode().len())),
            )
        {
            assert!(
                size <= MAX_FRAME_BYTES,
                "tag {tag:#04x} encodes to {size} B, over the {MAX_FRAME_BYTES} B frame cap"
            );
        }
    }

    #[test]
    fn snapshot_stays_inside_its_budget() {
        let size = S2c::Snapshot(snapshot_worst_case()).encode().len();
        // §6.8 budgets ~550 B average; the keyframe is the largest case, so if
        // it fits in 2 KB the average has room.
        assert!(
            size <= 2048,
            "worst-case snapshot is {size} B, over the 2048 B budget"
        );
        // Not merely under the ceiling: comfortably under the *average*, which
        // is what makes a keyframe every 60 ticks affordable.
        assert!(
            size <= 1024,
            "worst-case snapshot is {size} B; a keyframe every 3 s should cost well under 1 KB"
        );
    }

    // -- input frame --------------------------------------------------------

    #[test]
    fn wire_input_roundtrips_the_axes_it_can_represent() {
        for (mx, mz) in [
            (0i8, 0i8),
            (1, 0),
            (-1, 0),
            (0, 1),
            (0, -1),
            (1, 1),
            (-1, -1),
            (-8, 7),
            (7, -8),
        ] {
            let wire = WireInput {
                axes: WireInput::pack_axes(mx, mz),
                yaw: 0,
                pitch: 0,
                buttons: 0,
            };
            assert_eq!(wire.move_x(), mx, "strafe {mx} did not survive packing");
            assert_eq!(wire.move_z(), mz, "forward {mz} did not survive packing");
        }
    }

    #[test]
    fn wire_input_is_four_bytes() {
        // §7.1's "30 B" for C_INPUT assumes 8 B per frame. This is the actual
        // cost of a frame, and the reason the message comes in well under.
        let size = postcard::to_allocvec(&WireInput::default())
            .expect("encode")
            .len();
        assert_eq!(size, 4);
    }

    #[test]
    fn sim_input_roundtrips_through_the_wire() {
        let frame = InputFrame {
            tick: Tick(900),
            move_x: -1,
            move_z: 1,
            yaw: 128,
            pitch: 192,
            buttons: sim::input::buttons::FIRE | sim::input::buttons::STANCE,
        };
        let back = WireInput::from_sim(&frame).to_sim(frame.tick);
        assert_eq!(back, frame);
    }

    #[test]
    fn chat_text_is_bounded_by_its_length_byte() {
        let mut chat = Chat {
            len: 3,
            text: FixedBytes([b'x'; CHAT_LEN]),
        };
        assert_eq!(chat.text(), b"xxx");
        // A length byte past the array cannot read out of bounds.
        chat.len = 255;
        assert_eq!(chat.text().len(), CHAT_LEN);
    }
}
