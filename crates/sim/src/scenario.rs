//! A deterministic integration harness: every subsystem, one fixed tick order.
//!
//! # Why this is in the library rather than in a test
//!
//! The determinism harness (plan T-015) needs to drive movement, the recipe
//! matcher, buddy AI, combat and the match rules *in one order*, and both the
//! golden test and the `hash_trace` example need that same order. Duplicating
//! it in two places would give the two copies a way to disagree, which is the
//! exact failure the harness exists to catch.
//!
//! # What it is not
//!
//! This is not the room loop. It has no network, no airdrops and no crate
//! carrying — those arrive with the room (M2+). What it does have is the
//! property that matters here: **a pure function from a seed to a sequence of
//! hashes**, covering every arithmetic path in `sim` that a platform difference
//! could perturb.
//!
//! The scripted inputs are deliberately boring — four directions, a jump every
//! 50 ticks, a crate every 25 — because a determinism failure should be
//! reproducible by reading a 30-line script, not by bisecting a bot AI.
//!
//! # Bodies live in the match state, not here
//!
//! This harness used to carry its own `Vec<Mover>`, which meant player movement
//! was outside `MatchState::state_hash` and outside the code the room runs. Both
//! were wrong for the same reason: the point of the harness is to hash *the
//! rules*, and a copy of the rules is not the rules. Bodies are now
//! `MatchState`'s, driven through `MatchState::apply_input`, and the only thing
//! this module does is decide *where they stand* (`set_spawn`) and *what they
//! press* (`script`).
//!
//! # One deliberate simplification
//!
//! **Buddies are spawned directly** rather than crafted, so the buddy path is
//! exercised without the harness having to build a 9-crate recipe first.

use crate::buddy::{Buddy, BuddyContext, BuddyKind, Target};
use crate::combat::{self, Combatant, DamageEvent};
use crate::ids::{EntityId, PlayerId, TeamId};
use crate::input::{buttons, InputFrame};
use crate::match_rules::{Layout, Mode};
use crate::movement::MoveState;
use crate::state::{ring_pad_origins, MatchState, RoomSetup};
use crate::time::{Tick, TICK_HZ};
use crate::tuning;
use crate::vec::Vec3;
use crate::world_query::VoxelGrid;

/// Ticks between a player's melee swings (0.45 s at 20 Hz).
const SWING_TICKS: u32 = 9;

/// The yaw byte that faces from `from` toward `to`.
///
/// Yaw 0 faces `+Z` and increases toward `+X`, which is exactly `atan2(x, z)` —
/// not `atan2(z, x)`, and getting that backwards aims every scripted swing 90
/// degrees off. The result is wrapped into `0..256` because `atan2` returns
/// `-pi..=pi`.
fn yaw_toward(from: Vec3, to: Vec3) -> u8 {
    let rad = crate::fixed::atan2(to.x - from.x, to.z - from.z);
    let turns = (i64::from(rad.0) * 256) / i64::from(crate::fixed::TAU.0);
    turns.rem_euclid(256) as u8
}

/// How the harness drives one match.
#[derive(Clone, Copy, Debug)]
pub struct ScenarioConfig {
    /// Room shape.
    pub setup: RoomSetup,
    /// RNG seed.
    pub seed: u64,
    /// Raw damage each team deals to the next team's core every second.
    pub core_damage_per_s: i32,
    /// Ticks to run. The match may end sooner.
    pub ticks: u32,
}

impl ScenarioConfig {
    /// A two-player duel with no core damage: the match cannot end, so the
    /// hash sequence is always the full `ticks` long.
    pub fn duel(seed: u64, ticks: u32) -> ScenarioConfig {
        ScenarioConfig {
            setup: RoomSetup {
                mode: Mode::Ffa,
                teams: None,
                humans: 2,
                bots: 0,
            },
            seed,
            core_damage_per_s: 0,
            ticks,
        }
    }

    /// A 2v2 with bots and light chip damage on the cores.
    pub fn teams(seed: u64, ticks: u32) -> ScenarioConfig {
        ScenarioConfig {
            setup: RoomSetup {
                mode: Mode::Teams,
                teams: Some(2),
                humans: 2,
                bots: 2,
            },
            seed,
            core_damage_per_s: 1,
            ticks,
        }
    }

    /// Six FFA players and enough core damage to end the match mid-run, so
    /// elimination and the outcome are inside the hashed sequence.
    pub fn brawl(seed: u64, ticks: u32) -> ScenarioConfig {
        ScenarioConfig {
            setup: RoomSetup {
                mode: Mode::Ffa,
                teams: None,
                humans: 6,
                bots: 0,
            },
            seed,
            core_damage_per_s: 40,
            ticks,
        }
    }
}

/// A running scenario.
#[derive(Clone, Debug)]
pub struct Scenario {
    config: ScenarioConfig,
    state: MatchState,
    world: VoxelGrid,
    seed: u64,
    tick: Tick,
    /// Crates placed so far, used to walk the scripted pad pattern.
    placed: u32,
    /// Damage events emitted this tick.
    events: Vec<DamageEvent>,
    /// Damage events emitted over the whole run, for the combat hash.
    damage_total: i64,
}

/// One hashed checkpoint.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct HashFrame {
    /// Tick the checkpoint was taken at.
    pub tick: u32,
    /// Match-level hash (cores, pads, players, buddies, outcome).
    pub state: u64,
    /// Player positions and velocities.
    pub movement: u64,
    /// Buddy positions, health and state.
    pub buddies: u64,
    /// Player health and the damage events emitted this tick.
    pub combat: u64,
}

impl HashFrame {
    /// The wire format written to `tests/golden/*.hash`.
    ///
    /// Colon-separated hex so a human can diff two files and see immediately
    /// *which* subsystem moved, not just that something did.
    pub fn to_line(&self) -> String {
        format!(
            "{}:{:016x}:{:016x}:{:016x}:{:016x}",
            self.tick, self.state, self.movement, self.buddies, self.combat
        )
    }

    /// Parse a line written by [`HashFrame::to_line`].
    pub fn parse(line: &str) -> Option<HashFrame> {
        let mut parts = line.split(':');
        let tick = parts.next()?.parse().ok()?;
        let mut hex = || parts.next().and_then(|s| u64::from_str_radix(s, 16).ok());
        Some(HashFrame {
            tick,
            state: hex()?,
            movement: hex()?,
            buddies: hex()?,
            combat: hex()?,
        })
    }
}

impl Scenario {
    /// Build a scenario. Panics only if the config describes an illegal room,
    /// which is a programming error in a test, not a runtime condition.
    pub fn new(config: ScenarioConfig) -> Scenario {
        let layout = Layout::resolve(
            config.setup.mode,
            config.setup.humans + config.setup.bots,
            config.setup.teams,
        )
        .expect("scenario config must describe a legal room");
        let origins = ring_pad_origins(&layout, 20, 1);
        let mut state = MatchState::new(config.setup, &origins).expect("legal room");

        let mut world = VoxelGrid::new([-32, 0, -32], [64, 12, 64]);
        world.fill_layer(0, true);
        // A pair of walls so movement, line of sight and A* all have something
        // to be blocked by. Placed away from every pad.
        for z in 6..14 {
            for y in 1..4 {
                world.set_solid(0, y, z, true);
            }
        }
        for x in 6..14 {
            for y in 1..4 {
                world.set_solid(x, y, -8, true);
            }
        }

        // Pads sit 40 m apart on a ring, which is four times melee reach — a
        // match where nobody can hit anybody would hash an empty combat path.
        // Placing the bodies by hand puts them within reach of each other while
        // staying clear of the two walls, which is what keeps the scripted
        // circle-strafe meaningful.
        let seats: Vec<(PlayerId, TeamId)> =
            state.players().iter().map(|s| (s.id, s.team)).collect();
        for (i, (id, team)) in seats.into_iter().enumerate() {
            let spawn = Vec3::from_ints(
                -6 + i32::from(team.0) * 4 + (i as i32 % 2) * 2,
                1,
                -4 + (i as i32 % 3) * 3,
            );
            assert!(
                state.set_spawn(id, spawn),
                "seat {} must be addressable",
                id.0
            );
        }

        Scenario {
            config,
            state,
            world,
            seed: config.seed,
            tick: Tick::ZERO,
            placed: 0,
            events: Vec::new(),
            damage_total: 0,
        }
    }

    /// The match state.
    pub fn state(&self) -> &MatchState {
        &self.state
    }

    /// One player's body.
    pub fn mover(&self, index: usize) -> &MoveState {
        &self
            .state
            .body(PlayerId(index as u8))
            .expect("a scenario body per seat")
            .state
    }

    /// Current tick.
    pub fn tick(&self) -> Tick {
        self.tick
    }

    /// True once the match is over.
    pub fn is_over(&self) -> bool {
        self.state.is_over()
    }

    /// The scripted input for player `index` at `now`.
    ///
    /// Three things, all deterministic:
    ///
    /// * **Face the nearest other body.** This is the one that matters. The
    ///   script used to walk in four fixed compass directions, which meant two
    ///   duelists starting 6.7 m apart drifted further apart and *never landed a
    ///   hit in 20,000 ticks* — the `combat` field of every golden fixture was
    ///   hashing an empty event list. A guard test
    ///   (`the_scenario_reaches_movement_damage_and_respawn`) now fails if that
    ///   ever becomes true again.
    /// * **Walk forward, strafing on a two-second cycle.** Forward because the
    ///   yaw already points at the target, so advancing closes the distance;
    ///   strafing so the diagonal (`INV_SQRT2`) path and both collision axes
    ///   stay in play.
    /// * **Jump every 50 ticks.** Enough to exercise the jump arc, the landing
    ///   branch and the step-up attempt without dominating the hash.
    ///
    /// The seed offsets the strafe phase and the jump cadence, so two seeds do
    /// not walk identical paths — `a_different_seed_produces_a_different_sequence`
    /// is what checks that, and it has caught a decorative seed once already.
    fn script(&self, index: usize, now: Tick) -> InputFrame {
        let t = now.0;
        let seed = self.seed as u32;
        let bodies = self.state.bodies();
        let me = bodies[index].state.pos;

        let mut yaw = 0;
        let mut closest = None;
        for (i, other) in bodies.iter().enumerate() {
            if i == index {
                continue;
            }
            let d = me.distance_sq(other.state.pos);
            if closest.is_none_or(|best| d < best) {
                closest = Some(d);
                yaw = yaw_toward(me, other.state.pos);
            }
        }

        let strafe = match ((t / 40) + index as u32 + seed % 4) % 3 {
            0 => 1,
            1 => -1,
            _ => 0,
        };

        InputFrame {
            tick: now,
            move_x: strafe,
            move_z: 1,
            yaw,
            pitch: 0,
            buttons: if (t + seed) % 50 == 0 {
                buttons::JUMP
            } else {
                0
            },
        }
    }

    /// Advance one tick.
    pub fn step(&mut self) {
        if self.state.is_over() || self.tick.0 >= self.config.ticks {
            return;
        }
        let now = self.tick;
        self.events.clear();

        self.step_movement(now);
        self.step_crates(now);
        self.step_buddies(now);
        self.step_combat(now);
        self.step_cores(now);

        self.state.advance();
        self.tick = now.advance(1);
    }

    fn step_movement(&mut self, now: Tick) {
        // Respawn first, then input, so a player whose timer expires this tick
        // can act on this tick. Reversing the order costs one tick of dead time
        // per death, which over 20,000 ticks is 20,000 ticks of divergence from
        // what the room does.
        self.state.respawn_due(now, &self.world);

        for index in 0..self.state.players().len() {
            let id = self.state.players()[index].id;
            let frame = self.script(index, now);
            // A down or eliminated player returns `Err`, which is the correct
            // answer here: the script keeps pressing buttons, the simulation
            // ignores them.
            let _ = self.state.apply_input(id, &frame, &self.world);
        }
    }

    /// Place one crate every 25 ticks, walking a 3 x 2 slab and then a 2 x 2.
    ///
    /// The shapes are `R01_SWORD` (6 crates) and `R03_BANDAGE` (4), so the
    /// matcher fires, consumes, and starts over — which keeps both the flood
    /// fill and the canonical-form comparison in the hashed path.
    fn step_crates(&mut self, now: Tick) {
        if now.0 % 25 != 0 {
            return;
        }
        let teams = self.state.layout().teams;
        let team = TeamId((self.placed % teams as u32) as u8);
        let origin = match self.state.pads().get(team) {
            Some(pad) => pad.origin(),
            None => return,
        };
        let slot = (self.placed / teams as u32) % 10;
        let (x, z) = match slot {
            0..=5 => (slot % 3, slot / 3),
            _ => ((slot - 6) % 2, (slot - 6) / 2),
        };
        let cell = [origin[0] + x as i32, origin[1], origin[2] + z as i32];
        let _ = self.state.pads_mut().place(team, cell, true);
        let _ = self.state.craft(team, now);
        self.placed += 1;
    }

    fn step_buddies(&mut self, now: Tick) {
        // One swordsman per team, spawned on the tenth tick.
        if now.0 == 10 {
            for i in 0..self.state.layout().teams {
                let team = TeamId(i);
                let buddy = Buddy::new(
                    EntityId::new(u32::from(i), 0),
                    tuning::buddy(BuddyKind::Swordsman),
                    team,
                    PlayerId(i),
                    Vec3::from_ints(i as i32 * 4 - 4, 1, 4),
                );
                let _ = self.state.add_buddy(buddy);
            }
        }

        // Build the target list once: every living player, as the AI sees them.
        let targets: Vec<Target> = self
            .state
            .players()
            .iter()
            .zip(self.state.bodies())
            .map(|(player, body)| Target {
                id: EntityId::new(u32::from(player.id.0), 0),
                team: player.team,
                pos: body.state.pos,
                hp: body.hp,
                max_hp: tuning::embedded().player.hp,
                role: None,
            })
            .collect();

        let mut allies = Vec::new();
        let mut enemies = Vec::new();
        let teams = self.state.layout().teams as usize;
        for i in 0..teams {
            let team = TeamId(i as u8);
            allies.clear();
            enemies.clear();
            for target in &targets {
                if target.team == team {
                    allies.push(*target);
                } else {
                    enemies.push(*target);
                }
            }
            let owner = allies.first().copied();
            let ctx = BuddyContext {
                now,
                owner,
                allies: &allies,
                enemies: &enemies,
            };
            let world = &self.world;
            for buddy in &mut self.state.teams_mut()[i].buddies {
                let intent = buddy.think(&ctx, world);
                if let crate::buddy::BuddyAction::Attack(_) = intent.action {
                    // A buddy's swing is resolved by the room loop in a real
                    // match; here it only has to consume the same arithmetic.
                    self.damage_total += i64::from(buddy.hp % 3);
                }
            }
        }
    }

    fn step_combat(&mut self, now: Tick) {
        if now.0 % SWING_TICKS != 0 {
            return;
        }
        let weapon = tuning::embedded().weapons.sword;
        let cfg = *self.state.move_config();
        let combatants: Vec<Combatant> = self
            .state
            .players()
            .iter()
            .zip(self.state.bodies())
            .map(|(player, body)| Combatant {
                id: EntityId::new(u32::from(player.id.0), 0),
                team: player.team,
                pos: body.state.pos,
                half_width: cfg.half_width,
                height: cfg.height,
                hp: body.hp,
                alive: body.is_up(),
            })
            .collect();

        for (i, player) in self.state.players().iter().enumerate() {
            let Some(body) = self.state.body(player.id) else {
                continue;
            };
            // A downed player neither swings nor is swung at; a protected one
            // is excluded for the same reason, and `damage_player` enforces it
            // on the receiving side even if this filter is ever removed.
            if !body.is_up() || body.is_protected(now) {
                continue;
            }
            let eye = body.state.pos + Vec3::from_ints(0, 1, 0);
            let frame = self.script(i, now);
            let look = crate::input::look_direction(frame.yaw, 0);
            combat::melee(
                EntityId::new(u32::from(player.id.0), 0),
                player.team,
                eye,
                look,
                &weapon,
                &combatants,
                &mut self.events,
            );
        }

        // Copy the events out before touching the state: `damage_player` needs
        // the whole match state, and `self.events` is borrowed for the loop.
        let events = std::mem::take(&mut self.events);
        for event in &events {
            let Some(index) = self.index_of(event.target) else {
                continue;
            };
            let id = self.state.players()[index].id;
            // The applied amount, not the requested one: a protected or already
            // down player absorbs the hit, and counting it would make the
            // combat hash claim damage that never happened.
            let applied = self.state.damage_player(id, event.amount, now);
            self.damage_total += i64::from(applied);
        }
        self.events = events;
    }

    fn step_cores(&mut self, now: Tick) {
        if self.config.core_damage_per_s == 0 || now.0 % TICK_HZ != 0 {
            return;
        }
        let teams = self.state.layout().teams;
        for i in 0..teams {
            let victim = TeamId((i + 1) % teams);
            self.state
                .damage_core(victim, self.config.core_damage_per_s);
        }
    }

    fn index_of(&self, id: EntityId) -> Option<usize> {
        self.state
            .players()
            .iter()
            .position(|p| u32::from(p.id.0) == id.index())
    }

    /// A checkpoint of every subsystem.
    ///
    /// `movement` and `combat` both read the same bodies, deliberately: the
    /// split is by *question*, not by data. "Where is everyone?" and "how hurt
    /// is everyone?" diverge independently, and knowing which one moved first
    /// is the whole reason the frame has five fields.
    pub fn hash_frame(&self) -> HashFrame {
        let mut movement = Fnv::new();
        for body in self.state.bodies() {
            movement.vec(body.state.pos);
            movement.vec(body.state.vel);
            movement.byte(u8::from(body.state.on_ground));
            movement.byte(u8::from(body.state.in_water));
            movement.u32(body.respawn_at.map_or(u32::MAX, |t| t.0));
        }

        let mut buddies = Fnv::new();
        for team in self.state.teams() {
            buddies.u16(team.buddies.len() as u16);
            for buddy in &team.buddies {
                buddies.vec(buddy.pos);
                buddies.i32(buddy.hp);
                buddies.byte(buddy.state as u8);
                buddies.byte(buddy.stance as u8);
                buddies.u16(buddy.path.len() as u16);
            }
        }

        let mut combat = Fnv::new();
        for body in self.state.bodies() {
            combat.i32(body.hp);
            combat.u32(body.protect_until.map_or(u32::MAX, |t| t.0));
        }
        combat.u16(self.events.len() as u16);
        for event in &self.events {
            combat.i32(event.amount);
            combat.u32(event.target.index());
        }
        combat.u64(self.damage_total as u64);

        HashFrame {
            tick: self.tick.0,
            state: self.state.state_hash(),
            movement: movement.finish(),
            buddies: buddies.finish(),
            combat: combat.finish(),
        }
    }

    /// Run to completion, hashing every `every` ticks.
    ///
    /// The first frame is at tick 0 and the last is at the final tick, so a
    /// sequence always brackets the whole run.
    pub fn run(&mut self, every: u32, mut sink: impl FnMut(HashFrame)) {
        sink(self.hash_frame());
        while self.tick.0 < self.config.ticks && !self.state.is_over() {
            self.step();
            if self.tick.0 % every == 0 {
                sink(self.hash_frame());
            }
        }
    }
}

/// FNV-1a, matching `sim::state`'s hasher so the two agree in style.
struct Fnv(u64);

impl Fnv {
    fn new() -> Fnv {
        Fnv(0xcbf2_9ce4_8422_2325)
    }

    fn byte(&mut self, v: u8) {
        self.0 ^= u64::from(v);
        self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
    }

    fn u16(&mut self, v: u16) {
        self.byte((v & 0xFF) as u8);
        self.byte((v >> 8) as u8);
    }

    fn u32(&mut self, v: u32) {
        for i in 0..4 {
            self.byte((v >> (8 * i)) as u8);
        }
    }

    fn i32(&mut self, v: i32) {
        self.u32(v as u32);
    }

    fn u64(&mut self, v: u64) {
        for i in 0..8 {
            self.byte((v >> (8 * i)) as u8);
        }
    }

    fn vec(&mut self, v: Vec3) {
        self.i32(v.x.0);
        self.i32(v.y.0);
        self.i32(v.z.0);
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What one body did over a run, for the coverage assertions below.
    #[derive(Default)]
    struct Tally {
        moved: usize,
        downed: usize,
        revived: usize,
        protected_ticks: usize,
    }

    /// The harness has to actually reach the paths it claims to hash.
    ///
    /// A golden file proves *stability*, not *coverage*: a scenario where nobody
    /// ever dies would hash perfectly and prove nothing about respawning. This
    /// test is the counterweight, in the same spirit as the decay branch check in
    /// `state::tests::match_always_terminates`.
    #[test]
    fn the_scenario_reaches_movement_damage_and_respawn() {
        let mut s = Scenario::new(ScenarioConfig::duel(0x00C0_FFEE, 4_000));
        let start: Vec<Vec3> = s.state().bodies().iter().map(|b| b.state.pos).collect();
        let mut was_up: Vec<bool> = s.state().bodies().iter().map(|b| b.is_up()).collect();
        let mut tally = Tally::default();

        for _ in 0..4_000 {
            s.step();
            let now = s.tick();
            for (i, body) in s.state().bodies().iter().enumerate() {
                if body.state.pos != start[i] {
                    tally.moved += 1;
                }
                if was_up[i] && !body.is_up() {
                    tally.downed += 1;
                }
                if !was_up[i] && body.is_up() {
                    tally.revived += 1;
                }
                if body.is_protected(now) {
                    tally.protected_ticks += 1;
                }
                was_up[i] = body.is_up();
            }
        }

        println!(
            "duel, 200 s: {} body-ticks moved, {} downs, {} revives, {} protected ticks",
            tally.moved, tally.downed, tally.revived, tally.protected_ticks
        );
        assert!(tally.moved > 0, "no body ever moved");
        assert!(
            tally.downed > 0,
            "nobody was downed in 200 s of scripted melee, so the respawn path \
             is not in the golden sequence at all"
        );
        assert!(
            tally.revived > 0,
            "someone was downed but never came back, so the respawn timer is \
             not being run"
        );
        assert!(
            tally.protected_ticks > 0,
            "spawn protection never ran, so it is not covered by the goldens"
        );
        assert!(
            tally.revived + 2 >= tally.downed,
            "{} downs but only {} revives: at most two can be pending at the end \
             of a run, because only two bodies exist",
            tally.downed,
            tally.revived
        );
    }

    #[test]
    fn yaw_toward_covers_the_cardinal_directions() {
        let origin = Vec3::from_ints(0, 0, 0);
        // `atan2` is a cubic approximation with ~0.09 degrees of error, so a
        // cardinal lands within one yaw unit of the exact value rather than on
        // it. One unit is 1.4 degrees, which is far inside the 22.5-degree
        // melee cone the harness needs it for.
        for (target, want) in [
            (Vec3::from_ints(0, 0, 5), 0u8),
            (Vec3::from_ints(5, 0, 0), 64),
            (Vec3::from_ints(0, 0, -5), 128),
            (Vec3::from_ints(-5, 0, 0), 192),
        ] {
            let got = yaw_toward(origin, target);
            let diff = (i32::from(got) - i32::from(want)).rem_euclid(256);
            assert!(
                diff <= 1 || diff == 255,
                "yaw_toward({target:?}) = {got}, want {want}"
            );
        }
        // And the vector it produces points back at the target.
        let dir = crate::input::forward_from_yaw(yaw_toward(origin, Vec3::from_ints(3, 0, 3)));
        assert!(dir.x.to_f64() > 0.6 && dir.z.to_f64() > 0.6, "{dir:?}");
    }

    /// The script must actually bring bodies into reach.
    ///
    /// Starting apart is fine — the script walks toward the nearest other body —
    /// but if the closest approach over 10 s is still outside the sword's reach,
    /// the harness is hashing a fight that never happens.
    #[test]
    fn the_script_brings_bodies_within_melee_reach() {
        let mut s = Scenario::new(ScenarioConfig::duel(1, 1_000));
        let reach = tuning::embedded().weapons.sword.reach;
        let mut closest = crate::fixed::Fixed::from_int(1_000);

        for _ in 0..200 {
            s.step();
            let bodies = s.state().bodies();
            for (i, a) in bodies.iter().enumerate() {
                for b in bodies.iter().skip(i + 1) {
                    closest = closest.min(a.state.pos.distance(b.state.pos));
                }
            }
        }

        assert!(
            closest < reach,
            "closest approach over 10 s was {closest:?}, outside the {reach:?} reach — \
             the scripted melee never connects"
        );
    }
}
