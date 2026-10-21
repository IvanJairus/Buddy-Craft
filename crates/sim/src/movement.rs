//! Movement and collision (plan T-009).
//!
//! # Shape of the problem
//!
//! A player is a 0.6 x 1.8 x 0.6 box that must never overlap a solid voxel. At
//! 4.5 m/s and 20 Hz that is 0.225 m per tick, which is small enough that a
//! naive "move then push out" would mostly work — but arrows travel at 40 m/s
//! (2 m per tick) and knockback can stack, so the sweep has to be correct at
//! speeds where a single tick jumps clean over a wall.
//!
//! The sweep therefore never moves more than [`MoveConfig::max_substep`] in one
//! go, and when a step collides it binary-searches the contact point instead of
//! accepting the previous position. "Previous position" is what produces the
//! classic sticky-wall bug, where a player cannot slide along a surface because
//! every tick snaps them back to where they started.
//!
//! # Why the timestep is subdivided
//!
//! Gravity integrated once per tick at 20 Hz loses about 18% of a jump's apex to
//! Euler error: `v0 * dt` sampling overshoots the parabola's peak. Four
//! substeps bring the error under 1%, which is the difference between a jump
//! that clears a 1-block ledge and one that clips it. The substepping is
//! internal, so the outer tick rate stays 20 Hz and the netcode is unaffected.
//!
//! # What is deliberately simple
//!
//! Horizontal acceleration is instant: `velocity = wish_direction * speed`.
//! There is no momentum, no air control curve, no friction model. That makes
//! client prediction exact for horizontal motion (the predicted position after
//! one tick is a pure function of the input) and removes an entire class of
//! "the server and client disagree about acceleration" bugs. It also matches the
//! arcade feel the game is going for, where stopping on a dime is a feature.

use crate::fixed::{Fixed, ONE_RAW, ZERO};
use crate::input::{clamp_to_unit, wish_direction, InputFrame};
use crate::tuning::Tuning;
use crate::vec::Vec3;
use crate::world_query::VoxelQuery;

/// Internal physics substeps per tick.
///
/// Four is the smallest count that keeps the jump apex within 1% of the design
/// value at 20 Hz. Raising it improves accuracy sub-linearly and costs
/// linearly; lowering it to 1 loses ~18% of the apex.
pub const SUBSTEPS: u32 = 4;

/// Distance below the feet that counts as "standing on something".
const GROUND_PROBE: Fixed = Fixed(ONE_RAW / 20);

/// Smallest horizontal gain that makes a step-up worth taking, in metres.
///
/// This must be **smaller than one substep of horizontal movement** or every
/// legitimate step is rejected as "no gain": at 4.5 m/s with four substeps a
/// substep moves 0.04 m, so a 0.05 m threshold silently disabled step-up
/// entirely. The real filter against a fake step is the settle-down check in
/// [`try_step_up`], which rejects a climb that lands no higher than it started.
const MIN_STEP_GAIN: Fixed = Fixed(ONE_RAW / 1000);

/// Gap used to keep a face that is exactly touching from counting as a hit.
const EPS: Fixed = Fixed(1);

/// The simulation timestep as a `Fixed`: 1/20 s truncated to Q16.16.
///
/// Truncation means a tick is 0.0499878 s rather than 0.05 s, a 0.02% drift.
/// That is deliberate: both client and server use this same constant, so the
/// drift is identical on both sides and invisible. Using a "more accurate" value
/// derived differently in two places would not be.
pub fn tick_dt() -> Fixed {
    Fixed(ONE_RAW / crate::time::TICK_HZ as i32)
}

/// Collision and movement parameters, resolved from tuning once.
#[derive(Clone, Copy, Debug)]
pub struct MoveConfig {
    /// Half the AABB's horizontal extent.
    pub half_width: Fixed,
    /// Full AABB height.
    pub height: Fixed,
    /// Maximum horizontal speed on land.
    pub speed: Fixed,
    /// Downward acceleration on land.
    pub gravity: Fixed,
    /// Maximum fall speed.
    pub terminal: Fixed,
    /// Take-off speed for a standing jump.
    pub jump_speed: Fixed,
    /// Tallest ledge that can be walked up.
    pub step_up: Fixed,
    /// Maximum horizontal speed while submerged.
    pub swim_speed: Fixed,
    /// Downward acceleration while submerged.
    pub swim_gravity: Fixed,
    /// Largest single sweep increment.
    pub max_substep: Fixed,
}

impl MoveConfig {
    /// Resolve from the embedded tuning table.
    pub fn from_tuning(t: &Tuning) -> MoveConfig {
        MoveConfig {
            half_width: t.player.radius,
            height: t.player.height,
            speed: t.player.speed,
            gravity: t.physics.gravity,
            terminal: t.physics.terminal_velocity,
            jump_speed: t.physics.jump_speed(),
            step_up: t.physics.step_up,
            swim_speed: t.physics.swim_speed,
            swim_gravity: t.physics.swim_gravity(),
            max_substep: t.physics.max_substep,
        }
    }

    /// The default configuration from `data/tuning.ron`.
    pub fn embedded() -> MoveConfig {
        MoveConfig::from_tuning(crate::tuning::embedded())
    }
}

/// One entity's kinematic state.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct MoveState {
    /// Feet centre. `y` is the bottom of the AABB, not its centre.
    pub pos: Vec3,
    /// Velocity in metres per second.
    pub vel: Vec3,
    /// True when the entity is standing on a solid block.
    pub on_ground: bool,
    /// True when the entity's waist is submerged.
    pub in_water: bool,
}

impl MoveState {
    /// A state at rest at `pos`.
    pub fn at(pos: Vec3) -> MoveState {
        MoveState {
            pos,
            ..Default::default()
        }
    }
}

/// What the player asked for this tick.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct MoveIntent {
    /// Strafe axis, `-1..=1`.
    pub move_x: i8,
    /// Forward axis, `-1..=1`.
    pub move_z: i8,
    /// Facing, in 1/256 turns.
    pub yaw: u8,
    /// Jump was pressed this tick (an edge, not a hold).
    pub jump: bool,
    /// Jump is being held, which sustains an upward swim.
    pub jump_held: bool,
}

impl MoveIntent {
    /// Derive intent from a wire frame and the previous tick's buttons.
    pub fn from_frame(frame: &InputFrame, previous_buttons: u8) -> MoveIntent {
        MoveIntent {
            move_x: frame.move_x,
            move_z: frame.move_z,
            yaw: frame.yaw,
            jump: frame.pressed(crate::input::buttons::JUMP, previous_buttons),
            jump_held: frame.held(crate::input::buttons::JUMP),
        }
    }
}

/// Per-tick outcomes, for audio, animation and debugging.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct MoveFlags {
    /// The entity landed on something this tick.
    pub landed: bool,
    /// The entity left the ground this tick.
    pub jumped: bool,
    /// A horizontal move was blocked by geometry.
    pub blocked: bool,
    /// The entity climbed a step.
    pub stepped_up: bool,
    /// The entity is standing on something at the end of the tick.
    pub on_ground: bool,
    /// The entity's waist is submerged at the end of the tick.
    pub in_water: bool,
}

impl MoveFlags {
    fn merge(&mut self, other: MoveFlags) {
        self.landed |= other.landed;
        self.jumped |= other.jumped;
        self.blocked |= other.blocked;
        self.stepped_up |= other.stepped_up;
    }
}

/// Advance one entity by `dt` seconds.
///
/// Deterministic: the result is a pure function of `(world, cfg, state, intent,
/// dt)`. There is no wall clock, no RNG and no floating point.
pub fn step<W: VoxelQuery + ?Sized>(
    world: &W,
    cfg: &MoveConfig,
    st: &mut MoveState,
    intent: MoveIntent,
    dt: Fixed,
) -> MoveFlags {
    let mut flags = MoveFlags::default();
    let dt_sub = dt.saturating_div(Fixed::from_int(SUBSTEPS as i32));

    // The jump edge stays live for the whole tick rather than only the first
    // substep, so a player who lands mid-tick still gets their jump. Without
    // this, jumping while falling onto a ledge eats the input roughly half the
    // time, which reads as an unresponsive game.
    let mut jump_pending = intent.jump;

    let dir = clamp_to_unit(wish_direction(intent.move_x, intent.move_z, intent.yaw));

    for _ in 0..SUBSTEPS {
        // --- environment ---------------------------------------------------
        st.in_water = world.water(
            st.pos.x.floor_int(),
            (st.pos.y + cfg.height.saturating_div(Fixed::from_int(2))).floor_int(),
            st.pos.z.floor_int(),
        );

        // --- horizontal velocity -------------------------------------------
        // Instant acceleration: see the module docs. `dir` is already clamped
        // to unit length, so this cannot exceed the configured speed by more
        // than fixed-point rounding.
        let speed = if st.in_water {
            cfg.swim_speed
        } else {
            cfg.speed
        };
        st.vel.x = dir.x * speed;
        st.vel.z = dir.z * speed;

        // --- vertical velocity ---------------------------------------------
        let gravity = if st.in_water {
            cfg.swim_gravity
        } else {
            cfg.gravity
        };
        st.vel.y -= gravity * dt_sub;

        if jump_pending && (st.on_ground || st.in_water) {
            st.vel.y = if st.in_water {
                cfg.swim_speed
            } else {
                cfg.jump_speed
            };
            st.on_ground = false;
            flags.jumped = true;
            jump_pending = false;
        } else if st.in_water && intent.jump_held {
            // Holding jump in water is a sustained 2.0 m/s rise, not a jump.
            if st.vel.y < cfg.swim_speed {
                st.vel.y = cfg.swim_speed;
            }
        }

        if st.vel.y < -cfg.terminal {
            st.vel.y = -cfg.terminal;
        }

        // --- integrate -----------------------------------------------------
        unstick(world, cfg, &mut st.pos);

        let delta = Vec3::new(st.vel.x * dt_sub, st.vel.y * dt_sub, st.vel.z * dt_sub);
        flags.merge(sweep(world, cfg, st, delta));
    }

    flags.on_ground = st.on_ground;
    flags.in_water = st.in_water;
    flags
}

/// Move `st` by `delta`, resolving collisions, and update its velocity.
fn sweep<W: VoxelQuery + ?Sized>(
    world: &W,
    cfg: &MoveConfig,
    st: &mut MoveState,
    delta: Vec3,
) -> MoveFlags {
    let mut flags = MoveFlags::default();
    let vel_before = st.vel;

    // Vertical first, so walking off a ledge begins falling on the same
    // substep rather than hovering for one tick.
    if move_axis(world, cfg, &mut st.pos, 1, delta.y) {
        if delta.y < ZERO {
            flags.landed = true;
        }
        st.vel.y = ZERO;
    }

    let blocked_x = move_axis(world, cfg, &mut st.pos, 0, delta.x);
    let blocked_z = move_axis(world, cfg, &mut st.pos, 2, delta.z);
    if blocked_x {
        st.vel.x = ZERO;
    }
    if blocked_z {
        st.vel.z = ZERO;
    }
    flags.blocked = blocked_x || blocked_z;

    // A blocked horizontal move on the ground is a step-up candidate. Try it
    // before giving up, so a 1-block lip does not require a jump.
    if flags.blocked && st.on_ground && try_step_up(world, cfg, st, delta) {
        flags.stepped_up = true;
        flags.blocked = false;
        st.vel.x = vel_before.x;
        st.vel.z = vel_before.z;
    }

    st.on_ground = st.vel.y <= ZERO && probe_ground(world, cfg, st.pos);
    flags
}

/// Move along one axis by `amount`, stopping at the first contact.
///
/// Returns `true` when the move was cut short. Uses a binary search to land
/// exactly on the contact surface, which is what lets an entity slide along a
/// wall instead of sticking to it.
fn move_axis<W: VoxelQuery + ?Sized>(
    world: &W,
    cfg: &MoveConfig,
    pos: &mut Vec3,
    axis: usize,
    amount: Fixed,
) -> bool {
    let mut remaining = amount;
    let mut guard = 0;

    while remaining.to_raw() != 0 && guard < 512 {
        guard += 1;

        let step = if remaining > cfg.max_substep {
            cfg.max_substep
        } else if remaining < -cfg.max_substep {
            -cfg.max_substep
        } else {
            remaining
        };

        let mut trial = *pos;
        trial.set_axis(axis, pos.axis(axis) + step);
        if !collides(world, cfg, trial) {
            *pos = trial;
            remaining -= step;
            continue;
        }

        // Binary-search the largest fraction of `step` that stays clear.
        let mut lo = ZERO;
        let mut hi = step;
        for _ in 0..12 {
            let mid = Fixed((lo.to_raw() + hi.to_raw()) / 2);
            let mut probe = *pos;
            probe.set_axis(axis, pos.axis(axis) + mid);
            if collides(world, cfg, probe) {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        pos.set_axis(axis, pos.axis(axis) + lo);
        return true;
    }

    false
}

/// Try to climb a one-block ledge instead of stopping dead.
fn try_step_up<W: VoxelQuery + ?Sized>(
    world: &W,
    cfg: &MoveConfig,
    st: &mut MoveState,
    delta: Vec3,
) -> bool {
    let saved = st.pos;

    let mut lifted = saved;
    lifted.y = saved.y + cfg.step_up;
    if collides(world, cfg, lifted) {
        return false;
    }

    let mut moved = lifted;
    move_axis(world, cfg, &mut moved, 0, delta.x);
    move_axis(world, cfg, &mut moved, 2, delta.z);

    if moved.horizontal_distance(saved) <= MIN_STEP_GAIN {
        return false;
    }

    // Settle back down onto the step. Moving down by exactly `step_up` lands on
    // the surface; a shorter step lands part-way, which is correct for slabs.
    let mut settled = moved;
    move_axis(world, cfg, &mut settled, 1, -cfg.step_up);
    if settled.y <= saved.y {
        return false;
    }

    st.pos = settled;
    st.on_ground = true;
    true
}

/// True when there is a solid block just under the feet.
fn probe_ground<W: VoxelQuery + ?Sized>(world: &W, cfg: &MoveConfig, pos: Vec3) -> bool {
    let mut probe = pos;
    probe.y = pos.y - GROUND_PROBE;
    collides(world, cfg, probe)
}

/// True when the AABB at `pos` overlaps any solid cell.
///
/// The cell range is computed with a one-unit epsilon on the far faces so a box
/// resting exactly on a surface does not register a hit against the block it is
/// standing on. Without that, every grounded entity would be permanently
/// colliding and could never move.
pub fn collides<W: VoxelQuery + ?Sized>(world: &W, cfg: &MoveConfig, pos: Vec3) -> bool {
    let x0 = (pos.x - cfg.half_width).floor_int();
    let x1 = (pos.x + cfg.half_width - EPS).floor_int();
    let y0 = pos.y.floor_int();
    let y1 = (pos.y + cfg.height - EPS).floor_int();
    let z0 = (pos.z - cfg.half_width).floor_int();
    let z1 = (pos.z + cfg.half_width - EPS).floor_int();

    if x1 < x0 || y1 < y0 || z1 < z0 {
        return false;
    }

    for y in y0..=y1 {
        for z in z0..=z1 {
            for x in x0..=x1 {
                if world.solid(x, y, z) {
                    return true;
                }
            }
        }
    }
    false
}

/// Sweep an AABB of the given size from `start` by `delta`.
///
/// Pure and state-free, so projectiles can reuse it: an arrow at 40 m/s moves
/// 2 m per tick, which is twice the width of the wall it is aimed at. Returns
/// the final position, which stops at the contact surface when the move is
/// blocked.
pub fn sweep_aabb<W: VoxelQuery + ?Sized>(
    world: &W,
    cfg: &MoveConfig,
    start: Vec3,
    delta: Vec3,
) -> Vec3 {
    let mut pos = start;
    unstick(world, cfg, &mut pos);
    move_axis(world, cfg, &mut pos, 1, delta.y);
    move_axis(world, cfg, &mut pos, 0, delta.x);
    move_axis(world, cfg, &mut pos, 2, delta.z);
    pos
}

/// Lift an entity out of geometry it is already inside.
///
/// This should never fire in normal play. It exists because the alternative to
/// a small, bounded correction is an entity trapped inside a block forever,
/// which is both unrecoverable and indistinguishable from a server bug.
fn unstick<W: VoxelQuery + ?Sized>(world: &W, cfg: &MoveConfig, pos: &mut Vec3) -> bool {
    if !collides(world, cfg, *pos) {
        return false;
    }
    for i in 1..=6 {
        let mut probe = *pos;
        probe.y = pos.y + Fixed::from_int(i);
        if !collides(world, cfg, probe) {
            *pos = probe;
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixed::Fixed;
    use crate::time::TICK_HZ;
    use crate::world_query::{EmptyWorld, VoxelGrid};

    fn cfg() -> MoveConfig {
        MoveConfig::embedded()
    }

    /// A world with a solid floor whose *walking surface* is at `y`.
    ///
    /// The block occupies `[top - 1, top]`, so a grounded entity's feet are at
    /// exactly `top`. Getting this convention wrong is how three of these tests
    /// were initially written with an off-by-one expectation.
    fn floor_world(top: i32) -> VoxelGrid {
        let mut w = VoxelGrid::new([-8, top - 5, -8], [32, 10, 32]);
        w.fill_layer(top - 1, true);
        w
    }

    fn intent_forward(yaw: u8) -> MoveIntent {
        MoveIntent {
            move_z: 1,
            yaw,
            ..Default::default()
        }
    }

    /// Deterministic pseudo-random driver.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0 >> 33
        }
        fn range(&mut self, lo: i32, hi: i32) -> i32 {
            lo + (self.next() % ((hi - lo + 1) as u64)) as i32
        }
    }

    #[test]
    fn falls_and_lands_on_the_floor() {
        let w = floor_world(0);
        let c = cfg();
        // The block occupies [-1, 0], so a resting entity's feet are at y = 0.
        let mut st = MoveState::at(Vec3::from_ints(0, 6, 0));
        for _ in 0..200 {
            step(&w, &c, &mut st, MoveIntent::default(), tick_dt());
        }
        assert!(st.on_ground, "entity never landed");
        assert!(
            (st.pos.y - Fixed::from_int(0)).abs().to_f64() < 0.01,
            "expected to rest at y = 0, got {:?}",
            st.pos.y
        );
        assert_eq!(st.vel.y, ZERO, "a resting entity has no vertical velocity");
    }

    #[test]
    fn terminal_velocity_is_enforced() {
        let w = EmptyWorld;
        let c = cfg();
        let mut st = MoveState::at(Vec3::from_ints(0, 500, 0));
        for _ in 0..400 {
            step(&w, &c, &mut st, MoveIntent::default(), tick_dt());
            assert!(
                st.vel.y >= -c.terminal,
                "fell faster than terminal velocity: {:?}",
                st.vel.y
            );
        }
        assert!(
            (st.vel.y + c.terminal).abs().to_f64() < 0.1,
            "should be at terminal velocity after 20 s of falling, got {:?}",
            st.vel.y
        );
    }

    #[test]
    fn jump_apex_is_about_1_25_blocks() {
        let w = floor_world(0);
        let c = cfg();
        let mut st = MoveState::at(Vec3::from_ints(0, 0, 0));
        // Settle first so `on_ground` is true.
        for _ in 0..4 {
            step(&w, &c, &mut st, MoveIntent::default(), tick_dt());
        }
        assert!(st.on_ground);

        let start = st.pos.y;
        let mut peak = start;
        let jump = MoveIntent {
            jump: true,
            jump_held: true,
            ..Default::default()
        };
        for i in 0..60 {
            let it = if i == 0 {
                jump
            } else {
                MoveIntent {
                    jump_held: true,
                    ..Default::default()
                }
            };
            step(&w, &c, &mut st, it, tick_dt());
            peak = peak.max(st.pos.y);
        }

        let apex = (peak - start).to_f64();
        assert!(
            (apex - 1.25).abs() < 0.06,
            "jump apex is {apex:.3} blocks, plan §4.8 says 1.25"
        );
        assert!(st.on_ground, "entity should have landed again");
    }

    #[test]
    fn step_up_climbs_one_block_but_not_two() {
        let c = cfg();

        // One-block lip: walking into it must climb it without a jump. The
        // assertion tracks the peak rather than the final position, because the
        // entity keeps walking across the top and drops off the far side.
        let mut w = floor_world(0);
        w.fill_box([2, 0, -8], [3, 0, 8], true);
        let mut st = MoveState::at(Vec3::from_ints(0, 0, 0));
        let mut peak = st.pos.y;
        for _ in 0..30 {
            step(&w, &c, &mut st, intent_forward(64), tick_dt());
            peak = peak.max(st.pos.y);
        }
        assert_eq!(peak, Fixed::from_int(1), "a 1-block lip should be climbed");
        assert!(
            st.pos.x > Fixed::from_int(3),
            "entity did not cross the lip, ended at x = {:?}",
            st.pos.x
        );

        // Two-block wall: must not climb, must not pass through.
        let mut w = floor_world(0);
        w.fill_box([2, 0, -8], [3, 1, 8], true);
        let mut st = MoveState::at(Vec3::from_ints(0, 0, 0));
        let mut peak = st.pos.y;
        for _ in 0..30 {
            step(&w, &c, &mut st, intent_forward(64), tick_dt());
            peak = peak.max(st.pos.y);
        }
        assert_eq!(
            peak,
            Fixed::from_int(0),
            "a 2-block wall must not be climbable"
        );
        assert!(
            st.pos.x < Fixed::from_int(2),
            "entity passed through a 2-block wall at x = {:?}",
            st.pos.x
        );
    }

    #[test]
    fn no_tunnelling_at_twenty_metres_per_second() {
        // 20 m/s at 20 Hz is 1 m per tick: exactly the width of the wall. A
        // naive "add then check" sweep would pass straight through it.
        let mut w = VoxelGrid::new([0, 0, 0], [40, 8, 8]);
        w.fill_box([10, 0, 0], [10, 7, 7], true);
        let c = cfg();

        let start = Vec3::from_ints(0, 4, 4);
        let delta = Vec3::from_ints(20, 0, 0);
        let end = sweep_aabb(&w, &c, start, delta);

        assert!(
            end.x < Fixed::from_int(10),
            "swept through the wall, ended at x = {:?}",
            end.x
        );
        assert!(
            end.x > Fixed::from_int(9),
            "stopped too far from the wall, ended at x = {:?}",
            end.x
        );
        assert!(
            !collides(&w, &c, end),
            "the resting position is inside geometry"
        );
    }

    #[test]
    fn cannot_enter_solid() {
        // 10,000 random inputs against a random 16^3 field. This is the test
        // that would catch an off-by-one in the cell range or a sweep that
        // resolves to the wrong side of a face.
        let mut rng = Lcg(0x5EED_1234);
        let mut w = VoxelGrid::cube(16);
        for y in 0..16 {
            for z in 0..16 {
                for x in 0..16 {
                    // ~12% solid, plus a guaranteed floor.
                    let solid = y == 0 || rng.next() % 100 < 12;
                    w.set_solid(x, y, z, solid);
                }
            }
        }

        let c = cfg();
        // Start on the floor in a column that is clear at y = 1.
        let mut start = None;
        for z in 1..15 {
            for x in 1..15 {
                if !w.solid(x, 1, z) && !w.solid(x, 2, z) {
                    start = Some(Vec3::from_ints(x, 1, z));
                    break;
                }
            }
            if start.is_some() {
                break;
            }
        }
        let mut st = MoveState::at(start.expect("a clear starting column"));

        for i in 0..10_000 {
            let it = MoveIntent {
                move_x: rng.range(-1, 1) as i8,
                move_z: rng.range(-1, 1) as i8,
                yaw: (rng.next() & 0xFF) as u8,
                jump: rng.next() % 5 == 0,
                jump_held: rng.next() % 3 == 0,
            };
            step(&w, &c, &mut st, it, tick_dt());
            assert!(
                !collides(&w, &c, st.pos),
                "entity entered solid geometry on iteration {i} at {:?}",
                st.pos
            );
        }
    }

    #[test]
    fn t1_speed_hack_is_clamped() {
        // The horizontal velocity is `wish_direction * speed`, and the
        // direction is clamped to unit length, so no input sequence can exceed
        // `speed`. The tolerance covers Q16.16 rounding in the sin/cos table
        // (about 6e-5 m/s), not a logic allowance.
        let w = EmptyWorld;
        let c = cfg();
        let tolerance = Fixed::from_raw(8);
        let mut rng = Lcg(99);

        for _ in 0..2_000 {
            let it = MoveIntent {
                move_x: rng.range(-1, 1) as i8,
                move_z: rng.range(-1, 1) as i8,
                yaw: (rng.next() & 0xFF) as u8,
                jump: false,
                jump_held: false,
            };
            let mut st = MoveState::at(Vec3::from_ints(0, 40, 0));
            step(&w, &c, &mut st, it, tick_dt());
            let h = st.vel.horizontal_length();
            assert!(
                h <= c.speed + tolerance,
                "horizontal speed {h:?} exceeds the cap {:?} for input ({}, {}) at yaw {}",
                c.speed,
                it.move_x,
                it.move_z,
                it.yaw
            );
        }
    }

    #[test]
    fn swimming_rises_slower_than_walking_and_sinks_gently() {
        let c = cfg();
        let mut w = VoxelGrid::new([0, 0, 0], [16, 16, 16]);
        // A pool from y = 0 to y = 8, with a solid floor at y = 0.
        w.fill_layer(0, true);
        for y in 1..8 {
            for z in 0..16 {
                for x in 0..16 {
                    w.set_water(x, y, z, true);
                }
            }
        }

        // Holding jump rises at the swim speed, not the walk speed.
        let mut st = MoveState::at(Vec3::from_ints(8, 3, 8));
        for _ in 0..10 {
            step(
                &w,
                &c,
                &mut st,
                MoveIntent {
                    jump_held: true,
                    ..Default::default()
                },
                tick_dt(),
            );
        }
        assert!(st.in_water, "waist should be submerged");
        assert!(
            st.pos.y > Fixed::from_int(3),
            "holding jump in water must rise, ended at {:?}",
            st.pos.y
        );

        // Not holding jump sinks, but at `swim_gravity_mult` of the rate, not
        // free fall. Compare against the same drop in open air rather than
        // asserting a magic number, so the test tracks the tuning.
        let mut sunk = MoveState::at(Vec3::from_ints(8, 6, 8));
        for _ in 0..10 {
            step(&w, &c, &mut sunk, MoveIntent::default(), tick_dt());
        }
        let mut fell = MoveState::at(Vec3::from_ints(8, 6, 8));
        for _ in 0..10 {
            step(&EmptyWorld, &c, &mut fell, MoveIntent::default(), tick_dt());
        }
        assert!(
            sunk.vel.y > fell.vel.y * Fixed::from_int(2),
            "sinking ({:?}) should be far slower than falling ({:?})",
            sunk.vel.y,
            fell.vel.y
        );
        assert!(sunk.vel.y < ZERO, "an idle swimmer should still sink");
    }

    #[test]
    fn walking_off_a_ledge_starts_falling() {
        let c = cfg();
        let mut w = VoxelGrid::new([0, 0, 0], [32, 16, 8]);
        w.fill_box([0, 0, 0], [3, 0, 7], true);

        let mut st = MoveState::at(Vec3::from_ints(2, 0, 4));
        for _ in 0..4 {
            step(&w, &c, &mut st, MoveIntent::default(), tick_dt());
        }
        assert!(st.on_ground);

        for _ in 0..40 {
            step(&w, &c, &mut st, intent_forward(64), tick_dt());
        }
        assert!(!st.on_ground, "entity should be falling past the ledge");
        assert!(st.pos.y < ZERO, "entity should be below the ledge");
    }

    #[test]
    fn blocked_movement_does_not_stick_to_the_wall() {
        // Sliding along a wall must preserve the unblocked axis. Snapping back
        // to the previous position is the classic sticky-wall bug.
        let c = cfg();
        let mut w = floor_world(0);
        w.fill_box([3, 0, -8], [3, 3, 8], true);

        // Walk diagonally into the wall: +X blocked, +Z free. Yaw 32 is 45
        // degrees, so forward is (0.707, 0, 0.707) and both axes get input.
        let mut st = MoveState::at(Vec3::from_ints(0, 0, 0));
        let it = MoveIntent {
            move_z: 1,
            yaw: 32,
            ..Default::default()
        };
        let z0 = st.pos.z;
        for _ in 0..20 {
            step(&w, &c, &mut st, it, tick_dt());
        }
        assert!(
            st.pos.z > z0 + Fixed::from_int(1),
            "entity did not slide along the wall (z {:?} -> {:?})",
            z0,
            st.pos.z
        );
        assert!(st.pos.x < Fixed::from_int(3), "entity entered the wall");
    }

    #[test]
    fn tick_dt_matches_the_tick_rate() {
        let dt = tick_dt();
        assert_eq!(dt.to_raw(), ONE_RAW / TICK_HZ as i32);
        assert!((dt.to_f64() - 0.05).abs() < 1e-4);
    }

    #[test]
    fn an_entity_inside_geometry_is_lifted_out() {
        let c = cfg();
        let mut w = floor_world(0);
        w.fill_box([0, 0, 0], [1, 1, 1], true);
        let mut st = MoveState::at(Vec3::from_ints(0, 0, 0));
        assert!(
            collides(&w, &c, st.pos),
            "test setup must start inside a block"
        );
        step(&w, &c, &mut st, MoveIntent::default(), tick_dt());
        assert!(
            !collides(&w, &c, st.pos),
            "entity was not lifted out of geometry, still at {:?}",
            st.pos
        );
    }
}
