//! Tunable constants, loaded from `data/tuning.ron`.
//!
//! Every number a designer would want to change lives in one file. This module
//! is the **authoring boundary**: the RON file uses decimal floats for
//! readability, and they are converted to [`Fixed`] exactly once, at load. That
//! is why `tests/no_float.rs` exempts this file alongside `fixed.rs` — nowhere
//! else in `sim` may touch an `f32`.
//!
//! Validation is strict on purpose. A negative `carry_cap` or a `pad.size` of
//! zero would produce a game that is subtly broken in ways no test would
//! describe, so bad data fails loudly at load with the offending field named.

use core::sync::atomic::{AtomicU64, Ordering};

use serde::Deserialize;

use crate::buddy::{BuddyKind, BuddyStats};
use crate::combat::{AttackProfile, WeaponStats};
use crate::fixed::Fixed;
use crate::time::TICK_HZ;

/// Validation failure while loading tuning data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuningError {
    /// Dotted path of the offending field, e.g. `airdrop.carry_cap`.
    pub field: &'static str,
    /// Why it was rejected.
    pub reason: &'static str,
}

impl core::fmt::Display for TuningError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "invalid tuning at `{}`: {}", self.field, self.reason)
    }
}

impl std::error::Error for TuningError {}

// ---------------------------------------------------------------------------
// Raw (deserialised) shape — decimal floats live only here.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
struct TuningFile {
    #[serde(rename = "match")]
    match_cfg: MatchFile,
    player: PlayerFile,
    physics: PhysicsFile,
    core: CoreFile,
    weapons: WeaponsFile,
    airdrop: AirdropFile,
    pad: PadFile,
    terrain: TerrainFile,
    world: WorldFile,
}

#[derive(Debug, Clone, Deserialize)]
struct MatchFile {
    target_minutes: u32,
    overtime_start_s: u32,
    decay_per_30s: f32,
}

#[derive(Debug, Clone, Deserialize)]
struct PlayerFile {
    hp: i32,
    respawn_s: u32,
    spawn_protect_s: u32,
    speed: f32,
    sprint_speed: f32,
    radius: f32,
    height: f32,
}

#[derive(Debug, Clone, Deserialize)]
struct PhysicsFile {
    gravity: f32,
    terminal_velocity: f32,
    jump_height: f32,
    step_up: f32,
    swim_speed: f32,
    swim_gravity_mult: f32,
    max_substep: f32,
}

#[derive(Debug, Clone, Deserialize)]
struct CoreFile {
    hp: i32,
    damage_mult: f32,
    regen: f32,
}

#[derive(Debug, Clone, Deserialize)]
enum AttackProfileFile {
    Melee,
    Projectile { speed: f32 },
    Burst { speed: f32, radius: f32 },
}

#[derive(Debug, Clone, Deserialize)]
struct WeaponFile {
    damage: i32,
    cooldown_s: f32,
    reach: f32,
    profile: AttackProfileFile,
}

#[derive(Debug, Clone, Deserialize)]
struct WeaponsFile {
    fists: WeaponFile,
    sword: WeaponFile,
    spear: WeaponFile,
    bow: WeaponFile,
    mage_bolt: WeaponFile,
    swordsman_blade: WeaponFile,
    archer_bow: WeaponFile,
    healer_staff: WeaponFile,
    berserker_axe: WeaponFile,
    sniper_rifle: WeaponFile,
}

#[derive(Debug, Clone, Deserialize)]
struct BuddyFile {
    kind: BuddyKind,
    hp: i32,
    speed: f32,
    aggro: f32,
    cost: u32,
    cap: u32,
}

#[derive(Debug, Clone, Deserialize)]
struct AirdropFile {
    first_drop_s: u32,
    interval_early_s: u32,
    interval_late_s: u32,
    late_after_s: u32,
    crates_per_player: u32,
    heavy_every: u32,
    heavy_mult: f32,
    telegraph_s: u32,
    fall_speed: f32,
    pickup_radius: f32,
    carry_cap: u32,
    drop_loss: f32,
    pickup_ttl_s: u32,
    max_pickups: u32,
}

#[derive(Debug, Clone, Deserialize)]
struct PadFile {
    size: u32,
    max_height: u32,
    deposit_rate: u32,
}

#[derive(Debug, Clone, Deserialize)]
struct TerrainFile {
    breakable: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct WorldFile {
    size_x: u32,
    size_z: u32,
    height: u32,
    sea_level: u32,
    chunk: u32,
    base_radius: u32,
}

// ---------------------------------------------------------------------------
// Typed, fixed-point tuning
// ---------------------------------------------------------------------------

/// Match-level rules (plan §4.2).
#[derive(Debug, Clone)]
pub struct MatchTuning {
    /// Nominal match length in minutes; overtime decay starts before this.
    pub target_minutes: u32,
    /// Seconds at which overtime decay begins.
    pub overtime_start_s: u32,
    /// Fraction of max core HP removed every 30 s during overtime.
    pub decay_per_30s: Fixed,
}

/// Player movement and survival (plan §4.8).
#[derive(Debug, Clone)]
pub struct PlayerTuning {
    /// Maximum HP.
    pub hp: i32,
    /// Respawn delay in seconds.
    pub respawn_s: u32,
    /// Invulnerability window after respawn, in seconds.
    pub spawn_protect_s: u32,
    /// Maximum horizontal speed, metres per second.
    pub speed: Fixed,
    /// Maximum horizontal speed while sprinting, metres per second.
    pub sprint_speed: Fixed,
    /// Collision AABB half-width.
    pub radius: Fixed,
    /// Collision AABB height.
    pub height: Fixed,
}

/// Movement physics (plan T-009, §4.8).
#[derive(Debug, Clone)]
pub struct PhysicsTuning {
    /// Downward acceleration, metres per second squared.
    pub gravity: Fixed,
    /// Maximum fall speed, metres per second.
    pub terminal_velocity: Fixed,
    /// Apex of a standing jump, in blocks. Jump velocity is derived from this.
    pub jump_height: Fixed,
    /// Tallest ledge that can be walked up without jumping.
    pub step_up: Fixed,
    /// Upward speed while holding jump in water.
    pub swim_speed: Fixed,
    /// Fraction of gravity that applies while submerged.
    pub swim_gravity_mult: Fixed,
    /// Largest single collision-sweep increment.
    pub max_substep: Fixed,
}

impl PhysicsTuning {
    /// Take-off speed for a jump of `jump_height`: `sqrt(2 g h)`.
    ///
    /// Derived rather than stored so the designer-facing number is the apex,
    /// which is what a player can see and what the plan specifies.
    pub fn jump_speed(&self) -> Fixed {
        (Fixed::from_int(2) * self.gravity * self.jump_height).sqrt()
    }

    /// Gravity while submerged.
    pub fn swim_gravity(&self) -> Fixed {
        self.gravity * self.swim_gravity_mult
    }
}

/// Base core (plan §4.8).
#[derive(Debug, Clone)]
pub struct CoreTuning {
    /// Maximum HP.
    pub hp: i32,
    /// Damage multiplier applied to all incoming damage.
    pub damage_mult: Fixed,
    /// HP regenerated per second (zero by design).
    pub regen: Fixed,
}

/// The weapon table (plan §4.8 for players, §4.6 for buddies).
///
/// Named fields rather than a slice so a typo in `tuning.ron` is a load error
/// instead of a silently missing weapon.
#[derive(Debug, Clone)]
pub struct WeaponTuning {
    /// Bare hands: always available, weakest, so a player without a pickup can
    /// still contribute (§4.8).
    pub fists: WeaponStats,
    /// Melee pickup, `R01_SWORD`.
    pub sword: WeaponStats,
    /// Long-reach melee pickup, `R08_SPEAR`.
    pub spear: WeaponStats,
    /// Projectile pickup, `R02_BOW`.
    pub bow: WeaponStats,
    /// The Mage buddy's area attack (§4.6).
    pub mage_bolt: WeaponStats,
    /// Swordsman melee.
    pub swordsman_blade: WeaponStats,
    /// Archer projectile.
    pub archer_bow: WeaponStats,
    /// Healer's staff: `damage` is the heal amount.
    pub healer_staff: WeaponStats,
    /// Berserker melee.
    pub berserker_axe: WeaponStats,
    /// Sniper projectile.
    pub sniper_rifle: WeaponStats,
}

impl WeaponTuning {
    /// The weapon a buddy of `kind` fights with.
    ///
    /// A `match` rather than a table so adding a role without giving it a
    /// weapon is a compile error, not a buddy that stands still.
    pub fn for_buddy(&self, kind: BuddyKind) -> WeaponStats {
        match kind {
            BuddyKind::Swordsman => self.swordsman_blade,
            BuddyKind::Archer => self.archer_bow,
            BuddyKind::Mage => self.mage_bolt,
            BuddyKind::Healer => self.healer_staff,
            BuddyKind::Berserker => self.berserker_axe,
            BuddyKind::Sniper => self.sniper_rifle,
        }
    }
}

/// Validate and convert one authored weapon.
///
/// The cooldown is the interesting conversion: a designer writes `0.45`, and
/// this is the only place that decides it means 9 ticks. Rounding here rather
/// than comparing seconds at runtime is what stops two clients disagreeing
/// about whether the third swing is legal on the tick it lands.
fn weapon(field: &'static str, f: WeaponFile) -> Result<WeaponStats, TuningError> {
    if f.damage <= 0 {
        return Err(TuningError {
            field,
            reason: "damage must be positive",
        });
    }
    if f.cooldown_s <= 0.0 {
        return Err(TuningError {
            field,
            reason: "cooldown_s must be positive: a zero cadence is unlimited damage",
        });
    }
    if f.reach <= 0.0 {
        return Err(TuningError {
            field,
            reason: "reach must be positive",
        });
    }
    let cooldown_ticks = (f.cooldown_s * TICK_HZ as f32).round() as u32;
    if cooldown_ticks == 0 {
        return Err(TuningError {
            field,
            reason: "cooldown_s rounds to zero ticks, which would fire every tick",
        });
    }
    let profile = match f.profile {
        AttackProfileFile::Melee => AttackProfile::Melee,
        AttackProfileFile::Projectile { speed } => {
            if speed <= 0.0 {
                return Err(TuningError {
                    field,
                    reason: "projectile speed must be positive",
                });
            }
            AttackProfile::Projectile {
                speed: Fixed::from_f32(speed),
            }
        }
        AttackProfileFile::Burst { speed, radius } => {
            if speed <= 0.0 || radius <= 0.0 {
                return Err(TuningError {
                    field,
                    reason: "burst speed and radius must both be positive",
                });
            }
            AttackProfile::Burst {
                speed: Fixed::from_f32(speed),
                radius: Fixed::from_f32(radius),
            }
        }
    };
    Ok(WeaponStats {
        damage: f.damage,
        cooldown_ticks,
        reach: Fixed::from_f32(f.reach),
        profile,
    })
}

/// The largest per-team buddy total in §4.7 (2v2 and 3v3 Teams).
///
/// A per-role cap above this could never be reached, because the team total
/// binds first. That makes it a data error rather than a harmless surplus.
pub const MAX_TEAM_BUDDIES: u32 = 5;

/// Dotted field names for roster errors, indexed by [`BuddyKind`].
const BUDDY_FIELDS: [&str; 6] = [
    "buddies.Swordsman",
    "buddies.Archer",
    "buddies.Mage",
    "buddies.Healer",
    "buddies.Berserker",
    "buddies.Sniper",
];

/// Parse `data/buddies.ron` into a dense table indexed by [`BuddyKind`].
///
/// The roster is a separate file from `tuning.ron` (plan T-012) because it is a
/// table, not a set of knobs: adding a role is a row, not a new key in a struct
/// literal. The cost of that split is this function, which has to prove the
/// file is complete — a missing row would otherwise become a buddy that spawns
/// with zero HP and silently dies on its first tick.
fn buddy_table(src: &str, weapons: &WeaponTuning) -> Result<[BuddyStats; 6], TuningError> {
    let rows: Vec<BuddyFile> = ron::from_str(src).map_err(|_| TuningError {
        field: "buddies",
        reason: "not valid RON for the buddy roster",
    })?;

    let mut slots: [Option<BuddyStats>; 6] = [None; 6];

    for row in rows {
        let field = BUDDY_FIELDS[row.kind as usize];
        if row.hp <= 0 {
            return Err(TuningError {
                field,
                reason: "hp must be positive",
            });
        }
        if row.speed <= 0.0 {
            return Err(TuningError {
                field,
                reason: "speed must be positive",
            });
        }
        if row.aggro <= 0.0 {
            return Err(TuningError {
                field,
                reason: "aggro must be positive",
            });
        }
        if row.cost == 0 {
            return Err(TuningError {
                field,
                reason: "cost must be > 0: a free buddy is not a crate sink",
            });
        }
        if row.cap == 0 {
            return Err(TuningError {
                field,
                reason: "cap must be > 0: a role that cannot be built is dead data",
            });
        }
        if row.cap > MAX_TEAM_BUDDIES {
            return Err(TuningError {
                field,
                reason: "cap exceeds the §4.7 per-team buddy total, so it is unreachable",
            });
        }
        if slots[row.kind as usize].is_some() {
            return Err(TuningError {
                field,
                reason: "duplicate roster row: which one wins would depend on file order",
            });
        }
        slots[row.kind as usize] = Some(BuddyStats {
            kind: row.kind,
            hp: row.hp,
            weapon: weapons.for_buddy(row.kind),
            speed: Fixed::from_f32(row.speed),
            aggro: Fixed::from_f32(row.aggro),
            cost: row.cost,
            cap: row.cap,
        });
    }

    let mut dense = Vec::with_capacity(6);
    for kind in BuddyKind::ALL {
        match slots[kind as usize] {
            Some(stats) => dense.push(stats),
            None => {
                return Err(TuningError {
                    field: BUDDY_FIELDS[kind as usize],
                    reason: "missing from data/buddies.ron",
                })
            }
        }
    }

    // Six roles, each with `cap >= 1`, so the per-role caps always sum to at
    // least 6 and can always fill §4.7's largest team budget of 5. That is why
    // there is no "sum is too small" branch here: it would be unreachable, and
    // an unreachable branch is a claim no test can check.
    dense.try_into().map_err(|_| TuningError {
        field: "buddies",
        reason: "roster did not produce exactly six rows",
    })
}

/// Airdrop economy (plan §4.4).
#[derive(Debug, Clone)]
pub struct AirdropTuning {
    /// Seconds until the first drop.
    pub first_drop_s: u32,
    /// Interval before `late_after_s`.
    pub interval_early_s: u32,
    /// Interval after `late_after_s`.
    pub interval_late_s: u32,
    /// Seconds after which the schedule tightens.
    pub late_after_s: u32,
    /// Crates per drop, per player in the room.
    pub crates_per_player: u32,
    /// Every Nth drop is a heavy drop.
    pub heavy_every: u32,
    /// Multiplier applied to a heavy drop's crate count.
    pub heavy_mult: Fixed,
    /// Seconds of warning before impact.
    pub telegraph_s: u32,
    /// Descent speed, metres per second.
    pub fall_speed: Fixed,
    /// Auto-pickup radius, metres.
    pub pickup_radius: Fixed,
    /// Maximum crates carried.
    pub carry_cap: u32,
    /// Fraction of carried crates destroyed when the carrier dies.
    pub drop_loss: Fixed,
    /// Seconds a landed crate persists before despawning.
    pub pickup_ttl_s: u32,
    /// Maximum simultaneous crate pickups.
    pub max_pickups: u32,
}

/// Stacking pad (plan §4.3).
#[derive(Debug, Clone)]
pub struct PadTuning {
    /// Edge length in blocks.
    pub size: u32,
    /// Maximum build height in blocks.
    pub max_height: u32,
    /// Crates deposited per second while holding the place button.
    pub deposit_rate: u32,
}

/// Terrain rules (plan §4.4.4).
#[derive(Debug, Clone)]
pub struct TerrainTuning {
    /// Whether terrain blocks can be broken. False in v1; flipping this
    /// re-opens the tunnelling exploit class and the diff budget.
    pub breakable: bool,
}

/// World dimensions (plan §4.3).
#[derive(Debug, Clone)]
pub struct WorldTuning {
    /// Extent along X in blocks.
    pub size_x: u32,
    /// Extent along Z in blocks.
    pub size_z: u32,
    /// Extent along Y in blocks.
    pub height: u32,
    /// Sea level Y.
    pub sea_level: u32,
    /// Chunk column edge length.
    pub chunk: u32,
    /// Radius of the circle bases are placed on.
    pub base_radius: u32,
}

/// The complete tuning set.
#[derive(Debug, Clone)]
pub struct Tuning {
    /// Match rules.
    pub match_cfg: MatchTuning,
    /// Player rules.
    pub player: PlayerTuning,
    /// Movement physics.
    pub physics: PhysicsTuning,
    /// Core rules.
    pub core: CoreTuning,
    /// Weapon table.
    pub weapons: WeaponTuning,
    /// Buddy roster, indexed by [`BuddyKind`].
    pub buddies: [BuddyStats; 6],
    /// Airdrop rules.
    pub airdrop: AirdropTuning,
    /// Pad rules.
    pub pad: PadTuning,
    /// Terrain rules.
    pub terrain: TerrainTuning,
    /// World dimensions.
    pub world: WorldTuning,
}

impl Tuning {
    /// Parse and validate a RON document against the embedded roster.
    pub fn parse(src: &str) -> Result<Tuning, TuningError> {
        Tuning::parse_with_roster(src, EMBEDDED_BUDDIES_RON)
    }

    /// Parse `src` against a caller-supplied roster document.
    ///
    /// Separate from [`Tuning::parse`] so tests can drive a bad roster through
    /// the same validation the shipped one gets. There is no runtime roster
    /// override: the file is compiled in, like the rest of the tuning.
    pub fn parse_with_roster(src: &str, roster: &str) -> Result<Tuning, TuningError> {
        let file: TuningFile = ron::from_str(src).map_err(|_| TuningError {
            field: "<document>",
            reason: "not valid RON for the tuning schema",
        })?;
        Tuning::from_file(file, roster)
    }

    fn from_file(f: TuningFile, roster: &str) -> Result<Tuning, TuningError> {
        // --- ranges -----------------------------------------------------
        if f.player.hp <= 0 {
            return Err(TuningError {
                field: "player.hp",
                reason: "must be positive",
            });
        }
        if f.player.speed <= 0.0 {
            return Err(TuningError {
                field: "player.speed",
                reason: "must be positive",
            });
        }
        if f.player.sprint_speed <= f.player.speed {
            return Err(TuningError {
                field: "player.sprint_speed",
                reason: "must be greater than the walk speed",
            });
        }
        if f.physics.gravity <= 0.0 {
            return Err(TuningError {
                field: "physics.gravity",
                reason: "must be positive",
            });
        }
        if f.physics.terminal_velocity <= 0.0 {
            return Err(TuningError {
                field: "physics.terminal_velocity",
                reason: "must be positive",
            });
        }
        if f.physics.jump_height <= 0.0 {
            return Err(TuningError {
                field: "physics.jump_height",
                reason: "must be positive",
            });
        }
        if f.physics.step_up < 0.0 {
            return Err(TuningError {
                field: "physics.step_up",
                reason: "must be non-negative",
            });
        }
        if f.physics.swim_speed <= 0.0 {
            return Err(TuningError {
                field: "physics.swim_speed",
                reason: "must be positive",
            });
        }
        if !(0.0..=1.0).contains(&f.physics.swim_gravity_mult) {
            return Err(TuningError {
                field: "physics.swim_gravity_mult",
                reason: "must be within [0, 1]",
            });
        }
        if !(0.01..=0.5).contains(&f.physics.max_substep) {
            return Err(TuningError {
                field: "physics.max_substep",
                reason: "must be within [0.01, 0.5]: larger tunnels walls, smaller is wasted work",
            });
        }
        if f.core.hp <= 0 {
            return Err(TuningError {
                field: "core.hp",
                reason: "must be positive",
            });
        }
        if !(0.0..=1.0).contains(&f.core.damage_mult) {
            return Err(TuningError {
                field: "core.damage_mult",
                reason: "must be within [0, 1]",
            });
        }
        if f.airdrop.carry_cap == 0 {
            return Err(TuningError {
                field: "airdrop.carry_cap",
                reason: "must be > 0",
            });
        }
        if f.airdrop.interval_late_s == 0 || f.airdrop.interval_early_s == 0 {
            return Err(TuningError {
                field: "airdrop.interval_*_s",
                reason: "must be > 0",
            });
        }
        if f.airdrop.heavy_every == 0 {
            return Err(TuningError {
                field: "airdrop.heavy_every",
                reason: "must be > 0",
            });
        }
        if !(0.0..=1.0).contains(&f.airdrop.drop_loss) {
            return Err(TuningError {
                field: "airdrop.drop_loss",
                reason: "must be within [0, 1]",
            });
        }
        if f.pad.size == 0 || f.pad.size % 2 != 0 {
            return Err(TuningError {
                field: "pad.size",
                reason: "must be a positive even number (the pad is centred on a base)",
            });
        }
        if f.pad.max_height == 0 {
            return Err(TuningError {
                field: "pad.max_height",
                reason: "must be > 0",
            });
        }
        if f.world.chunk == 0 || f.world.height % f.world.chunk != 0 {
            return Err(TuningError {
                field: "world.height",
                reason: "must be a multiple of world.chunk",
            });
        }
        if f.world.sea_level >= f.world.height {
            return Err(TuningError {
                field: "world.sea_level",
                reason: "must be below world.height",
            });
        }

        // Built as locals because the roster needs to read the weapon table
        // before either is moved into the struct literal.
        let weapons = WeaponTuning {
            fists: weapon("weapons.fists", f.weapons.fists)?,
            sword: weapon("weapons.sword", f.weapons.sword)?,
            spear: weapon("weapons.spear", f.weapons.spear)?,
            bow: weapon("weapons.bow", f.weapons.bow)?,
            mage_bolt: weapon("weapons.mage_bolt", f.weapons.mage_bolt)?,
            swordsman_blade: weapon("weapons.swordsman_blade", f.weapons.swordsman_blade)?,
            archer_bow: weapon("weapons.archer_bow", f.weapons.archer_bow)?,
            healer_staff: weapon("weapons.healer_staff", f.weapons.healer_staff)?,
            berserker_axe: weapon("weapons.berserker_axe", f.weapons.berserker_axe)?,
            sniper_rifle: weapon("weapons.sniper_rifle", f.weapons.sniper_rifle)?,
        };
        let buddies = buddy_table(roster, &weapons)?;

        Ok(Tuning {
            match_cfg: MatchTuning {
                target_minutes: f.match_cfg.target_minutes,
                overtime_start_s: f.match_cfg.overtime_start_s,
                decay_per_30s: Fixed::from_f32(f.match_cfg.decay_per_30s),
            },
            player: PlayerTuning {
                hp: f.player.hp,
                respawn_s: f.player.respawn_s,
                spawn_protect_s: f.player.spawn_protect_s,
                speed: Fixed::from_f32(f.player.speed),
                sprint_speed: Fixed::from_f32(f.player.sprint_speed),
                radius: Fixed::from_f32(f.player.radius),
                height: Fixed::from_f32(f.player.height),
            },
            physics: PhysicsTuning {
                gravity: Fixed::from_f32(f.physics.gravity),
                terminal_velocity: Fixed::from_f32(f.physics.terminal_velocity),
                jump_height: Fixed::from_f32(f.physics.jump_height),
                step_up: Fixed::from_f32(f.physics.step_up),
                swim_speed: Fixed::from_f32(f.physics.swim_speed),
                swim_gravity_mult: Fixed::from_f32(f.physics.swim_gravity_mult),
                max_substep: Fixed::from_f32(f.physics.max_substep),
            },
            core: CoreTuning {
                hp: f.core.hp,
                damage_mult: Fixed::from_f32(f.core.damage_mult),
                regen: Fixed::from_f32(f.core.regen),
            },
            weapons,
            buddies,
            airdrop: AirdropTuning {
                first_drop_s: f.airdrop.first_drop_s,
                interval_early_s: f.airdrop.interval_early_s,
                interval_late_s: f.airdrop.interval_late_s,
                late_after_s: f.airdrop.late_after_s,
                crates_per_player: f.airdrop.crates_per_player,
                heavy_every: f.airdrop.heavy_every,
                heavy_mult: Fixed::from_f32(f.airdrop.heavy_mult),
                telegraph_s: f.airdrop.telegraph_s,
                fall_speed: Fixed::from_f32(f.airdrop.fall_speed),
                pickup_radius: Fixed::from_f32(f.airdrop.pickup_radius),
                carry_cap: f.airdrop.carry_cap,
                drop_loss: Fixed::from_f32(f.airdrop.drop_loss),
                pickup_ttl_s: f.airdrop.pickup_ttl_s,
                max_pickups: f.airdrop.max_pickups,
            },
            pad: PadTuning {
                size: f.pad.size,
                max_height: f.pad.max_height,
                deposit_rate: f.pad.deposit_rate,
            },
            terrain: TerrainTuning {
                breakable: f.terrain.breakable,
            },
            world: WorldTuning {
                size_x: f.world.size_x,
                size_z: f.world.size_z,
                height: f.world.height,
                sea_level: f.world.sea_level,
                chunk: f.world.chunk,
                base_radius: f.world.base_radius,
            },
        })
    }
}

impl AirdropTuning {
    /// Interval between drops at `elapsed_s` seconds into the match (plan §4.4.1).
    ///
    /// The schedule tightens once, at `late_after_s`. It is a step function
    /// rather than a curve because a designer can reason about two numbers,
    /// and this is the knob §18.4 says to turn first when pacing is wrong.
    pub fn interval_at(&self, elapsed_s: u32) -> u32 {
        if elapsed_s < self.late_after_s {
            self.interval_early_s
        } else {
            self.interval_late_s
        }
    }

    /// Crate count for the drop with the given 1-based index.
    pub fn crates_for_drop(&self, drop_index: u32, player_count: u32) -> u32 {
        let base = self.crates_per_player.saturating_mul(player_count.max(1));
        if self.heavy_every > 0 && drop_index % self.heavy_every == 0 {
            self.heavy_mult
                .saturating_mul(Fixed::from_int(base as i32))
                .round_int()
                .max(0) as u32
        } else {
            base
        }
    }
}

/// The tuning embedded in the binary at compile time.
pub fn embedded() -> &'static Tuning {
    static CELL: std::sync::OnceLock<&'static Tuning> = std::sync::OnceLock::new();
    CELL.get_or_init(|| {
        let parsed = Tuning::parse(EMBEDDED_RON).expect("embedded data/tuning.ron is invalid");
        // Leak once at startup: tuning lives for the process lifetime and is
        // shared by every room, so a single allocation is the right trade.
        Box::leak(Box::new(parsed))
    })
}

/// Raw embedded RON, exposed so tests can re-parse and diff it.
pub const EMBEDDED_RON: &str = include_str!("../../../data/tuning.ron");

/// Raw embedded buddy roster (plan T-012).
pub const EMBEDDED_BUDDIES_RON: &str = include_str!("../../../data/buddies.ron");

/// Stats for one buddy role.
///
/// Returns a `'static` reference into the embedded tuning so callers do not
/// have to thread a `&Tuning` through the AI: a buddy's numbers are immutable
/// for the process lifetime, and the alternative — copying a [`BuddyStats`]
/// into every buddy — invites two buddies of the same role disagreeing.
pub fn buddy(kind: BuddyKind) -> &'static BuddyStats {
    &embedded().buddies[kind as usize]
}

/// A process-wide generation counter, useful for cache invalidation when
/// tuning is reloaded in a dev build.
static TUNING_GENERATION: AtomicU64 = AtomicU64::new(1);

/// Current tuning generation.
pub fn generation() -> u64 {
    TUNING_GENERATION.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_tuning_parses() {
        let t = Tuning::parse(EMBEDDED_RON).expect("embedded tuning must parse");
        assert_eq!(t.player.hp, 100);
    }

    /// Guards against doc/code drift: the values asserted here are the ones
    /// written down in plan §4.8, §4.4 and §4.7. If someone changes the RON
    /// without updating the plan, this fails.
    /// Fixed-point round-trips are exact for dyadic decimals (4.5, 0.0) but
    /// not for values like 0.35, so decimal comparisons use this tolerance.
    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn tuning_matches_docs() {
        let t = embedded();

        // §4.8 combat and base math
        assert_eq!(t.player.hp, 100);
        assert_eq!(t.player.respawn_s, 8);
        assert_eq!(t.player.spawn_protect_s, 2);
        assert!(close(t.player.speed.to_f32(), 4.5));
        assert!(close(t.player.sprint_speed.to_f32(), 5.6));
        assert_eq!(t.core.hp, 10_000);
        assert!(close(t.core.damage_mult.to_f32(), 0.35));
        assert!(close(t.core.regen.to_f32(), 0.0));

        // §4.8 / T-009 physics
        assert!(close(t.physics.gravity.to_f32(), 24.0));
        assert!(close(t.physics.terminal_velocity.to_f32(), 50.0));
        assert!(close(t.physics.jump_height.to_f32(), 1.25));
        assert!(close(t.physics.step_up.to_f32(), 1.0));
        assert!(close(t.physics.swim_speed.to_f32(), 2.0));
        // Apex is 1.25 blocks, so take-off speed is sqrt(2 * 24 * 1.25) = 7.746.
        assert!(close(t.physics.jump_speed().to_f32(), 7.746));

        // §4.4 airdrop economy
        assert_eq!(t.airdrop.first_drop_s, 10);
        assert_eq!(t.airdrop.interval_early_s, 25);
        assert_eq!(t.airdrop.interval_late_s, 15);
        assert_eq!(t.airdrop.late_after_s, 300);
        assert_eq!(t.airdrop.crates_per_player, 3);
        assert_eq!(t.airdrop.heavy_every, 5);
        assert_eq!(t.airdrop.telegraph_s, 5);
        assert_eq!(t.airdrop.carry_cap, 12);
        assert_eq!(t.airdrop.max_pickups, 120);

        // §4.3 map and pad
        assert_eq!(t.pad.size, 8);
        assert_eq!(t.pad.max_height, 4);
        assert_eq!(t.world.size_x, 256);
        assert_eq!(t.world.height, 96);
        assert_eq!(t.world.sea_level, 40);
        assert_eq!(t.world.chunk, 16);
        assert_eq!(t.world.base_radius, 96);

        // §4.4.4 terrain is indestructible in v1
        assert!(!t.terrain.breakable);
    }

    #[test]
    fn drop_interval_switches_once() {
        let t = embedded();
        assert_eq!(t.airdrop.interval_at(0), 25);
        assert_eq!(t.airdrop.interval_at(299), 25);
        assert_eq!(t.airdrop.interval_at(300), 15);
        assert_eq!(t.airdrop.interval_at(1199), 15);
    }

    #[test]
    fn drop_size_scales_with_players() {
        let t = embedded();
        // Plan §4.4.1: 3 x player_count, x1.75 on every 5th drop.
        assert_eq!(t.airdrop.crates_for_drop(1, 2), 6);
        assert_eq!(t.airdrop.crates_for_drop(1, 6), 18);
        assert_eq!(t.airdrop.crates_for_drop(5, 2), 11); // 6 * 1.75 = 10.5, rounded half away from zero
        assert_eq!(t.airdrop.crates_for_drop(5, 6), 32); // 18 * 1.75 = 31.5 -> 32
        assert_eq!(t.airdrop.crates_for_drop(10, 4), 21); // heavy again
        assert_eq!(t.airdrop.crates_for_drop(11, 4), 12);
    }

    #[test]
    fn invalid_data_is_rejected_with_field_name() {
        let bad = EMBEDDED_RON.replace("carry_cap: 12", "carry_cap: 0");
        let err = Tuning::parse(&bad).unwrap_err();
        assert_eq!(err.field, "airdrop.carry_cap");

        let bad = EMBEDDED_RON.replace("damage_mult: 0.35", "damage_mult: 1.5");
        let err = Tuning::parse(&bad).unwrap_err();
        assert_eq!(err.field, "core.damage_mult");

        let bad = EMBEDDED_RON.replace("size: 8", "size: 7");
        let err = Tuning::parse(&bad).unwrap_err();
        assert_eq!(err.field, "pad.size");

        assert!(Tuning::parse("not ron at all (").is_err());
    }

    #[test]
    fn generation_is_positive() {
        assert!(generation() >= 1);
    }

    /// Drop the roster line for `kind`, leaving the rest untouched.
    fn roster_without(kind: &str) -> String {
        let needle = format!("kind: {kind},");
        EMBEDDED_BUDDIES_RON
            .lines()
            .filter(|line| !line.contains(&needle))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn roster_must_have_one_row_per_role() {
        // Every role must be present: a missing row is a load error, not a
        // buddy that spawns with zero HP and dies on its first tick.
        for kind in BuddyKind::ALL {
            let name = format!("{kind:?}");
            let roster = roster_without(&name);
            assert_ne!(
                roster, EMBEDDED_BUDDIES_RON,
                "{name} was not found to remove"
            );
            let err = Tuning::parse_with_roster(EMBEDDED_RON, &roster).unwrap_err();
            assert_eq!(err.field, format!("buddies.{name}"), "removing {name}");
        }
    }

    #[test]
    fn roster_rejects_duplicates() {
        // Renaming the Archer row to Swordsman leaves two swordsmen and no
        // archer. The duplicate must be reported, because "last row wins"
        // would make the roster depend on file order.
        let roster = EMBEDDED_BUDDIES_RON.replace("kind: Archer,", "kind: Swordsman,");
        let err = Tuning::parse_with_roster(EMBEDDED_RON, &roster).unwrap_err();
        assert_eq!(err.field, "buddies.Swordsman");
        assert!(err.reason.contains("duplicate"), "{}", err.reason);
    }

    #[test]
    fn roster_rejects_unreachable_caps() {
        // §4.7 caps a team at 5 buddies, so a per-role cap of 9 could never be
        // spent. Catch it at load rather than letting a designer tune a number
        // that does nothing.
        let roster = EMBEDDED_BUDDIES_RON.replace("cap: 4)", "cap: 9)");
        let err = Tuning::parse_with_roster(EMBEDDED_RON, &roster).unwrap_err();
        assert_eq!(err.field, "buddies.Swordsman");
        assert!(err.reason.contains("unreachable"), "{}", err.reason);
    }

    #[test]
    fn roster_rejects_zeroed_fields() {
        for (from, to, field) in [
            ("hp: 120,", "hp: 0,", "buddies.Swordsman"),
            ("speed: 3.4", "speed: 0.0", "buddies.Swordsman"),
            ("aggro: 12.0", "aggro: 0.0", "buddies.Swordsman"),
            ("cost: 9,", "cost: 0,", "buddies.Swordsman"),
            ("cap: 4)", "cap: 0)", "buddies.Swordsman"),
        ] {
            let roster = EMBEDDED_BUDDIES_RON.replace(from, to);
            assert_ne!(roster, EMBEDDED_BUDDIES_RON, "{from} was not found");
            let err = Tuning::parse_with_roster(EMBEDDED_RON, &roster).unwrap_err();
            assert_eq!(err.field, field, "for {from}");
        }
        assert!(Tuning::parse_with_roster(EMBEDDED_RON, "not ron (").is_err());
    }

    #[test]
    fn buddy_content_matches_the_plan() {
        // §4.6: hp, speed, aggro, cost, per-team cap, then weapon damage,
        // cooldown ticks and reach. Damage and cadence live in the weapon
        // table, so this one row checks both files cannot drift apart.
        let expected = [
            (BuddyKind::Swordsman, 120, 3.4, 12.0, 9, 4, 14, 12, 1.8),
            (BuddyKind::Archer, 80, 3.2, 16.0, 8, 4, 12, 22, 14.0),
            (BuddyKind::Mage, 70, 3.0, 14.0, 12, 3, 22, 36, 10.0),
            (BuddyKind::Healer, 75, 3.2, 12.0, 10, 2, 15, 30, 8.0),
            (BuddyKind::Berserker, 200, 3.8, 14.0, 18, 2, 20, 10, 1.8),
            (BuddyKind::Sniper, 60, 2.8, 32.0, 15, 2, 40, 50, 28.0),
        ];
        let t = embedded();
        for (kind, hp, speed, aggro, cost, cap, damage, cooldown, reach) in expected {
            let stats = &t.buddies[kind as usize];
            assert_eq!(stats.kind, kind);
            assert_eq!(stats.hp, hp, "{kind:?} hp");
            assert!(close(stats.speed.to_f32(), speed), "{kind:?} speed");
            assert!(close(stats.aggro.to_f32(), aggro), "{kind:?} aggro");
            assert_eq!(stats.cost, cost, "{kind:?} cost");
            assert_eq!(stats.cap, cap, "{kind:?} cap");
            assert_eq!(stats.weapon.damage, damage, "{kind:?} damage");
            assert_eq!(
                stats.weapon.cooldown_ticks, cooldown,
                "{kind:?} cooldown ticks"
            );
            assert!(close(stats.weapon.reach.to_f32(), reach), "{kind:?} reach");
        }
    }

    #[test]
    fn the_roster_takes_its_weapons_from_the_weapon_table() {
        // One table of damage numbers: the buddy row must not restate them.
        let t = embedded();
        let swordsman = t.buddies[BuddyKind::Swordsman as usize];
        assert_eq!(swordsman.weapon.damage, t.weapons.swordsman_blade.damage);
        assert_eq!(swordsman.weapon.cooldown_ticks, 12);
        let healer = t.buddies[BuddyKind::Healer as usize];
        assert!(healer.heals());
        assert_eq!(healer.weapon.damage, t.weapons.healer_staff.damage);
        // And the accessor agrees with the table it reads.
        assert_eq!(buddy(BuddyKind::Sniper).hp, 60);
        assert_eq!(buddy(BuddyKind::Sniper).weapon.damage, 40);
    }
}
