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
//! This is not the room loop. It has no network, no airdrops, no crate carrying
//! and no respawn rules from §4.8 — those arrive with the room (M2+). What it
//! does have is the property that matters here: **a pure function from a seed
//! to a sequence of hashes**, covering every arithmetic path in `sim` that a
//! platform difference could perturb.
//!
//! The scripted inputs are deliberately boring — four directions, a jump every
//! 50 ticks, a crate every 25 — because a determinism failure should be
//! reproducible by reading a 30-line script, not by bisecting a bot AI.
//!
//! # Two deliberate simplifications
//!
//! * **Players respawn after 8 s at their spawn point**, which is §4.8's number
//!   but not its rules: there is no spawn protection and no "died while
//!   mid-air" handling. Those belong to the room loop.
//! * **Buddies are spawned directly** rather than crafted, so the buddy path is
//!   exercised without the harness having to build a 9-crate recipe first.

use crate::buddy::{Buddy, BuddyContext, BuddyKind, Target};
use crate::combat::{self, Combatant, DamageEvent};
use crate::ids::{EntityId, PlayerId, TeamId};
use crate::input::{buttons, InputFrame};
use crate::match_rules::{Layout, Mode};
use crate::movement::{self, MoveConfig, MoveIntent, MoveState};
use crate::state::{ring_pad_origins, MatchState, RoomSetup};
use crate::time::{Tick, TICK_HZ};
use crate::tuning;
use crate::vec::Vec3;
use crate::world_query::VoxelGrid;

/// Ticks a dead player waits before respawning (§4.8: 8 s).
const RESPAWN_TICKS: u32 = 8 * TICK_HZ;

/// Ticks between a player's melee swings (0.45 s at 20 Hz).
const SWING_TICKS: u32 = 9;

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

/// A player's simulated body.
#[derive(Clone, Debug)]
struct Mover {
    state: MoveState,
    hp: i32,
    /// Tick the player respawns, if dead.
    respawn_at: Option<Tick>,
    prev_buttons: u8,
    spawn: Vec3,
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

/// A running scenario.
#[derive(Clone, Debug)]
pub struct Scenario {
    config: ScenarioConfig,
    state: MatchState,
    world: VoxelGrid,
    cfg: MoveConfig,
    movers: Vec<Mover>,
    seed: u64,
    tick: Tick,
    /// Crates placed so far, used to walk the scripted pad pattern.
    placed: u32,
    /// Damage events emitted this tick.
    events: Vec<DamageEvent>,
    /// Damage events emitted over the whole run, for the combat hash.
    damage_total: i64,
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
        let state = MatchState::new(config.setup, &origins).expect("legal room");

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

        let players = state.players().to_vec();
        let movers = players
            .iter()
            .enumerate()
            .map(|(i, p)| {
                // Deterministic, distinct per team, and well away from the two
                // walls so the scripted circle-strafe has room to run.
                let spawn = Vec3::from_ints(
                    -6 + i32::from(p.team.0) * 4 + (i as i32 % 2) * 2,
                    1,
                    -4 + (i as i32 % 3) * 3,
                );
                Mover {
                    state: MoveState::at(spawn),
                    hp: tuning::embedded().player.hp,
                    respawn_at: None,
                    prev_buttons: 0,
                    spawn,
                }
            })
            .collect();

        Scenario {
            config,
            state,
            world,
            cfg: MoveConfig::embedded(),
            movers,
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
        &self.movers[index].state
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
    /// Four directions changing every two seconds, a jump every 50 ticks, and
    /// a yaw that sweeps the full circle. Deterministic, and enough to keep
    /// every movement branch (walk, jump, land, step up, block) in play.
    ///
    /// The seed offsets both the direction phase and the jump cadence. Without
    /// that the field would be decorative — two fixtures with different seeds
    /// would run identical input and the `a_different_seed` test would have
    /// nothing to catch.
    fn script(&self, index: usize, now: Tick) -> InputFrame {
        let t = now.0;
        let phase = (t / 40) as usize + index + (self.seed % 4) as usize;
        let (move_x, move_z) = match phase % 4 {
            0 => (1, 0),
            1 => (0, 1),
            2 => (-1, 0),
            _ => (0, -1),
        };
        InputFrame {
            tick: now,
            move_x,
            move_z,
            yaw: ((t.wrapping_mul(3).wrapping_add(self.seed as u32)) % 256) as u8,
            pitch: 0,
            buttons: if (t + self.seed as u32) % 50 == 0 {
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
        let dt = movement::tick_dt();
        let frames: Vec<InputFrame> = (0..self.movers.len())
            .map(|i| self.script(i, now))
            .collect();
        for (i, mover) in self.movers.iter_mut().enumerate() {
            if let Some(at) = mover.respawn_at {
                if now.0 >= at.0 {
                    mover.respawn_at = None;
                    mover.hp = tuning::embedded().player.hp;
                    mover.state = MoveState::at(mover.spawn);
                } else {
                    continue;
                }
            }
            let frame = frames[i];
            let intent = MoveIntent::from_frame(&frame, mover.prev_buttons);
            mover.prev_buttons = frame.buttons;
            movement::step(&self.world, &self.cfg, &mut mover.state, intent, dt);
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
            .movers
            .iter()
            .zip(self.state.players())
            .map(|(mover, player)| Target {
                id: EntityId::new(u32::from(player.id.0), 0),
                team: player.team,
                pos: mover.state.pos,
                hp: mover.hp,
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
        let combatants: Vec<Combatant> = self
            .movers
            .iter()
            .zip(self.state.players())
            .map(|(mover, player)| Combatant {
                id: EntityId::new(u32::from(player.id.0), 0),
                team: player.team,
                pos: mover.state.pos,
                half_width: self.cfg.half_width,
                height: self.cfg.height,
                hp: mover.hp,
                alive: mover.hp > 0,
            })
            .collect();

        for (i, player) in self.state.players().iter().enumerate() {
            if self.movers[i].hp <= 0 {
                continue;
            }
            let eye = self.movers[i].state.pos + Vec3::from_ints(0, 1, 0);
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

        for event in &self.events {
            let Some(index) = self.index_of(event.target) else {
                continue;
            };
            if self.movers[index].respawn_at.is_some() {
                continue;
            }
            self.movers[index].hp = (self.movers[index].hp - event.amount).max(0);
            self.damage_total += i64::from(event.amount);
            if self.movers[index].hp == 0 {
                self.movers[index].respawn_at = Some(now.advance(RESPAWN_TICKS));
            }
        }
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
    pub fn hash_frame(&self) -> HashFrame {
        let mut movement = Fnv::new();
        for mover in &self.movers {
            movement.vec(mover.state.pos);
            movement.vec(mover.state.vel);
            movement.byte(u8::from(mover.state.on_ground));
            movement.u32(mover.respawn_at.map_or(u32::MAX, |t| t.0));
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
        for mover in &self.movers {
            combat.i32(mover.hp);
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
