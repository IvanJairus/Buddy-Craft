//! Per-client snapshot encoding, interest management and delta baselines
//! (plan §6.3, §7.3; task T-021).
//!
//! # What this module is
//!
//! One [`SnapshotEncoder`] per connected client. Every tick the room hands it
//! the match state, and it produces the bytes of one `S_SNAPSHOT` frame for
//! that client alone: which entities that client is allowed to know about, and
//! which of their fields actually changed since the last frame the client
//! acknowledged.
//!
//! # Why the baseline is the *acknowledged* frame
//!
//! §6.3 says each snapshot is encoded against "the last snapshot the client
//! **acknowledged**", not the last one sent. That distinction is the whole
//! packet-loss story. If the server delta'd against the last frame it *sent*
//! and that frame was dropped, the client would apply a delta to a state it
//! never had, and the error would persist until the next keyframe — three
//! seconds of a player standing in the wrong place. Encoding against the acked
//! frame means a dropped snapshot costs exactly one frame: the next one is a
//! delta against a baseline the client provably has.
//!
//! The cost is that the server must keep the frames it sent, keyed by sequence.
//! [`BASELINE_HISTORY`] of them is 64 frames — one more than
//! [`KEYFRAME_INTERVAL`], so an ack that is exactly one keyframe old still
//! resolves. Older than that and the server sends a keyframe instead of
//! guessing, which is the self-healing path §6.3 calls "immediately if the
//! client NACKs a missing baseline". There is no NACK message on the wire: a
//! baseline that fell out of the history *is* the NACK.
//!
//! # Why the acknowledgement rides on `C_INPUT`
//!
//! `protocol::Input::acked_seq`. A separate 20 Hz ack message plus the 15 Hz
//! input stream is 35 messages a second against §7.4's 30/s token bucket, so
//! the client would rate-limit itself into a disconnect. See the doc comment on
//! that field for the full reasoning.
//!
//! # Allocation
//!
//! `encode` allocates a handful of small `Vec`s per call — 20 Hz × 6 clients,
//! a few hundred bytes each. That is deliberate and it is *not* the client's
//! zero-allocation rule (§8.4 `cb_tick`): this code runs on the server, where a
//! reusable scratch buffer would save microseconds and cost a class of
//! aliasing bugs. Revisit only if a profile says the allocator is hot.

use std::collections::VecDeque;

use protocol::{
    player_flags, presence, S2c, Snapshot, SnapshotBuddy, SnapshotEvent, SnapshotPlayer,
    SnapshotProjectile, SnapshotStructure, MAX_BUDDIES, MAX_EVENTS, MAX_PLAYERS, MAX_PROJECTILES,
    MAX_STRUCTURES,
};
use sim::{Fixed, MatchState, PlayerId, Projectile, Tick, Vec3};

/// Interest radius, horizontally (§6.3): 48 m, which is the 96 m render
/// diameter the client can actually see.
pub const AOI_RADIUS: Fixed = Fixed::from_int(48);

/// The band beyond [`AOI_RADIUS`] in which an entity that was already visible
/// stays visible (§6.3: "4 m hysteresis").
///
/// Without it, an entity oscillating around 48 m would be included, excluded,
/// included — a visible pop-in/pop-out at exactly the distance where a buddy is
/// most likely to be fighting someone.
pub const AOI_HYSTERESIS: Fixed = Fixed::from_int(4);

/// Ticks between keyframes (§6.2: every 3 s at 20 Hz).
pub const KEYFRAME_INTERVAL: u16 = 60;

/// Sent frames kept per client, so an acknowledgement can still be resolved.
///
/// Must exceed [`KEYFRAME_INTERVAL`]: a client that acks the newest keyframe
/// after losing every delta in between is still inside this window.
pub const BASELINE_HISTORY: usize = 64;

/// Horizontal speed above which a body is considered to be walking, for the
/// animation phase. 0.1 m/s — well under the 4.5 m/s movement cap (§4.11) and
/// well over the residue a collision slide leaves behind.
const MOVING_SPEED: Fixed = Fixed::from_raw(6_554);

/// Ticks in one walk cycle, and therefore how much the phase advances per tick
/// (`256 / WALK_CYCLE`).
///
/// 16 ticks at 20 Hz is 0.8 s per stride — a jog. The phase is a function of
/// the tick rather than of distance travelled because the server owns it: every
/// client must see the same leg forward, and two clients deriving the cycle
/// from their own interpolation would drift apart by a frame or two, which
/// reads as a limp.
const WALK_CYCLE: u32 = 16;

/// Which kind of entity a [`Seen`] row refers to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    /// A player, keyed by [`PlayerId`].
    Player,
    /// A buddy, keyed by `EntityId::index`.
    Buddy,
}

/// One entity this client was told about on the previous snapshot.
///
/// Only used for the hysteresis band: "was it visible last tick" is the entire
/// question, and the position is re-read from the state, not remembered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Seen {
    kind: Kind,
    key: u16,
}

/// One player exactly as it went on the wire, so the next delta can be
/// computed against it.
///
/// Positions are absolute raw Q16.16 values here, not deltas: keeping the
/// running total is what makes a delta against an *older* frame possible.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct SentPlayer {
    id: u8,
    x: i32,
    y: i32,
    z: i32,
    yaw: u8,
    hp: u8,
    flags: u8,
    anim: u8,
}

/// One buddy as it went on the wire. Same reasoning as [`SentPlayer`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct SentBuddy {
    id: u8,
    x: i32,
    y: i32,
    z: i32,
    hp: u8,
}

/// One frame this client was sent, retained for acknowledgement.
///
/// Rows are stored in wire order, so a lookup is a linear scan over at most six
/// players and twelve buddies — cheaper than the hash the borrow checker would
/// insist on for a map.
#[derive(Clone, PartialEq, Eq, Debug)]
struct SentFrame {
    seq: u16,
    players: Vec<SentPlayer>,
    buddies: Vec<SentBuddy>,
}

/// What one call to [`SnapshotEncoder::encode`] produced.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Encoded {
    /// Sequence number of this snapshot. Never 0 — 0 means "keyframe / none" on
    /// the wire.
    pub seq: u16,
    /// The baseline this snapshot was encoded against; 0 for a keyframe.
    pub baseline_seq: u16,
    /// True when every field of every entity is present.
    pub keyframe: bool,
    /// The complete `S_SNAPSHOT` frame: tag byte, then postcard payload.
    pub bytes: Vec<u8>,
    /// Player entries written.
    pub players: usize,
    /// Buddy entries written.
    pub buddies: usize,
    /// Events that did not fit in this tick's `MAX_EVENTS` budget.
    ///
    /// Reported rather than swallowed: §6.3 says events are never culled, so an
    /// overflow is a design signal (a tick that produced more than 16 events),
    /// not something to hide in a `truncate`.
    pub dropped_events: usize,
}

/// Encodes the snapshot stream for one client.
#[derive(Clone, Debug)]
pub struct SnapshotEncoder {
    /// Whose eyes this stream is for.
    viewer: PlayerId,
    /// Last sequence number handed out; 0 before the first frame.
    seq: u16,
    /// Ticks since the last keyframe.
    since_keyframe: u16,
    /// Frames sent but not yet confirmed, oldest first.
    history: VecDeque<SentFrame>,
    /// Entities visible on the previous call, for the hysteresis band.
    seen: Vec<Seen>,
    /// Where the viewer was last seen, so a downed player still has an origin
    /// for interest management instead of a stream that collapses to nothing.
    last_viewer: Vec3,
}

impl SnapshotEncoder {
    /// A fresh encoder for `viewer`, with no history.
    ///
    /// The first frame is always a keyframe: the client has acknowledged
    /// nothing, so there is nothing to delta against.
    pub fn new(viewer: PlayerId) -> SnapshotEncoder {
        SnapshotEncoder {
            viewer,
            seq: 0,
            since_keyframe: 0,
            history: VecDeque::with_capacity(BASELINE_HISTORY),
            seen: Vec::with_capacity(MAX_PLAYERS + MAX_BUDDIES),
            last_viewer: Vec3::from_ints(0, 0, 0),
        }
    }

    /// The most recent sequence number handed out, or 0 before the first frame.
    pub fn last_seq(&self) -> u16 {
        self.seq
    }

    /// Encode one snapshot for this client.
    ///
    /// `acked_seq` is the newest snapshot sequence the client has reported
    /// receiving (0 for none). `projectiles` and `events` are the tick's
    /// transient state, which the room owns — the encoder does not read them
    /// from `MatchState` because the simulation does not store projectiles yet
    /// (combat is a pure function in `sim::combat`; the room keeps the live
    /// ones).
    pub fn encode(
        &mut self,
        now: Tick,
        state: &MatchState,
        projectiles: &[Projectile],
        events: &[SnapshotEvent],
        acked_seq: u16,
    ) -> Encoded {
        let viewer_pos = match state.body(self.viewer) {
            Some(body) => body.state.pos,
            None => self.last_viewer,
        };
        self.last_viewer = viewer_pos;

        // Resolve the baseline before touching anything: the keyframe decision
        // depends on whether the acknowledged frame still exists, and the
        // decision has to be made once, not per entity.
        //
        // `since` counts *this* frame, not the previous one: a keyframe at tick
        // K must be followed by one at K + 60, which is 60 frames later, and a
        // counter that starts at 0 and is tested before incrementing would fire
        // at K + 61 instead.
        let since = self.since_keyframe.saturating_add(1);
        let baseline = self
            .history
            .iter()
            .find(|frame| frame.seq == acked_seq && acked_seq != 0);
        let keyframe = acked_seq == 0 || baseline.is_none() || since >= KEYFRAME_INTERVAL;

        // Both are borrowed later while `self` is mutated, so take the row
        // lookups up front.
        let baseline_players: Option<&[SentPlayer]> = baseline.map(|f| f.players.as_slice());
        let baseline_buddies: Option<&[SentBuddy]> = baseline.map(|f| f.buddies.as_slice());

        let mut wire_players = Vec::with_capacity(MAX_PLAYERS);
        let mut sent_players = Vec::with_capacity(MAX_PLAYERS);
        let mut next_seen = Vec::with_capacity(MAX_PLAYERS + MAX_BUDDIES);

        for slot in state.players() {
            let Some(body) = state.body(slot.id) else {
                // A slot with no body is not in the world: nothing to draw, and
                // inventing a position would be worse than omitting it.
                continue;
            };
            let id = slot.id.0;
            let pos = body.state.pos;
            let distance = pos.horizontal_distance(viewer_pos);
            let own = slot.id == self.viewer;
            let visible = own
                || distance <= AOI_RADIUS
                || (self.was_seen(Kind::Player, id as u16)
                    && distance <= AOI_RADIUS + AOI_HYSTERESIS);
            if !visible {
                continue;
            }

            let mut flags = 0u8;
            if slot.life.is_alive() && body.is_up() {
                flags |= player_flags::ALIVE;
            }
            if slot.bot {
                flags |= player_flags::BOT;
            }
            if distance > AOI_RADIUS {
                // Only reachable through the hysteresis band, which is exactly
                // what STALE means: present, but its position is last-known.
                flags |= player_flags::STALE;
            }
            // ATTACKING and CARRYING are deliberately absent: `MatchState` does
            // not track a swing or a carried crate yet (T-025 adds the latter),
            // and a flag the server cannot compute is a lie the client renders.

            let row = SentPlayer {
                id,
                x: pos.x.to_raw(),
                y: pos.y.to_raw(),
                z: pos.z.to_raw(),
                yaw: body.yaw,
                hp: body.hp.clamp(0, 255) as u8,
                flags,
                anim: walk_phase(now, body.state.vel),
            };

            // §6.3's table is a *scope* table, and "own player: always, full
            // fidelity" is about inclusion: your own body is never culled, no
            // matter how far you are from yourself. It is deliberately not an
            // instruction to re-send it whole on every frame. An own-player
            // entry that is absolute while every other entry is a delta is a
            // decoder trap — one rule ("add the delta") that silently doubles
            // your position if you forget the exception, which is exactly the
            // failure the reconstruction test caught. One rule for every entity
            // costs a few bytes and removes the trap.
            let base = baseline_players.and_then(|rows| rows.iter().find(|b| b.id == id));
            wire_players.push(player_entry(&row, base, keyframe));
            sent_players.push(row);
            next_seen.push(Seen {
                kind: Kind::Player,
                key: id as u16,
            });
        }

        let mut wire_buddies = Vec::with_capacity(MAX_BUDDIES);
        let mut sent_buddies = Vec::with_capacity(MAX_BUDDIES);
        for team in state.teams() {
            for buddy in &team.buddies {
                let id = buddy.id.index() as u8;
                let distance = buddy.pos.horizontal_distance(viewer_pos);
                let visible = distance <= AOI_RADIUS
                    || (self.was_seen(Kind::Buddy, buddy.id.index() as u16)
                        && distance <= AOI_RADIUS + AOI_HYSTERESIS);
                if !visible || wire_buddies.len() >= MAX_BUDDIES {
                    continue;
                }

                let row = SentBuddy {
                    id,
                    x: buddy.pos.x.to_raw(),
                    y: buddy.pos.y.to_raw(),
                    z: buddy.pos.z.to_raw(),
                    hp: buddy.hp.clamp(0, 255) as u8,
                };
                let base = baseline_buddies.and_then(|rows| rows.iter().find(|b| b.id == id));
                wire_buddies.push(buddy_entry(&row, base, keyframe));
                sent_buddies.push(row);
                next_seen.push(Seen {
                    kind: Kind::Buddy,
                    key: buddy.id.index() as u16,
                });
            }
        }

        let wire_projectiles = projectile_entries(projectiles, viewer_pos);
        let wire_structures = structure_entries(state, viewer_pos);

        let (event_slice, dropped_events) = if events.len() > MAX_EVENTS {
            (&events[..MAX_EVENTS], events.len() - MAX_EVENTS)
        } else {
            (events, 0)
        };

        // The client cannot acknowledge a frame it cannot name. The wire
        // carries this sequence, and the next `C_INPUT` echoes it as the
        // baseline. Commit the counter only after a successful encode.
        let seq = if self.seq == u16::MAX {
            1
        } else {
            self.seq + 1
        };
        let snapshot = Snapshot {
            tick: now,
            seq,
            baseline_seq: if keyframe { 0 } else { acked_seq },
            keyframe,
            players: wire_players,
            buddies: wire_buddies,
            projectiles: wire_projectiles,
            structures: wire_structures,
            events: event_slice.to_vec(),
        };
        let bytes = S2c::Snapshot(snapshot).encode();
        self.seq = seq;
        let players = sent_players.len();
        let buddies = sent_buddies.len();

        self.history.push_back(SentFrame {
            seq,
            players: sent_players,
            buddies: sent_buddies,
        });
        while self.history.len() > BASELINE_HISTORY {
            self.history.pop_front();
        }
        self.since_keyframe = if keyframe { 0 } else { since };
        self.seen = next_seen;

        Encoded {
            seq,
            baseline_seq: if keyframe { 0 } else { acked_seq },
            keyframe,
            bytes,
            players,
            buddies,
            dropped_events,
        }
    }

    fn was_seen(&self, kind: Kind, key: u16) -> bool {
        self.seen.iter().any(|s| s.kind == kind && s.key == key)
    }
}

/// The walk-cycle phase for a body at `now`.
fn walk_phase(now: Tick, velocity: Vec3) -> u8 {
    if velocity.horizontal_length() <= MOVING_SPEED {
        return 0;
    }
    let step = 256 / WALK_CYCLE;
    ((now.0 % WALK_CYCLE) * step) as u8
}

/// Build one `SnapshotPlayer` from a row, as a delta against `base` or whole.
fn player_entry(row: &SentPlayer, base: Option<&SentPlayer>, full: bool) -> SnapshotPlayer {
    match base {
        Some(base) if !full => {
            let mut presence_bits = 0u16;
            if (row.x, row.y, row.z) != (base.x, base.y, base.z) {
                presence_bits |= presence::POS;
            }
            if row.yaw != base.yaw {
                presence_bits |= presence::YAW;
            }
            if row.hp != base.hp {
                presence_bits |= presence::HP;
            }
            if row.flags != base.flags {
                presence_bits |= presence::FLAGS;
            }
            if row.anim != base.anim {
                presence_bits |= presence::ANIM;
            }
            SnapshotPlayer {
                id: row.id,
                presence: presence_bits,
                x: row.x - base.x,
                y: row.y - base.y,
                z: row.z - base.z,
                yaw: row.yaw,
                hp: row.hp,
                flags: row.flags,
                anim: row.anim,
            }
        }
        // No baseline row means the client has never been told about this
        // entity. A delta relative to zero *is* an absolute value, which is why
        // "new entity" needs no extra wire bit: the client applies the delta to
        // the origin it does not have, i.e. zero, and lands on the truth.
        _ => SnapshotPlayer {
            id: row.id,
            presence: presence::ALL,
            x: row.x,
            y: row.y,
            z: row.z,
            yaw: row.yaw,
            hp: row.hp,
            flags: row.flags,
            anim: row.anim,
        },
    }
}

/// Build one `SnapshotBuddy` from a row. Same rules as [`player_entry`].
fn buddy_entry(row: &SentBuddy, base: Option<&SentBuddy>, full: bool) -> SnapshotBuddy {
    match base {
        Some(base) if !full => {
            let mut presence_bits = 0u16;
            if (row.x, row.y, row.z) != (base.x, base.y, base.z) {
                presence_bits |= presence::POS;
            }
            if row.hp != base.hp {
                presence_bits |= presence::HP;
            }
            SnapshotBuddy {
                id: row.id,
                presence: presence_bits,
                x: row.x - base.x,
                y: row.y - base.y,
                z: row.z - base.z,
                hp: row.hp,
            }
        }
        _ => SnapshotBuddy {
            id: row.id,
            presence: presence::POS | presence::HP,
            x: row.x,
            y: row.y,
            z: row.z,
            hp: row.hp,
        },
    }
}

/// Projectiles in range, nearest first, capped at `MAX_PROJECTILES`.
///
/// Nearest-first rather than state order because the cap has to mean something:
/// if 30 arrows are in flight, the four a client is most likely to be hit by
/// are the four it should see.
fn projectile_entries(projectiles: &[Projectile], viewer: Vec3) -> Vec<SnapshotProjectile> {
    let mut in_range: Vec<(i32, u32, &Projectile)> = projectiles
        .iter()
        .filter_map(|p| {
            let distance = p.pos.horizontal_distance(viewer);
            (distance <= AOI_RADIUS).then(|| (distance.to_raw(), p.id.index(), p))
        })
        .collect();
    in_range.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    in_range.truncate(MAX_PROJECTILES);

    in_range
        .into_iter()
        .map(|(_, _, p)| {
            let velocity = p.dir * p.speed;
            SnapshotProjectile {
                id: p.id.index() as u16,
                x: clamp_i16(p.pos.x.round_int()),
                y: clamp_i16(p.pos.y.round_int()),
                z: clamp_i16(p.pos.z.round_int()),
                vx: clamp_i8(velocity.x.round_int()),
                vy: clamp_i8(velocity.y.round_int()),
                vz: clamp_i8(velocity.z.round_int()),
            }
        })
        .collect()
}

/// One structure per team: its core, as a percentage of maximum health.
///
/// Cores are AOI-filtered like every other structure (§6.3). The HUD's
/// knowledge of a distant core's health comes from `BASE_HIT` events, which are
/// never culled — the number in the corner does not require the entity in the
/// snapshot.
fn structure_entries(state: &MatchState, viewer: Vec3) -> Vec<SnapshotStructure> {
    let mut rows: Vec<(u8, SnapshotStructure)> = state
        .teams()
        .iter()
        .filter_map(|team| {
            let core = &team.base.core;
            let origin = core.origin();
            let pos = Vec3::from_ints(origin[0], origin[1], origin[2]);
            (pos.horizontal_distance(viewer) <= AOI_RADIUS).then(|| {
                (
                    team.id.0,
                    SnapshotStructure {
                        id: team.id.0,
                        hp: percent(core.fraction()),
                        x: clamp_i16(origin[0]),
                        y: clamp_i16(origin[1]),
                        z: clamp_i16(origin[2]),
                    },
                )
            })
        })
        .collect();
    // Team order, not distance order: the cap is on a list of at most six, and
    // a stable order keeps two encodes of the same state byte-identical.
    rows.sort_by_key(|(id, _)| *id);
    rows.truncate(MAX_STRUCTURES);
    rows.into_iter().map(|(_, row)| row).collect()
}

/// A 0..=1 fixed-point fraction as 0..=100, without a float round-trip.
fn percent(fraction: Fixed) -> u8 {
    let hundredths = (fraction.to_raw() as i64 * 100) / i64::from(1 << 16);
    hundredths.clamp(0, 100) as u8
}

fn clamp_i16(value: i32) -> i16 {
    value.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

fn clamp_i8(value: i32) -> i8 {
    value.clamp(i32::from(i8::MIN), i32::from(i8::MAX)) as i8
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{decode_s2c, S2c};
    use sim::{buddy::BuddyKind, tuning, Buddy, Mode, RoomSetup, TeamId};

    fn room(players: u8) -> crate::Room {
        crate::Room::on_ring(Tick(0), RoomSetup::humans(Mode::Ffa, None, players), 20, 40)
            .expect("room")
    }

    /// Decode an encoded frame back into a snapshot, failing loudly.
    fn decode(encoded: &Encoded) -> Snapshot {
        match decode_s2c(&encoded.bytes).expect("decode") {
            S2c::Snapshot(snapshot) => snapshot,
            other => panic!("expected a snapshot, got {other:?}"),
        }
    }

    fn stand(room: &mut crate::Room, id: PlayerId, x: i32, y: i32, z: i32) {
        let body = room.state_mut().body_mut(id).expect("body");
        body.state.pos = Vec3::from_ints(x, y, z);
        body.spawn = body.state.pos;
    }

    #[test]
    fn the_first_frame_is_a_keyframe_and_whole() {
        let room = room(2);
        let mut encoder = SnapshotEncoder::new(PlayerId(0));
        let encoded = encoder.encode(Tick(10), room.state(), &[], &[], 0);

        assert!(encoded.keyframe);
        assert_eq!(encoded.baseline_seq, 0);
        assert_eq!(encoded.seq, 1);

        let snapshot = decode(&encoded);
        assert!(snapshot.keyframe);
        assert_eq!(snapshot.seq, encoded.seq, "the wire must name the frame");
        assert_eq!(snapshot.baseline_seq, 0);
        for player in &snapshot.players {
            assert_eq!(
                player.presence,
                presence::ALL,
                "a keyframe carries every field of every entity"
            );
        }
    }

    #[test]
    fn a_second_frame_against_a_fresh_ack_omits_unchanged_fields() {
        let room = room(2);
        let mut encoder = SnapshotEncoder::new(PlayerId(0));
        let first = encoder.encode(Tick(10), room.state(), &[], &[], 0);
        // Nothing moved, so the delta against the first frame is all-empty.
        let second = encoder.encode(Tick(11), room.state(), &[], &[], first.seq);

        assert!(!second.keyframe, "an acked baseline must produce a delta");
        assert_eq!(second.baseline_seq, first.seq);
        let snapshot = decode(&second);
        // Every entity obeys the same rule, including the viewer's own body:
        // absent from the baseline means absolute, present and unchanged means
        // no fields at all.
        for player in &snapshot.players {
            assert_eq!(
                player.presence, 0,
                "player {} is unchanged, so it carries no fields",
                player.id
            );
        }
    }

    #[test]
    fn an_unresolvable_ack_falls_back_to_a_keyframe() {
        let room = room(2);
        let mut encoder = SnapshotEncoder::new(PlayerId(0));
        let first = encoder.encode(Tick(10), room.state(), &[], &[], 0);
        let second = encoder.encode(Tick(11), room.state(), &[], &[], first.seq);
        // 9_999 was never sent: the client is describing a world this server did
        // not produce, so guessing would be worse than a keyframe.
        let third = encoder.encode(Tick(12), room.state(), &[], &[], 9_999);
        assert!(third.keyframe);
        assert_eq!(third.baseline_seq, 0);
        assert_ne!(second.seq, third.seq);
    }

    #[test]
    fn a_keyframe_is_sent_every_sixty_ticks() {
        let room = room(2);
        let mut encoder = SnapshotEncoder::new(PlayerId(0));
        let mut acked = encoder.encode(Tick(0), room.state(), &[], &[], 0).seq;

        let mut keyframes = 0;
        for tick in 1..=KEYFRAME_INTERVAL {
            let encoded = encoder.encode(Tick(tick as u32), room.state(), &[], &[], acked);
            if encoded.keyframe {
                keyframes += 1;
            }
            acked = encoded.seq;
        }
        assert_eq!(keyframes, 1, "exactly one forced keyframe in 60 ticks");
    }

    #[test]
    fn sequence_numbers_wrap_to_one_and_never_to_zero() {
        let room = room(2);
        let mut encoder = SnapshotEncoder::new(PlayerId(0));
        // Fast-forward to the last number without encoding 65,535 frames: the
        // wrap is a property of the counter, not of the frames.
        encoder.seq = u16::MAX - 1;
        let a = encoder.encode(Tick(1), room.state(), &[], &[], 0);
        let b = encoder.encode(Tick(2), room.state(), &[], &[], a.seq);
        assert_eq!(a.seq, u16::MAX);
        assert_eq!(b.seq, 1, "0 is reserved for keyframe/none on the wire");
    }

    #[test]
    fn a_stationary_entity_costs_no_position_bytes() {
        let mut room = room(2);
        let mut encoder = SnapshotEncoder::new(PlayerId(0));
        let first = encoder.encode(Tick(0), room.state(), &[], &[], 0);
        let still = encoder.encode(Tick(1), room.state(), &[], &[], first.seq);
        stand(&mut room, PlayerId(1), 40, 40, 40);
        let moved = encoder.encode(Tick(2), room.state(), &[], &[], still.seq);

        let still_size = decode(&still).players[1].presence;
        let moved_size = decode(&moved).players[1].presence;
        assert_eq!(still_size & presence::POS, 0);
        assert_ne!(
            moved_size & presence::POS,
            0,
            "a player that moved must carry a position"
        );
        // The delta is small enough to be a varint: one tick at walking speed is
        // well under a metre, so each axis costs one or two bytes.
        assert!(moved.bytes.len() > still.bytes.len());
    }

    #[test]
    fn the_hysteresis_band_keeps_an_entity_visible_and_flags_it_stale() {
        let mut room = room(2);
        let mut encoder = SnapshotEncoder::new(PlayerId(0));
        stand(&mut room, PlayerId(0), 0, 40, 0);

        // 47 m: inside the radius, not stale.
        stand(&mut room, PlayerId(1), 47, 40, 0);
        let inside = encoder.encode(Tick(0), room.state(), &[], &[], 0);
        let snapshot = decode(&inside);
        let other = snapshot.players.iter().find(|p| p.id == 1).expect("other");
        assert_eq!(other.flags & player_flags::STALE, 0);

        // 50 m: outside the radius but inside the band, so still present — and
        // flagged, because the position is last-known rather than guaranteed.
        stand(&mut room, PlayerId(1), 50, 40, 0);
        let banded = encoder.encode(Tick(1), room.state(), &[], &[], inside.seq);
        let snapshot = decode(&banded);
        let other = snapshot.players.iter().find(|p| p.id == 1).expect("other");
        assert_ne!(
            other.flags & player_flags::STALE,
            0,
            "beyond 48 m the client must be told the position is stale"
        );

        // 53 m: past the band, gone. The client removes what it is not told
        // about, which is why no explicit "despawn" message exists.
        stand(&mut room, PlayerId(1), 53, 40, 0);
        let gone = encoder.encode(Tick(2), room.state(), &[], &[], banded.seq);
        let snapshot = decode(&gone);
        assert!(
            snapshot.players.iter().all(|p| p.id != 1),
            "an entity past the hysteresis band must leave the snapshot"
        );
    }

    #[test]
    fn a_fresh_entity_outside_the_radius_is_never_sent() {
        let mut room = room(2);
        let mut encoder = SnapshotEncoder::new(PlayerId(0));
        stand(&mut room, PlayerId(0), 0, 40, 0);
        stand(&mut room, PlayerId(1), 49, 40, 0);
        let encoded = encoder.encode(Tick(0), room.state(), &[], &[], 0);
        let snapshot = decode(&encoded);
        assert!(
            snapshot.players.iter().all(|p| p.id != 1),
            "the band only extends visibility, it never grants it"
        );
        assert_eq!(encoded.players, 1, "the viewer is always included");
    }

    #[test]
    fn events_are_never_culled_and_the_overflow_is_reported() {
        let room = room(2);
        let mut encoder = SnapshotEncoder::new(PlayerId(0));
        let events = vec![
            SnapshotEvent {
                kind: protocol::event_kind::BASE_HIT,
                a: 1,
                b: 2,
                c: 3,
            };
            MAX_EVENTS + 3
        ];
        let encoded = encoder.encode(Tick(0), room.state(), &[], &events, 0);
        assert_eq!(encoded.dropped_events, 3);
        let snapshot = decode(&encoded);
        assert_eq!(snapshot.events.len(), MAX_EVENTS);
        assert!(
            !snapshot.events.is_empty(),
            "events are the game's information layer; they are never culled"
        );
    }

    #[test]
    fn a_buddy_beyond_the_radius_is_dropped_and_one_inside_is_sent() {
        let mut room = room(2);
        let mut encoder = SnapshotEncoder::new(PlayerId(0));
        stand(&mut room, PlayerId(0), 0, 40, 0);
        let near = Buddy::new(
            sim::EntityId::new(1, 0),
            tuning::buddy(BuddyKind::Swordsman),
            TeamId(1),
            PlayerId(1),
            Vec3::from_ints(10, 40, 0),
        );
        let far = Buddy::new(
            sim::EntityId::new(2, 0),
            tuning::buddy(BuddyKind::Archer),
            TeamId(1),
            PlayerId(1),
            Vec3::from_ints(200, 40, 0),
        );
        room.state_mut().teams_mut()[1].buddies.push(near);
        room.state_mut().teams_mut()[1].buddies.push(far);

        let encoded = encoder.encode(Tick(0), room.state(), &[], &[], 0);
        let snapshot = decode(&encoded);
        assert_eq!(snapshot.buddies.len(), 1);
        assert_eq!(snapshot.buddies[0].id, 1);
        assert_eq!(
            snapshot.buddies[0].presence,
            presence::POS | presence::HP,
            "a buddy has no yaw, flags or animation fields to send"
        );
    }
}
