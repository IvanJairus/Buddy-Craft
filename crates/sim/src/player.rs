//! Player bodies: the part of a player that can move.
//!
//! # Why this is a separate type from [`crate::base::PlayerSlot`]
//!
//! `PlayerSlot` is what *elimination* sees, and D-02 requires that elimination
//! look at nothing but `team` and `life` — a player mid-swing dies on the same
//! tick as one standing still. A body is the opposite: it is everything
//! elimination must ignore. Keeping them in two types is what makes "elimination
//! does not depend on the body" checkable by reading one struct definition
//! instead of auditing a function.
//!
//! # Two kinds of dead
//!
//! There are two independent ways for a player to stop moving, and conflating
//! them is the bug this module is written to make impossible:
//!
//! | | `hp` | `life` | Comes back? |
//! |---|---|---|---|
//! | Shot to zero | `0` | `Alive` | Yes, after [`respawn_ticks`] |
//! | Team eliminated (D-02) | anything | `Dead` | Never |
//!
//! `PlayerLife` is one-way (there is no spectator state, §4.8), so a respawn
//! must check it first. A body whose `respawn_at` fires while its slot is `Dead`
//! is a bug, and [`MatchState::respawn_due`](crate::state::MatchState::respawn_due)
//! is written to skip it rather than trust the caller.
//!
//! # Spawn points are geometry, not a constant
//!
//! "Cannot respawn inside a block" is a rule, not an assertion, so the spawn
//! point is *searched*: a ring walk outward from the pad's centre, skipping the
//! core's protected volume and any column a crate stands in, then a second
//! search upward for a free AABB. Both searches are deterministic and bounded,
//! which matters because the server and the client both run them.

use crate::base::PlayerLife;
use crate::ids::PlayerId;
use crate::input::{buttons, InputFrame};
use crate::movement::{self, MoveConfig, MoveFlags, MoveIntent, MoveState};
use crate::pad::{Pad, PAD_HEIGHT, PAD_SIZE};
use crate::time::{Tick, TICK_HZ};
use crate::tuning;
use crate::vec::Vec3;
use crate::world_query::VoxelQuery;

/// Ticks a player waits before respawning (§4.8: 8 s at their own base).
pub fn respawn_ticks() -> u32 {
    tuning::embedded().player.respawn_s * TICK_HZ
}

/// Ticks of invulnerability after a respawn (§4.8: 2 s).
///
/// Protection is two-way. §4.8's own wording is "2 s spawn protection", and the
/// design note behind it is that a fresh player must not be able to walk out of
/// the pad and delete someone who is already fighting. [`PlayerBody::is_protected`]
/// is therefore consulted by both the damage path and the attack path; this
/// module only owns the flag.
pub fn protect_ticks() -> u32 {
    tuning::embedded().player.spawn_protect_s * TICK_HZ
}

/// How far the spawn search will look upward for a free AABB, in blocks.
///
/// A pad is four blocks tall, so six is "above anything that could be built on
/// it" with two blocks of margin for a body that is 1.8 tall.
const SPAWN_LIFT_LIMIT: i32 = 6;

/// One player's body.
///
/// `Copy` because a body is four fixed-width fields and a `MoveState`; there is
/// no allocation and no ownership, which keeps `MatchState` cheap to clone for
/// the determinism harness.
#[derive(Clone, Copy, Debug)]
pub struct PlayerBody {
    /// Position, velocity and ground contact.
    pub state: MoveState,
    /// Current HP. Zero means "down, waiting for the timer".
    pub hp: i32,
    /// Where this player respawns. Set from the pad at match start and
    /// overridable so a harness can place bodies deliberately.
    pub spawn: Vec3,
    /// Tick the player comes back, if they are down.
    pub respawn_at: Option<Tick>,
    /// Tick spawn protection expires, if it is running.
    pub protect_until: Option<Tick>,
    /// The previous tick's buttons, so edges are derived rather than sent.
    pub prev_buttons: u8,
}

impl PlayerBody {
    /// A body standing at `pos` with `max_hp` health.
    pub fn new(pos: Vec3, max_hp: i32) -> PlayerBody {
        PlayerBody {
            state: MoveState::at(pos),
            hp: max_hp,
            spawn: pos,
            respawn_at: None,
            protect_until: None,
            prev_buttons: 0,
        }
    }

    /// A body at the tuning table's full health.
    pub fn spawned(pos: Vec3) -> PlayerBody {
        PlayerBody::new(pos, tuning::embedded().player.hp)
    }

    /// True while the body is standing and can act.
    #[inline]
    pub fn is_up(&self) -> bool {
        self.hp > 0
    }

    /// True while spawn protection is running at `now`.
    ///
    /// The comparison is `<`, not `<=`: protection for exactly
    /// [`protect_ticks`] ticks means it is gone on the tick it expires, so a
    /// 2 s window covers 40 ticks rather than 41.
    #[inline]
    pub fn is_protected(&self, now: Tick) -> bool {
        match self.protect_until {
            Some(until) => now.0 < until.0,
            None => false,
        }
    }

    /// Put the body back on its feet at `at`.
    ///
    /// Resets the movement state rather than reusing it, so velocity, ground
    /// contact and water contact all start clean — a player who died falling
    /// must not respawn still falling.
    pub fn respawn(&mut self, at: Vec3, now: Tick) {
        self.state = MoveState::at(at);
        self.hp = tuning::embedded().player.hp;
        self.respawn_at = None;
        self.protect_until = Some(now.advance(protect_ticks()));
        self.prev_buttons = 0;
    }
}

/// What one player's frame asked for and what came of it.
///
/// The movement result is a value rather than a mutation of the caller's state,
/// so a client can predict without committing and a server can log without
/// inspecting.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct PlayerTick {
    /// Movement outcome: landed, jumped, blocked, stepped up.
    pub flags: MoveFlags,
    /// The fire button went down this tick (an edge, not a hold).
    pub fired: bool,
    /// The use button went down this tick.
    pub used: bool,
    /// The stance button went down this tick.
    pub stanced: bool,
}

/// The nearest position to `base` where a player's AABB fits.
///
/// Searched in a fixed order — height first, then ring radius, and within a ring
/// the four cardinals before the corners — so the answer is identical on every
/// target and on both the server and the client.
///
/// The cardinal-first pass is not cosmetic. `(r, 0)` is `r` away and `(r, r)` is
/// `r * sqrt(2)` away, so a plain `(dz, dx)` walk of a ring takes a corner when
/// a nearer cardinal was free: a respawn 41% further from where the player died,
/// for no reason anyone could see.
///
/// If nothing within [`SPAWN_LIFT_LIMIT`] is free the base point is returned
/// unchanged; `movement::step`'s unstick then lifts the body out on its first
/// tick, which is a worse answer than a good spawn point but a better one than a
/// panic.
pub fn nearest_free<W: VoxelQuery + ?Sized>(world: &W, cfg: &MoveConfig, base: Vec3) -> Vec3 {
    for dy in 0..=SPAWN_LIFT_LIMIT {
        for r in 0..=2i32 {
            for pass in 0..2 {
                for dz in -r..=r {
                    for dx in -r..=r {
                        // Only the ring's perimeter: the interior was covered by
                        // a smaller `r`, and re-testing it would waste probes on
                        // a pad that is already full.
                        if dx.abs().max(dz.abs()) != r {
                            continue;
                        }
                        let cardinal = dx == 0 || dz == 0;
                        if cardinal != (pass == 0) {
                            continue;
                        }
                        let mut probe = base;
                        probe.x += crate::fixed::Fixed::from_int(dx);
                        probe.y += crate::fixed::Fixed::from_int(dy);
                        probe.z += crate::fixed::Fixed::from_int(dz);
                        if !movement::collides(world, cfg, probe) {
                            return probe;
                        }
                    }
                }
            }
        }
    }
    base
}

/// The spawn point for a team whose pad is `pad`.
///
/// Walks rings outward from the pad's centre, cardinals first, and returns the
/// first column that is (a) inside the pad, (b) not part of the core's protected
/// volume, (c) not occupied by a crate at any height, and (d) clear in the world.
/// The centre itself is rejected by (b) in every layout, because the core is
/// centred on the pad — so this never returns a point inside the core, which the
/// world query cannot see.
pub fn spawn_point<W: VoxelQuery + ?Sized>(world: &W, cfg: &MoveConfig, pad: &Pad) -> Vec3 {
    let origin = pad.origin();
    let centre = PAD_SIZE / 2;

    for r in 0..=centre {
        for pass in 0..2 {
            for dz in -r..=r {
                for dx in -r..=r {
                    if dx.abs().max(dz.abs()) != r {
                        continue;
                    }
                    let cardinal = dx == 0 || dz == 0;
                    if cardinal != (pass == 0) {
                        continue;
                    }
                    let lx = centre + dx;
                    let lz = centre + dz;
                    if !(0..PAD_SIZE).contains(&lx) || !(0..PAD_SIZE).contains(&lz) {
                        continue;
                    }
                    if !column_clear(pad, lx, lz) {
                        continue;
                    }
                    let pos = Vec3::from_ints(origin[0] + lx, origin[1], origin[2] + lz);
                    if !movement::collides(world, cfg, pos) {
                        return pos;
                    }
                }
            }
        }
    }

    // Every pad cell is built over. Spawn above the build limit rather than
    // inside a crate tower; `nearest_free` will lift further if the world is
    // solid up there too.
    Vec3::from_ints(
        origin[0] + centre,
        origin[1] + PAD_HEIGHT + 1,
        origin[2] + centre,
    )
}

/// True when nothing in `pad` occupies the column at local `(lx, lz)`.
///
/// Both masks are consulted: `is_blocked` is the core's protected volume, which
/// is static geometry stamped at layout, and `get` is a placed crate. A crate at
/// any height counts, because a body is 1.8 blocks tall and would be standing
/// inside it.
fn column_clear(pad: &Pad, lx: i32, lz: i32) -> bool {
    (0..PAD_HEIGHT).all(|y| !pad.is_blocked([lx, y, lz]) && !pad.get([lx, y, lz]))
}

/// Why a player could not take an input frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NoBody {
    /// The seat does not exist.
    NoSuchPlayer,
    /// The team was eliminated (D-02). There is no coming back.
    Eliminated,
    /// Down and waiting for the respawn timer.
    Down,
}

impl NoBody {
    /// True when the reason is permanent.
    pub fn is_final(self) -> bool {
        matches!(self, NoBody::Eliminated)
    }
}

/// Apply one resolved input frame to one body.
///
/// Free function rather than a method on `MatchState` so the borrow of the
/// body and the read of the slot do not have to be split by the caller; the
/// match state's wrapper is three lines and adds the slot checks.
///
/// `previous_buttons` is the *previous* tick's byte, and it is the caller's job
/// to have stored it. Edges are derived here rather than sent on the wire, so a
/// dropped packet cannot silently eat a jump (§6.2).
pub fn apply<W: VoxelQuery + ?Sized>(
    body: &mut PlayerBody,
    frame: &InputFrame,
    previous_buttons: u8,
    world: &W,
    cfg: &MoveConfig,
) -> PlayerTick {
    let intent = MoveIntent::from_frame(frame, previous_buttons);
    let flags = movement::step(world, cfg, &mut body.state, intent, movement::tick_dt());
    PlayerTick {
        flags,
        fired: frame.pressed(buttons::FIRE, previous_buttons),
        used: frame.pressed(buttons::USE, previous_buttons),
        stanced: frame.pressed(buttons::STANCE, previous_buttons),
    }
}

/// Kill a body: zero HP, stop it dead, start the timer.
///
/// Separated from [`apply`] because it is called from three places — melee, a
/// projectile, and a fall — and none of them should have to remember which
/// fields to reset.
pub fn down(body: &mut PlayerBody, now: Tick) {
    body.hp = 0;
    // Velocity is cleared, not merely zeroed vertically: a corpse that keeps
    // sliding is a corpse the client renders moving, and the client predicts
    // from the same state.
    body.state.vel = Vec3::ZERO;
    body.respawn_at = Some(now.advance(respawn_ticks()));
    // Protection does not survive death. Otherwise a player killed during their
    // 2 s window would respawn with the old window still running and a fresh
    // one on top of it.
    body.protect_until = None;
}

/// Remove a body from the match for good (D-02).
///
/// Distinct from [`down`] in exactly one field, and it is the field that
/// matters: `respawn_at` stays `None`, so nothing can bring the body back. The
/// seat's `PlayerLife` is what stops input being applied; this is what stops the
/// state hash and the snapshot from reporting a team at full health that is no
/// longer in the match.
pub fn eliminate(body: &mut PlayerBody) {
    body.hp = 0;
    body.state.vel = Vec3::ZERO;
    body.respawn_at = None;
    body.protect_until = None;
}

/// True when `life` still allows a body to exist in the simulation.
#[inline]
pub fn may_move(life: PlayerLife) -> bool {
    life.is_alive()
}

/// True when `id` is a seat this module can address.
#[inline]
pub fn is_seated(players: &[crate::base::PlayerSlot], id: PlayerId) -> bool {
    players.iter().any(|slot| slot.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pad::Pads;
    use crate::world_query::{EmptyWorld, VoxelGrid};

    fn cfg() -> MoveConfig {
        MoveConfig::embedded()
    }

    fn max_hp() -> i32 {
        tuning::embedded().player.hp
    }

    /// A pad at the origin whose walking surface is `y = 0`, with a floor.
    fn pad_world() -> (Pads, VoxelGrid) {
        let mut pads = Pads::new();
        let pad = Pad::new([0, 0, 0]);
        let team = pads.push(pad);
        assert_eq!(team, crate::ids::TeamId(0));
        let mut world = VoxelGrid::new([-16, -4, -16], [48, 16, 48]);
        world.fill_layer(-1, true);
        (pads, world)
    }

    #[test]
    fn spawn_never_lands_inside_the_core() {
        // The core is not a voxel, so the world query cannot see it. This is the
        // test that fails if `spawn_point` ever starts trusting `collides`
        // alone: with an empty world every point is "free", including the one
        // the core is standing on.
        let (mut pads, world) = pad_world();
        let pad = pads.get_mut(crate::ids::TeamId(0)).expect("pad");
        let core = crate::base::Base::core_origin_for_pad(pad.origin());
        crate::base::Base::new(crate::ids::TeamId(0), core).stamp_protected_volume(pad);

        let pos = spawn_point(&world, &cfg(), pad);
        let cell = pos.floor_to_ints();
        assert!(
            !pad.is_blocked([cell[0], 0, cell[2]]),
            "spawned inside the core's protected volume at {cell:?}"
        );
        // And it is actually on the pad.
        for (axis, value) in cell.iter().enumerate().take(3) {
            let origin = pad.origin()[axis];
            assert!(
                (*value - origin).abs() < PAD_SIZE,
                "spawn escaped the pad on axis {axis}: {cell:?}"
            );
        }
    }

    #[test]
    fn spawn_avoids_a_column_with_a_crate_in_it() {
        let (mut pads, world) = pad_world();
        let team = crate::ids::TeamId(0);
        let pad = pads.get_mut(team).expect("pad");
        let core = crate::base::Base::core_origin_for_pad(pad.origin());
        crate::base::Base::new(team, core).stamp_protected_volume(pad);

        // Wall off every pad column except one, so the search has exactly one
        // answer and the test can assert it rather than "not the centre".
        let mut open = None;
        for lx in 0..PAD_SIZE {
            for lz in 0..PAD_SIZE {
                let cell = [lx, 0, lz];
                if pad.is_blocked(cell) {
                    continue;
                }
                if open.is_none() {
                    open = Some((lx, lz));
                } else {
                    pad.set(cell, true);
                }
            }
        }
        let (lx, lz) = open.expect("a buildable cell outside the core");
        let pos = spawn_point(&world, &cfg(), pad);
        assert_eq!(pos.floor_to_ints(), [lx, 0, lz]);
    }

    #[test]
    fn spawn_falls_back_above_a_full_pad() {
        let (mut pads, world) = pad_world();
        let team = crate::ids::TeamId(0);
        let pad = pads.get_mut(team).expect("pad");
        for lx in 0..PAD_SIZE {
            for lz in 0..PAD_SIZE {
                for y in 0..PAD_HEIGHT {
                    pad.set([lx, y, lz], true);
                }
            }
        }
        let pos = spawn_point(&world, &cfg(), pad);
        assert_eq!(
            pos.floor_to_ints(),
            [PAD_SIZE / 2, PAD_HEIGHT + 1, PAD_SIZE / 2],
            "a fully built pad must spawn above the build limit"
        );
    }

    #[test]
    fn nearest_free_steps_sideways_before_lifting() {
        // One blocked column. The search prefers a neighbour at the same height
        // over climbing, because a player respawning one block to the side is
        // invisible while a player respawning three blocks up is a fall.
        let mut world = VoxelGrid::new([-8, -4, -8], [32, 16, 32]);
        world.fill_layer(-1, true);
        world.fill_box([0, 0, 0], [0, 2, 0], true);

        let base = Vec3::from_ints(0, 0, 0);
        let free = nearest_free(&world, &cfg(), base);
        assert!(
            !movement::collides(&world, &cfg(), free),
            "nearest_free returned an occupied point: {free:?}"
        );
        assert_eq!(free.y, base.y, "it should have stepped aside, not up");
        assert_eq!(
            free.horizontal_distance(base),
            crate::fixed::Fixed::from_int(1),
            "the cheapest sideways move is one block"
        );
    }

    #[test]
    fn nearest_free_lifts_when_the_whole_neighbourhood_is_solid() {
        let mut world = VoxelGrid::new([-8, -4, -8], [32, 16, 32]);
        world.fill_layer(-1, true);
        // Everything within the sideways search radius, three blocks tall.
        world.fill_box([-2, 0, -2], [2, 2, 2], true);

        let base = Vec3::from_ints(0, 0, 0);
        let free = nearest_free(&world, &cfg(), base);
        assert!(
            !movement::collides(&world, &cfg(), free),
            "nearest_free returned an occupied point: {free:?}"
        );
        assert_eq!(
            free.y,
            crate::fixed::Fixed::from_int(3),
            "the first free height above a 3-tall block is 3, got {free:?}"
        );
    }

    #[test]
    fn nearest_free_returns_the_base_when_nothing_blocks_it() {
        let world = EmptyWorld;
        let base = Vec3::from_ints(3, 7, 11);
        assert_eq!(nearest_free(&world, &cfg(), base), base);
    }

    #[test]
    fn a_body_starts_up_and_unprotected() {
        let body = PlayerBody::spawned(Vec3::from_ints(0, 0, 0));
        assert!(body.is_up());
        assert_eq!(body.hp, max_hp());
        assert!(!body.is_protected(Tick::ZERO));
        assert!(body.respawn_at.is_none());
    }

    #[test]
    fn downing_starts_the_timer_and_clears_velocity() {
        let mut body = PlayerBody::spawned(Vec3::from_ints(0, 0, 0));
        body.state.vel = Vec3::from_ints(3, -2, 1);
        down(&mut body, Tick(100));
        assert!(!body.is_up());
        assert_eq!(body.hp, 0);
        assert_eq!(body.state.vel, Vec3::ZERO, "a corpse must not slide");
        assert_eq!(body.respawn_at, Some(Tick(100 + respawn_ticks())));
    }

    #[test]
    fn protection_expires_exactly_on_the_tick_it_says() {
        let mut body = PlayerBody::spawned(Vec3::from_ints(0, 0, 0));
        body.respawn(Vec3::from_ints(0, 0, 0), Tick(50));
        let until = 50 + protect_ticks();
        assert_eq!(body.protect_until, Some(Tick(until)));
        assert!(body.is_protected(Tick(50)), "protected on the respawn tick");
        assert!(
            body.is_protected(Tick(until - 1)),
            "protected on the last tick of the window"
        );
        assert!(
            !body.is_protected(Tick(until)),
            "protection must end on the expiry tick, not one later"
        );
    }

    #[test]
    fn respawning_restores_health_and_clears_the_button_history() {
        let mut body = PlayerBody::spawned(Vec3::from_ints(9, 9, 9));
        body.prev_buttons = buttons::FIRE;
        down(&mut body, Tick(0));
        body.respawn(Vec3::from_ints(1, 2, 3), Tick(respawn_ticks()));
        assert!(body.is_up());
        assert_eq!(body.hp, max_hp());
        assert_eq!(body.state.pos, Vec3::from_ints(1, 2, 3));
        assert_eq!(
            body.prev_buttons, 0,
            "a respawn must not inherit a held button, or the first shot is eaten"
        );
        assert!(body.respawn_at.is_none());
    }

    #[test]
    fn an_eliminated_slot_is_permanently_unusable() {
        // The table in the module docs, asserted: `Alive` may move, `Dead` may
        // not, and the two are not the same question as `hp`.
        assert!(may_move(PlayerLife::Alive));
        assert!(!may_move(PlayerLife::Dead));
        assert!(!NoBody::Down.is_final());
        assert!(NoBody::Eliminated.is_final());
    }
}
