//! Combat: melee cones, projectiles, area damage, and damage events.
//!
//! # Why combat emits events instead of applying damage
//!
//! Every hit produces a [`DamageEvent`] pushed into a per-tick vector. Nothing
//! in this module touches presentation, and nothing here decides what happens
//! when HP reaches zero — that is the match loop's job (T-013/T-014).
//!
//! The split is not ceremony. Three consumers need the same hit in three
//! different forms: the renderer wants a hit flash and a damage number, the
//! audio layer wants a material-dependent thud, and the match loop wants HP and
//! death. If combat applied damage directly, the first two would have to
//! re-derive what happened from a changed HP value, and the third would have to
//! guess at ordering. Emitting once and applying once makes the order explicit.
//!
//! # What is deliberately simple
//!
//! * **No friendly fire.** A hit ignores anyone on the attacker's team, and a
//!   projectile ignores its owner. §4.8 does not specify it, and in a 3v3 with
//!   area damage, friendly fire is a griefing tool rather than a skill ceiling.
//! * **Melee hits every enemy in the cone**, not just the nearest. A 45° cone
//!   at 2.6 m is narrow enough that this reads as "one swing, everything in
//!   front of me" rather than as an area attack.
//! * **Reach is measured to the target's centre**, not to its surface. Reach
//!   numbers in §4.8 are whole metres; measuring to a surface would make a
//!   2.6 m sword hit at 2.9 m on a wide target, and the stated numbers would
//!   stop meaning anything.
//! * **Projectiles travel in straight lines with no gravity** (§4.8, v1), so
//!   client prediction only has to integrate one vector.

use crate::fixed::{cos_yaw, Fixed, HALF, ONE, ZERO};
use crate::ids::{EntityId, TeamId};
use crate::vec::Vec3;
use crate::world_query::VoxelQuery;

/// Half-angle of the melee cone, in yaw units.
///
/// 16 units is 1/16 of a turn, i.e. 22.5°, so the cone is 45° wide (§4.8).
/// Expressed in yaw units rather than degrees because that is what the sin/cos
/// table indexes, which makes the cone edge exactly `cos_yaw(16)` instead of a
/// rounded decimal.
pub const MELEE_HALF_ANGLE_YAW: u8 = 16;

/// Distance between terrain samples along a projectile's path, in metres.
///
/// A projectile moves up to 2 m per tick at 40 m/s, which is wide enough to
/// tunnel a wall if only the endpoints were tested. Sampling at 0.2 m gives ten
/// checks per tick and cannot miss a 1 m block except by grazing a corner; the
/// failure mode is a hit that should have happened, not a wall that vanished.
pub const TERRAIN_SAMPLE: Fixed = Fixed(13_107); // 0.2 m

/// Below this, a ray is treated as parallel to an axis plane.
const PARALLEL_EPS: Fixed = Fixed(16); // ~2.4e-4 m

/// How an attack delivers its damage.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AttackProfile {
    /// Instant, this tick, inside a cone.
    Melee,
    /// A travelling hit along a straight line.
    Projectile {
        /// Metres per second.
        speed: Fixed,
    },
    /// A projectile that damages everything near its impact point.
    Burst {
        /// Metres per second.
        speed: Fixed,
        /// Damage radius around the impact point, in metres.
        radius: Fixed,
    },
}

impl AttackProfile {
    /// True when the attack is resolved immediately rather than travelling.
    pub fn is_melee(self) -> bool {
        matches!(self, AttackProfile::Melee)
    }

    /// Projectile speed, or zero for melee.
    pub fn speed(self) -> Fixed {
        match self {
            AttackProfile::Melee => ZERO,
            AttackProfile::Projectile { speed } | AttackProfile::Burst { speed, .. } => speed,
        }
    }

    /// Burst radius, or zero for a single-target hit.
    pub fn burst_radius(self) -> Fixed {
        match self {
            AttackProfile::Burst { radius, .. } => radius,
            _ => ZERO,
        }
    }
}

/// One weapon's numbers (§4.8 for players, §4.6 for buddies).
///
/// Built by [`crate::tuning`] from `data/tuning.ron`, which is the only place a
/// decimal may appear. Combat itself is integer-only.
#[derive(Clone, Copy, Debug)]
pub struct WeaponStats {
    /// Damage per hit, in HP.
    pub damage: i32,
    /// Minimum ticks between two attacks. Integral by construction: a cooldown
    /// of 0.45 s is 9 ticks at 20 Hz, and rounding it to a whole tick at load
    /// is what stops two clients disagreeing about whether the third swing is
    /// legal this tick.
    pub cooldown_ticks: u32,
    /// Maximum range in metres: reach for melee, flight distance for a
    /// projectile.
    pub reach: Fixed,
    /// How the damage is delivered.
    pub profile: AttackProfile,
}

impl WeaponStats {
    /// Damage per second, ignoring travel time.
    ///
    /// Uses the cooldown in ticks rather than the authored seconds, so this is
    /// the rate the simulation actually produces.
    pub fn dps(&self) -> Fixed {
        if self.cooldown_ticks == 0 {
            return ZERO;
        }
        let ticks = Fixed::from_int(self.cooldown_ticks as i32);
        Fixed::from_int(self.damage) * Fixed::from_int(crate::time::TICK_HZ as i32) / ticks
    }
}

/// What produced a hit, for audio and for the kill feed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DamageKind {
    /// A melee cone.
    Melee,
    /// A direct projectile hit.
    Projectile,
    /// Area damage around an impact.
    Burst,
    /// A siege structure hitting a core. Produced by T-013, listed here so the
    /// event type does not have to change.
    Structure,
}

/// One hit, ready to be applied by the match loop.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DamageEvent {
    /// Who took the damage.
    pub target: EntityId,
    /// Who dealt it. [`EntityId::NONE`] for environmental damage.
    pub source: EntityId,
    /// The attacker's team, so the kill feed and the scoreboard do not have to
    /// look it up from an entity that may already be gone.
    pub source_team: TeamId,
    /// Damage in HP, before any core multiplier.
    pub amount: i32,
    /// How it was delivered.
    pub kind: DamageKind,
    /// World position of the impact, for the hit flash and the damage number.
    pub at: Vec3,
}

/// An axis-aligned box, used for hit tests.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Aabb {
    /// Minimum corner.
    pub min: Vec3,
    /// Maximum corner.
    pub max: Vec3,
}

/// The parts of an entity that combat needs.
///
/// A view rather than a borrow of the real entity storage: combat must stay
/// usable from the client's prediction, the server's authority, and a unit test
/// with three literals, and none of those share a storage type yet.
#[derive(Clone, Copy, Debug)]
pub struct Combatant {
    /// Stable handle.
    pub id: EntityId,
    /// Team, for friendly-fire filtering.
    pub team: TeamId,
    /// Feet centre. `y` is the bottom of the box.
    pub pos: Vec3,
    /// Half the box's horizontal extent.
    pub half_width: Fixed,
    /// Full box height.
    pub height: Fixed,
    /// Current HP. Combat never writes this; it only reads it to skip the dead.
    pub hp: i32,
    /// False once the entity has been removed from play.
    pub alive: bool,
}

impl Combatant {
    /// A player-sized combatant at `pos`.
    pub fn player(id: EntityId, team: TeamId, pos: Vec3) -> Combatant {
        let t = crate::tuning::embedded();
        Combatant {
            id,
            team,
            pos,
            half_width: t.player.radius,
            height: t.player.height,
            hp: t.player.hp,
            alive: true,
        }
    }

    /// The entity's box in world space.
    pub fn aabb(&self) -> Aabb {
        Aabb {
            min: Vec3::new(
                self.pos.x - self.half_width,
                self.pos.y,
                self.pos.z - self.half_width,
            ),
            max: Vec3::new(
                self.pos.x + self.half_width,
                self.pos.y + self.height,
                self.pos.z + self.half_width,
            ),
        }
    }

    /// The point a hit is aimed at: chest height, box centre.
    pub fn centre(&self) -> Vec3 {
        self.pos + Vec3::new(ZERO, self.height * HALF, ZERO)
    }
}

/// Where along a segment `[from, to]` it first enters `box`.
///
/// The slab method: for each axis, compute the interval of `t` for which the
/// point is between the two planes, then intersect the three intervals. Returns
/// a fraction in `0..=1`, or `None` when the segment misses.
///
/// This is exact, so a projectile cannot tunnel: the test is against the whole
/// swept segment, not against the endpoint.
pub fn segment_aabb(from: Vec3, to: Vec3, box_: Aabb) -> Option<Fixed> {
    let mut enter = ZERO;
    let mut exit = ONE;

    for axis in 0..3 {
        let start = from.axis(axis);
        let delta = to.axis(axis) - start;
        let lo = box_.min.axis(axis);
        let hi = box_.max.axis(axis);

        if delta.abs() <= PARALLEL_EPS {
            // Parallel to this pair of planes: either always inside or never.
            if start < lo || start > hi {
                return None;
            }
            continue;
        }

        let mut t0 = (lo - start) / delta;
        let mut t1 = (hi - start) / delta;
        if t0 > t1 {
            core::mem::swap(&mut t0, &mut t1);
        }
        if t0 > enter {
            enter = t0;
        }
        if t1 < exit {
            exit = t1;
        }
        if enter > exit {
            return None;
        }
    }

    Some(enter)
}

/// Resolve one melee swing.
///
/// `eye` is the attacker's eye position (feet plus most of their height) and
/// `look` must be a unit vector — [`crate::input::look_direction`] produces one.
/// Hits are appended to `out`; the return value is how many landed, which the
/// caller uses for hitstop and for the swing sound.
///
/// Deterministic: iterates `targets` in slice order and emits in that order, so
/// two clients that build the same slice produce the same event vector.
pub fn melee(
    attacker: EntityId,
    team: TeamId,
    eye: Vec3,
    look: Vec3,
    weapon: &WeaponStats,
    targets: &[Combatant],
    out: &mut Vec<DamageEvent>,
) -> usize {
    debug_assert!(
        {
            // Written in raw units rather than via `to_f64`: this is
            // authoritative code, and `tests/no_float.rs` fails the build if a
            // float type appears anywhere in `sim` outside the two conversion
            // boundaries.
            let len_sq = look.length_sq().to_raw();
            len_sq > 59_000 && len_sq < 72_000
        },
        "melee needs a unit look vector, got {look:?}"
    );
    let cone = cos_yaw(MELEE_HALF_ANGLE_YAW);
    let mut hits = 0;

    for target in targets {
        if !can_be_hit(target, attacker, team) {
            continue;
        }
        let delta = target.centre() - eye;
        let distance = delta.length();
        // `reach + half_width` measures to the near face, so a 2.6 m sword
        // connects with a 0.3 m radius target whose centre is 2.8 m away.
        if distance > weapon.reach + target.half_width {
            continue;
        }
        // The cone is skipped at contact range. Two bodies that overlap have a
        // direction between them that is dominated by whichever centimetre they
        // happen to be offset by — a point-blank swing would then miss as often
        // as it hit, and *bodies do overlap*, because `sim` has no
        // player-versus-player collision. Inside the target's own half-width the
        // attacker is within its silhouette, and aim cannot be wrong.
        if distance > target.half_width {
            let along = look.dot(delta.normalize());
            if along < cone {
                continue;
            }
        }
        out.push(DamageEvent {
            target: target.id,
            source: attacker,
            source_team: team,
            amount: weapon.damage,
            kind: DamageKind::Melee,
            at: target.centre(),
        });
        hits += 1;
    }
    hits
}

/// True when `target` may be damaged by `attacker` of `team`.
///
/// One predicate, used by melee, projectiles and bursts, so the friendly-fire
/// and dead-entity rules cannot drift apart between attack types.
#[inline]
fn can_be_hit(target: &Combatant, attacker: EntityId, team: TeamId) -> bool {
    target.alive && target.id != attacker && target.team != team
}

/// A projectile in flight.
#[derive(Clone, Copy, Debug)]
pub struct Projectile {
    /// Handle, allocated by the caller so the renderer can interpolate it.
    pub id: EntityId,
    /// Who fired it; never damaged by its own projectile.
    pub owner: EntityId,
    /// The firing team.
    pub team: TeamId,
    /// Current position.
    pub pos: Vec3,
    /// Unit direction of travel.
    pub dir: Vec3,
    /// Metres per second.
    pub speed: Fixed,
    /// Damage on impact.
    pub damage: i32,
    /// Radius of the area damage on impact; zero for a single-target hit.
    pub burst_radius: Fixed,
    /// Metres left before it despawns.
    pub remaining: Fixed,
}

/// What happened to a projectile this tick.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProjectileOutcome {
    /// Still travelling.
    Flying,
    /// Ran out of range without hitting anything.
    Expired,
    /// Hit something. `target` is `None` when it hit terrain.
    Impact {
        /// The entity struck, if any.
        target: Option<EntityId>,
        /// World position of the impact.
        at: Vec3,
    },
}

impl Projectile {
    /// Spawn a projectile for `weapon`, or `None` if the weapon is melee.
    pub fn from_weapon(
        id: EntityId,
        owner: EntityId,
        team: TeamId,
        origin: Vec3,
        dir: Vec3,
        weapon: &WeaponStats,
    ) -> Option<Projectile> {
        if weapon.profile.is_melee() {
            return None;
        }
        Some(Projectile {
            id,
            owner,
            team,
            pos: origin,
            dir,
            speed: weapon.profile.speed(),
            damage: weapon.damage,
            burst_radius: weapon.profile.burst_radius(),
            remaining: weapon.reach,
        })
    }

    /// Advance one tick.
    ///
    /// Order of checks matters and is fixed: terrain first, then entities
    /// nearest-first. Terrain first means a projectile cannot hit someone
    /// standing behind a wall, which is the only ordering that is defensible
    /// when a player is looking at both.
    pub fn step<W: VoxelQuery + ?Sized>(
        &mut self,
        world: &W,
        dt: Fixed,
        targets: &[Combatant],
        out: &mut Vec<DamageEvent>,
    ) -> ProjectileOutcome {
        let travel = (self.speed * dt).min(self.remaining);
        if travel <= ZERO {
            return ProjectileOutcome::Expired;
        }
        let from = self.pos;
        let to = from + self.dir * travel;
        let span = to - from;

        if let Some(t) = self.terrain_hit(world, from, to) {
            let at = from + span * t;
            self.pos = at;
            self.remaining = ZERO;
            self.resolve(at, None, targets, out);
            return ProjectileOutcome::Impact { target: None, at };
        }

        let mut nearest: Option<(Fixed, EntityId)> = None;
        for target in targets {
            if !can_be_hit(target, self.owner, self.team) {
                continue;
            }
            if let Some(t) = segment_aabb(from, to, target.aabb()) {
                let better = match nearest {
                    None => true,
                    Some((best, _)) => t < best,
                };
                if better {
                    nearest = Some((t, target.id));
                }
            }
        }

        if let Some((t, id)) = nearest {
            let at = from + span * t;
            self.pos = at;
            self.remaining = ZERO;
            self.resolve(at, Some(id), targets, out);
            return ProjectileOutcome::Impact {
                target: Some(id),
                at,
            };
        }

        self.pos = to;
        self.remaining -= travel;
        if self.remaining <= ZERO {
            ProjectileOutcome::Expired
        } else {
            ProjectileOutcome::Flying
        }
    }

    /// Emit damage for an impact at `at`.
    ///
    /// A burst emits once per enemy in range — including the entity it struck
    /// directly, which is inside radius zero by definition. A plain projectile
    /// emits for the struck entity only.
    fn resolve(
        &self,
        at: Vec3,
        direct: Option<EntityId>,
        targets: &[Combatant],
        out: &mut Vec<DamageEvent>,
    ) {
        let radius = self.burst_radius;
        if radius <= ZERO {
            let Some(id) = direct else { return };
            out.push(DamageEvent {
                target: id,
                source: self.owner,
                source_team: self.team,
                amount: self.damage,
                kind: DamageKind::Projectile,
                at,
            });
            return;
        }
        for target in targets {
            if !can_be_hit(target, self.owner, self.team) {
                continue;
            }
            if target.centre().distance(at) > radius + target.half_width {
                continue;
            }
            out.push(DamageEvent {
                target: target.id,
                source: self.owner,
                source_team: self.team,
                amount: self.damage,
                kind: DamageKind::Burst,
                at: target.centre(),
            });
        }
    }

    /// First fraction along `[from, to]` where the path meets a solid cell.
    ///
    /// Sampled rather than DDA-swept. At 40 m/s a projectile covers 2 m per
    /// tick; ten samples across that span cannot miss a 1 m block except by
    /// clipping a corner by less than 0.2 m. A DDA would be exact, and is the
    /// upgrade if a playtester ever reports an arrow passing through a wall —
    /// the interface would not change.
    fn terrain_hit<W: VoxelQuery + ?Sized>(
        &self,
        world: &W,
        from: Vec3,
        to: Vec3,
    ) -> Option<Fixed> {
        let span = to - from;
        let length = span.length();
        if length <= ZERO {
            return None;
        }
        let steps = (length / TERRAIN_SAMPLE).round_int().max(1);
        for i in 1..=steps {
            let t = Fixed::from_int(i) / Fixed::from_int(steps);
            let p = from + span * t;
            let cell = p.floor_to_ints();
            if world.solid(cell[0], cell[1], cell[2]) {
                return Some(t);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::TICK_HZ;
    use crate::tuning;
    use crate::world_query::{EmptyWorld, VoxelGrid};

    fn dt() -> Fixed {
        crate::movement::tick_dt()
    }

    /// A player-sized combatant with an explicit id.
    ///
    /// Ids are explicit rather than derived from the team, because two enemies
    /// on the same team must still be two entities — deriving the id from the
    /// team made `mage_hits_3_clustered` pass its count assertion while
    /// reporting all three hits against one id.
    fn mk(id: u32, team: u8, x: i32, y: i32, z: i32) -> Combatant {
        Combatant::player(EntityId::new(id, 0), TeamId(team), Vec3::from_ints(x, y, z))
    }

    /// The attacker: id 1, team 0, at the origin.
    fn hero() -> Combatant {
        mk(1, 0, 0, 0, 0)
    }

    /// An enemy standing on the ground at `(x, z)`.
    fn foe(id: u32, x: i32, z: i32) -> Combatant {
        mk(id, 1, x, 0, z)
    }

    /// The eye, taken as the chest centre so cone tests are purely horizontal.
    fn eye_of(c: &Combatant) -> Vec3 {
        c.centre()
    }

    /// Facing +Z, level.
    fn look_forward() -> Vec3 {
        Vec3::new(ZERO, ZERO, ONE)
    }

    #[test]
    fn sword_ttk_is_1_8s() {
        let t = tuning::embedded();
        let sword = &t.weapons.sword;
        assert_eq!(sword.damage, 25, "§4.8");
        assert_eq!(sword.cooldown_ticks, 9, "0.45 s at 20 Hz");

        // 100 HP / 25 per hit = 4 hits, and 4 x 0.45 s = 1.8 s (§4.8).
        let hits = (t.player.hp + sword.damage - 1) / sword.damage;
        assert_eq!(hits, 4);
        let ttk_ticks = sword.cooldown_ticks * hits as u32;
        assert_eq!(ttk_ticks, 36);
        let ttk = Fixed::from_int(ttk_ticks as i32) / Fixed::from_int(TICK_HZ as i32);
        assert!(
            (ttk.to_f64() - 1.8).abs() < 1e-3,
            "sword TTK is {ttk:?}, §4.8 says 1.8 s"
        );

        // The continuous form agrees for the sword; for fists it does not
        // (13 hits at 0.5 s is 6.5 s, against the plan's 6.3 s steady-state
        // figure), which is why the discrete number is the one that ships.
        assert!((sword.dps().to_f64() - 55.0).abs() < 1.0);
    }

    #[test]
    fn every_player_weapon_matches_the_plan() {
        let w = &tuning::embedded().weapons;
        assert_eq!(w.fists.damage, 8);
        assert_eq!(w.fists.cooldown_ticks, 10);
        assert!((w.fists.reach.to_f64() - 2.2).abs() < 1e-3);
        assert_eq!(w.sword.damage, 25);
        assert!((w.sword.reach.to_f64() - 2.6).abs() < 1e-3);
        assert_eq!(w.spear.damage, 20);
        assert_eq!(w.spear.cooldown_ticks, 11);
        assert!((w.spear.reach.to_f64() - 3.6).abs() < 1e-3);
        assert_eq!(w.bow.damage, 30);
        assert!((w.bow.profile.speed().to_f64() - 40.0).abs() < 1e-3);
        // The staff line in §4.8 (40 dmg) has no owner in the recipe table;
        // the only thing that casts is the Mage, whose §4.6 row says 22.
        assert_eq!(w.mage_bolt.damage, 22);
        assert!((w.mage_bolt.profile.burst_radius().to_f64() - 2.5).abs() < 1e-3);
    }

    #[test]
    fn melee_hits_in_front_and_not_behind() {
        let sword = &tuning::embedded().weapons.sword;
        let a = hero();
        let front = foe(10, 0, 2);
        let behind = foe(11, 0, -2);
        let mut out = Vec::new();
        let hits = melee(
            a.id,
            a.team,
            eye_of(&a),
            look_forward(),
            sword,
            &[front, behind],
            &mut out,
        );
        assert_eq!(hits, 1);
        assert_eq!(out[0].target, front.id);
        assert_eq!(out[0].amount, 25);
        assert_eq!(out[0].kind, DamageKind::Melee);
        assert_eq!(out[0].source, a.id);
        assert_eq!(out[0].source_team, a.team);
    }

    #[test]
    fn melee_respects_reach() {
        let sword = &tuning::embedded().weapons.sword;
        let a = hero();
        let mut out = Vec::new();
        // 2.6 m reach plus a 0.3 m radius: 2 m connects, 3 m does not.
        assert_eq!(
            melee(
                a.id,
                a.team,
                eye_of(&a),
                look_forward(),
                sword,
                &[foe(10, 0, 2)],
                &mut out
            ),
            1
        );
        out.clear();
        assert_eq!(
            melee(
                a.id,
                a.team,
                eye_of(&a),
                look_forward(),
                sword,
                &[foe(10, 0, 3)],
                &mut out
            ),
            0,
            "3 m is outside a 2.6 m reach"
        );
    }

    #[test]
    fn melee_cone_is_45_degrees_wide() {
        // The spear, so reach is not the thing under test: 3.6 m + 0.3 m radius.
        let spear = &tuning::embedded().weapons.spear;
        let a = hero();
        let mut out = Vec::new();

        // atan(1/3) = 18.4 degrees: inside a 22.5 degree half-angle.
        assert_eq!(
            melee(
                a.id,
                a.team,
                eye_of(&a),
                look_forward(),
                spear,
                &[foe(10, 1, 3)],
                &mut out
            ),
            1,
            "18.4 degrees is inside the cone"
        );

        // atan(1/2) = 26.6 degrees: outside, even though it is closer.
        out.clear();
        assert_eq!(
            melee(
                a.id,
                a.team,
                eye_of(&a),
                look_forward(),
                spear,
                &[foe(10, 1, 2)],
                &mut out
            ),
            0,
            "26.6 degrees is outside a 22.5 degree half-angle"
        );

        // Straight ahead, and well inside.
        out.clear();
        assert_eq!(
            melee(
                a.id,
                a.team,
                eye_of(&a),
                look_forward(),
                spear,
                &[foe(10, 0, 2)],
                &mut out
            ),
            1
        );
    }

    #[test]
    fn a_point_blank_swing_always_connects() {
        // Two bodies that walk into each other end up overlapping, because
        // `sim` has no player-versus-player collision. At that distance the
        // direction between them is noise, so the cone test used to reject the
        // hit and two players standing on top of each other could not damage
        // each other at all — the most common melee situation in the game.
        let sword = &tuning::embedded().weapons.sword;
        let a = hero();
        let mut out = Vec::new();

        // Overlapping: 5 cm apart, and the attacker facing the wrong way.
        let touching = foe(10, 0, 0);
        let hits = melee(
            a.id,
            a.team,
            eye_of(&a),
            Vec3::new(ZERO, ZERO, -crate::fixed::ONE),
            sword,
            &[touching],
            &mut out,
        );
        assert_eq!(hits, 1, "a point-blank swing must land regardless of aim");

        // Just outside the silhouette the cone applies again: 1 m behind the
        // attacker is out of reach of the aim, not of the arm.
        out.clear();
        let behind = foe(11, 0, -1);
        assert_eq!(
            melee(
                a.id,
                a.team,
                eye_of(&a),
                look_forward(),
                sword,
                &[behind],
                &mut out
            ),
            0,
            "1 m behind must still be rejected by the cone"
        );
    }

    #[test]
    fn no_friendly_fire_and_no_self_hits() {
        let sword = &tuning::embedded().weapons.sword;
        let a = hero();
        let ally = mk(2, 0, 0, 0, 1);
        let enemy = foe(10, 0, 2);
        let mut out = Vec::new();
        let hits = melee(
            a.id,
            a.team,
            eye_of(&a),
            look_forward(),
            sword,
            &[a, ally, enemy],
            &mut out,
        );
        assert_eq!(hits, 1, "only the enemy");
        assert_eq!(out[0].target, enemy.id);
    }

    #[test]
    fn no_damage_to_dead_entities() {
        let t = tuning::embedded();
        let a = hero();

        let mut corpse = foe(10, 0, 1);
        corpse.alive = false;
        corpse.hp = 0;
        let mut out = Vec::new();
        assert_eq!(
            melee(
                a.id,
                a.team,
                eye_of(&a),
                look_forward(),
                &t.weapons.sword,
                &[corpse],
                &mut out
            ),
            0,
            "a corpse cannot be hit by melee"
        );
        assert!(out.is_empty());

        // The same must hold for projectiles.
        let mut arrow = Projectile::from_weapon(
            EntityId::new(99, 0),
            a.id,
            a.team,
            Vec3::from_ints(0, 1, 0),
            look_forward(),
            &t.weapons.bow,
        )
        .expect("bow is a projectile");
        let outcome = arrow.step(&EmptyWorld, dt(), &[corpse], &mut out);
        assert!(out.is_empty(), "a corpse cannot be hit by a projectile");
        assert!(matches!(
            outcome,
            ProjectileOutcome::Flying | ProjectileOutcome::Expired
        ));
    }

    #[test]
    fn arrow_hits_at_14m() {
        let bow = &tuning::embedded().weapons.bow;
        let a = hero();
        let mut arrow = Projectile::from_weapon(
            EntityId::new(50, 0),
            a.id,
            a.team,
            Vec3::from_ints(0, 1, 0),
            look_forward(),
            bow,
        )
        .expect("bow is a projectile");

        let target = foe(10, 0, 14);
        let mut out = Vec::new();
        let mut hit = None;
        for tick in 0..30u32 {
            match arrow.step(&EmptyWorld, dt(), &[target], &mut out) {
                ProjectileOutcome::Flying => {}
                ProjectileOutcome::Expired => panic!("expired at tick {tick} without a hit"),
                ProjectileOutcome::Impact { target: t, at } => {
                    hit = Some((tick, t, at));
                    break;
                }
            }
        }

        let (tick, who, at) = hit.expect("the arrow must hit a target 14 m away");
        assert_eq!(who, Some(target.id));
        assert_eq!(out.len(), 1, "one arrow, one hit");
        assert_eq!(out[0].amount, 30);
        assert_eq!(out[0].kind, DamageKind::Projectile);
        // 14 m at 40 m/s is 7 ticks of flight, so the hit lands on tick 6
        // (0-indexed) against the target's near face at 13.7 m.
        assert_eq!(tick, 6, "14 m at 40 m/s is 7 ticks, 0-indexed");
        assert!(
            (at.z.to_f64() - 13.7).abs() < 0.05,
            "should stop at the near face, got {:.3}",
            at.z.to_f64()
        );
    }

    #[test]
    fn arrow_stops_at_the_first_of_two_targets() {
        let bow = &tuning::embedded().weapons.bow;
        let a = hero();
        let mut arrow = Projectile::from_weapon(
            EntityId::new(50, 0),
            a.id,
            a.team,
            Vec3::from_ints(0, 1, 0),
            look_forward(),
            bow,
        )
        .unwrap();
        let near = foe(10, 0, 6);
        let far = foe(11, 0, 12);
        let mut out = Vec::new();
        for _ in 0..30 {
            if let ProjectileOutcome::Impact { target, .. } =
                arrow.step(&EmptyWorld, dt(), &[near, far], &mut out)
            {
                assert_eq!(target, Some(near.id));
                break;
            }
        }
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].target, near.id);
    }

    #[test]
    fn arrow_expires_at_its_range() {
        let bow = &tuning::embedded().weapons.bow;
        let mut arrow = Projectile::from_weapon(
            EntityId::new(50, 0),
            EntityId::new(1, 0),
            TeamId(0),
            Vec3::ZERO,
            look_forward(),
            bow,
        )
        .unwrap();
        let mut out = Vec::new();
        let mut expired = false;
        for _ in 0..200 {
            if arrow.step(&EmptyWorld, dt(), &[], &mut out) == ProjectileOutcome::Expired {
                expired = true;
                break;
            }
        }
        assert!(
            expired,
            "a projectile with no target must eventually expire"
        );
        assert!(out.is_empty());
    }

    #[test]
    fn mage_hits_3_clustered() {
        let bolt = &tuning::embedded().weapons.mage_bolt;
        assert_eq!(bolt.damage, 22, "§4.6");
        let mut p = Projectile::from_weapon(
            EntityId::new(70, 0),
            EntityId::new(1, 0),
            TeamId(0),
            Vec3::from_ints(0, 1, 0),
            look_forward(),
            bolt,
        )
        .expect("the mage bolt is a projectile");

        // Three enemies inside the 2.5 m burst radius, one far away.
        let targets = [foe(10, 0, 6), foe(11, 1, 6), foe(12, 0, 7), foe(13, 0, 40)];
        let distant = targets[3].id;

        let mut out = Vec::new();
        let mut impacted = false;
        for _ in 0..60 {
            if matches!(
                p.step(&EmptyWorld, dt(), &targets, &mut out),
                ProjectileOutcome::Impact { .. }
            ) {
                impacted = true;
                break;
            }
        }
        assert!(impacted, "the bolt must reach the cluster");
        assert_eq!(out.len(), 3, "all three clustered enemies take the burst");
        assert!(out.iter().all(|e| e.kind == DamageKind::Burst));
        assert!(out.iter().all(|e| e.amount == 22));
        assert!(
            out.iter().all(|e| e.target != distant),
            "the distant enemy is outside the radius"
        );
        // Each entity is hit once, not once per member of the cluster.
        let mut ids: Vec<_> = out.iter().map(|e| e.target).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 3);
    }

    #[test]
    fn projectiles_stop_at_terrain() {
        let bow = &tuning::embedded().weapons.bow;
        let mut world = VoxelGrid::new([0, 0, 0], [32, 8, 32]);
        // A wall at z = 5, one block thick, from the floor to y = 4.
        for x in 0..32 {
            for y in 0..4 {
                world.set_solid(x, y, 5, true);
            }
        }
        let mut arrow = Projectile::from_weapon(
            EntityId::new(50, 0),
            EntityId::new(1, 0),
            TeamId(0),
            Vec3::from_ints(1, 1, 0),
            look_forward(),
            bow,
        )
        .unwrap();
        let mut out = Vec::new();
        let mut stopped = None;
        for _ in 0..30 {
            if let ProjectileOutcome::Impact { target, at } =
                arrow.step(&world, dt(), &[], &mut out)
            {
                stopped = Some((target, at));
                break;
            }
        }
        let (target, at) = stopped.expect("the arrow must stop at the wall");
        assert_eq!(target, None, "terrain is not an entity");
        assert!(
            (at.z.to_f64() - 5.0).abs() < 0.3,
            "should stop at the wall face, got {:.2}",
            at.z.to_f64()
        );
        assert!(out.is_empty());
    }

    #[test]
    fn an_enemy_behind_a_wall_is_not_hit() {
        // Terrain is tested before entities, so cover actually works.
        let bow = &tuning::embedded().weapons.bow;
        let mut world = VoxelGrid::new([0, 0, 0], [32, 8, 32]);
        for x in 0..32 {
            for y in 0..4 {
                world.set_solid(x, y, 5, true);
            }
        }
        let target = foe(10, 0, 10);
        let mut arrow = Projectile::from_weapon(
            EntityId::new(50, 0),
            EntityId::new(1, 0),
            TeamId(0),
            Vec3::from_ints(1, 1, 0),
            look_forward(),
            bow,
        )
        .unwrap();
        let mut out = Vec::new();
        for _ in 0..30 {
            arrow.step(&world, dt(), &[target], &mut out);
        }
        assert!(out.is_empty(), "the wall absorbs the arrow");
    }

    #[test]
    fn segment_aabb_is_exact_at_the_boundaries() {
        let box_ = Aabb {
            min: Vec3::from_ints(1, 0, 1),
            max: Vec3::from_ints(3, 2, 3),
        };
        let from = Vec3::from_ints(-5, 1, 2);
        let to = Vec3::from_ints(5, 1, 2);
        let t = segment_aabb(from, to, box_).expect("a ray through the box must hit");
        // Enters at x = 1, which is 6/10 of the way along.
        assert!((t.to_f64() - 0.6).abs() < 1e-3, "entered at {t:?}");

        // A ray that misses on the y axis.
        assert_eq!(
            segment_aabb(Vec3::from_ints(-5, 5, 2), Vec3::from_ints(5, 5, 2), box_),
            None
        );

        // A ray parallel to the box's z axis but outside it.
        assert_eq!(
            segment_aabb(Vec3::from_ints(-5, 1, 9), Vec3::from_ints(5, 1, 9), box_),
            None
        );

        // Starting inside returns zero, not None.
        assert_eq!(
            segment_aabb(Vec3::from_ints(2, 1, 2), Vec3::from_ints(5, 1, 2), box_),
            Some(ZERO)
        );
    }

    #[test]
    fn melee_is_order_stable() {
        // Two clients building the same target slice must emit the same events
        // in the same order; that is what keeps the replay hashes identical.
        let spear = &tuning::embedded().weapons.spear;
        let a = hero();
        let targets = [foe(10, 0, 1), foe(11, 0, 2), foe(12, 0, 3)];
        let mut first = Vec::new();
        let mut second = Vec::new();
        melee(
            a.id,
            a.team,
            eye_of(&a),
            look_forward(),
            spear,
            &targets,
            &mut first,
        );
        melee(
            a.id,
            a.team,
            eye_of(&a),
            look_forward(),
            spear,
            &targets,
            &mut second,
        );
        assert_eq!(first, second);
        assert_eq!(
            first.len(),
            3,
            "all three are inside the cone and the reach"
        );
        assert_eq!(
            first.iter().map(|e| e.target).collect::<Vec<_>>(),
            targets.iter().map(|t| t.id).collect::<Vec<_>>(),
            "events follow slice order"
        );
    }

    #[test]
    fn melee_weapons_produce_no_projectile() {
        let w = &tuning::embedded().weapons;
        for weapon in [&w.fists, &w.sword, &w.spear] {
            assert!(
                Projectile::from_weapon(
                    EntityId::new(1, 0),
                    EntityId::new(2, 0),
                    TeamId(0),
                    Vec3::ZERO,
                    look_forward(),
                    weapon
                )
                .is_none(),
                "a melee weapon must not spawn a projectile"
            );
        }
    }
}
