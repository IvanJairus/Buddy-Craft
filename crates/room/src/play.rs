//! The headless match loop: airdrops, edits, combat, buddies and cores.
//!
//! The plan's room task list stops at the edit pipeline and airdrops and then
//! asks T-023 for bots that build and fight and T-024 for a full match. Nothing
//! in between joins those systems, so this module is the missing join. It is
//! **P-02**, a prerequisite recorded in the README the same way P-01 was: the
//! task graph assumed a loop that no task created.
//!
//! # Why this is not inside `Room`
//!
//! [`Room`](crate::Room) owns lifecycle, input queues and movement; those are
//! transport-facing concerns and they have their own tests. This loop owns
//! *gameplay* and takes input frames as a value, so the same code runs under a
//! WebSocket host (T-045), inside a Web Worker for offline play (T-048), and in
//! a `cargo test` with no runtime at all. The host composes the two.
//!
//! # The order of a tick
//!
//! ```text
//! respawn -> bot frames -> movement -> swings -> airdrops -> edits
//!         -> crafting -> buddies -> clock/outcome -> elimination spill
//! ```
//!
//! The order is a contract, not an implementation detail. Airdrops happen after
//! movement because pickup is "walked over it this tick". Edits happen after
//! pickups because a deposit can use a crate picked up on the same tick. Crafting
//! happens after edits because the block that completes a recipe was placed
//! this tick. The clock advances last so an elimination caused by combat is
//! settle-once, and the spilled crates are booked in the same tick.
//!
//! # What it deliberately leaves for later tasks
//!
//! Weapons and structures crafted by recipes are reported as events but there is
//! no player inventory yet: players fight with fists, and a crafted sword does
//! not change that. Projectiles, siege structures and wards arrive with their
//! own tasks. Bots only need fists to prove the loop, and a half-inventory would
//! be a second rule system with no test.

use protocol::{edit_action, edit_reject, Edit};
use sim::airdrop::Airdrops;
use sim::buddy::{Buddy, BuddyAction, BuddyContext, BuddyIntent, BuddyKind, Target};
use sim::combat::{self, Combatant, DamageEvent};
use sim::ids::{EntityId, PlayerId, TeamId, MAX_PLAYERS, MAX_TEAMS};
use sim::input::{self, InputFrame};
use sim::match_rules::{Layout, Outcome};
use sim::recipe::{CraftOutcome, RecipeBook, RecipeId, RecipeOutput};
use sim::state::{MatchState, RoomSetup, SetupError};
use sim::time::Tick;
use sim::{player, tuning, Vec3, VoxelQuery};
use world::gen::PAD_SIZE;
use world::WorldGen;

use crate::bot::{Bot, BotView};
use crate::edit::{EditPipeline, EditRequest, EditResult, EditWorld};

/// Frames plus edit messages for one tick, from humans and bots alike.
type TickInputs = (Vec<(PlayerId, InputFrame)>, Vec<(PlayerId, Edit)>);

/// Something observable happened this tick.
///
/// The host turns these into `S_EVENT`s; tests use them instead of reaching
/// into private counters. They are cleared at the start of every tick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MatchEvent {
    /// A drop was announced (the telegraph).
    AirdropSpawned {
        /// Landing site.
        site: [i32; 3],
        /// Crates it carries.
        crates: u32,
        /// Heavy drop.
        heavy: bool,
    },
    /// A drop touched down.
    AirdropLanded {
        /// Landing site.
        site: [i32; 3],
        /// Crates scattered.
        crates: u32,
    },
    /// A player picked crates up.
    Picked {
        /// Who.
        player: PlayerId,
        /// How many.
        count: u32,
    },
    /// A crate was placed on a pad.
    Deposited {
        /// Who.
        player: PlayerId,
        /// World cell.
        cell: [i32; 3],
    },
    /// A recipe fired.
    Crafted {
        /// Whose pad.
        team: TeamId,
        /// Which recipe.
        recipe: RecipeId,
        /// The buddy it spawned, when the recipe makes one.
        buddy: Option<BuddyKind>,
    },
    /// A player hit a core.
    CoreHit {
        /// The core's team.
        team: TeamId,
        /// HP actually removed after the multiplier.
        amount: i32,
        /// Who swung.
        by: PlayerId,
    },
    /// A player went down (not eliminated).
    PlayerDown {
        /// Who.
        player: PlayerId,
    },
    /// A team's core fell and the team left the match.
    Eliminated {
        /// Which team.
        team: TeamId,
    },
}

/// A complete, headless match.
pub struct Match {
    seed: u64,
    now: Tick,
    state: MatchState,
    airdrops: Airdrops,
    world: EditWorld,
    pipeline: EditPipeline,
    bots: [Option<Bot>; MAX_PLAYERS],
    /// Tick each seat's next melee swing is allowed.
    next_swing: [Tick; MAX_PLAYERS],
    /// Buddy handles start above the player ids so one `EntityId` space can
    /// address both without a tag.
    next_buddy: u32,
    seen_eliminated: [bool; MAX_TEAMS],
    events: Vec<MatchEvent>,
}

impl Match {
    /// Build a match from a seed.
    ///
    /// Bases, spawn points and airdrop sites all come from `world::gen`, so the
    /// server and a client that regenerates the map agree on every coordinate
    /// without a map download.
    pub fn new(seed: u64, setup: RoomSetup) -> Result<Match, SetupError> {
        let layout = Layout::resolve(setup.mode, setup.players(), setup.teams).ok_or(
            SetupError::NotALegalShape {
                mode: setup.mode,
                players: setup.players(),
                teams: setup.teams.unwrap_or(0),
            },
        )?;

        let gen = WorldGen::new(seed, layout.teams);
        let origins: Vec<[i32; 3]> = gen
            .bases()
            .iter()
            .map(|base| {
                [
                    base.pad_x - PAD_SIZE / 2,
                    base.y + 1,
                    base.pad_z - PAD_SIZE / 2,
                ]
            })
            .collect();

        let mut state = MatchState::new(setup, &origins)?;
        let world = EditWorld::new(seed, layout.teams);
        state.place_spawns(&world);

        // `DropSite::y` is the *surface block's* height, the same convention
        // `height_at` uses. A crate sits in the air cell above it, which is
        // also where a player's feet go; using the surface y would bury every
        // crate in the top block.
        let sites = gen
            .drop_sites()
            .iter()
            .map(|site| [site.x, site.y + 1, site.z])
            .collect();
        let airdrops = Airdrops::new(seed, state.players().len() as u32, sites);

        Ok(Match {
            seed,
            now: Tick::ZERO,
            state,
            airdrops,
            world,
            pipeline: EditPipeline::new(),
            bots: std::array::from_fn(|_| None),
            next_swing: [Tick::ZERO; MAX_PLAYERS],
            next_buddy: MAX_PLAYERS as u32,
            seen_eliminated: [false; MAX_TEAMS],
            events: Vec::new(),
        })
    }

    /// Attach a deterministic brain to one seat.
    ///
    /// Valid for bot seats and for a human seat in tests that need six
    /// bot-driven bodies while still giving D-05 a human to look for.
    pub fn add_bot(&mut self, player: PlayerId) -> bool {
        if !self.state.players().iter().any(|slot| slot.id == player) {
            return false;
        }
        self.bots[player.0 as usize] = Some(Bot::new(player));
        true
    }

    /// The seed the world was generated from.
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// The current tick.
    pub fn now(&self) -> Tick {
        self.now
    }

    /// The authoritative match state.
    pub fn state(&self) -> &MatchState {
        &self.state
    }

    /// The crate economy.
    pub fn airdrops(&self) -> &Airdrops {
        &self.airdrops
    }

    /// The mutable world the match edits.
    pub fn world(&self) -> &EditWorld {
        &self.world
    }

    /// Take every pending column diff, one message per column.
    ///
    /// A column with more than [`protocol::MAX_DIFF_ENTRIES`] pending cells is
    /// split across calls; the host keeps calling until this returns empty.
    pub fn take_diffs(&mut self) -> Vec<protocol::ChunkDiff> {
        let columns = self.world.pending_columns();
        columns
            .into_iter()
            .filter_map(|(cx, cz)| self.world.take_diff(cx, cz))
            .collect()
    }

    /// Every column a client joining now needs, in deterministic key order.
    pub fn join_diffs(&mut self) -> Vec<protocol::ChunkDiff> {
        let columns = self.world.edited_columns();
        columns
            .into_iter()
            .flat_map(|(cx, cz)| self.world.full_diffs(cx, cz))
            .collect()
    }

    /// Events emitted this tick.
    pub fn events(&self) -> &[MatchEvent] {
        &self.events
    }

    /// Apply raw damage to a core, through the same 0.35 multiplier the rest
    /// of the game uses. Siege structures and tests both enter here.
    pub fn damage_core(&mut self, team: TeamId, raw: i32) -> i32 {
        self.state.damage_core_at(team, raw, self.now)
    }

    /// Whether the match is over.
    pub fn is_over(&self) -> bool {
        self.state.is_over()
    }

    /// The current outcome.
    pub fn outcome(&self) -> Outcome {
        self.state.outcome()
    }

    /// Advance one tick. Human frames are supplied by the host; bot frames are
    /// produced inside.
    pub fn tick(&mut self, human: &[(PlayerId, InputFrame)]) -> Outcome {
        self.events.clear();
        if self.state.is_over() {
            return self.state.outcome();
        }
        self.now = self.now.next();

        self.state.respawn_due(self.now, &self.world);

        let (frames, edits) = self.collect_inputs(human);
        let mut fired = Vec::new();
        for (id, frame) in &frames {
            if let Ok(tick) = self.state.apply_input(*id, frame, &self.world) {
                if tick.fired {
                    fired.push((*id, frame.yaw, frame.pitch));
                }
            }
        }

        for (id, yaw, pitch) in fired {
            self.resolve_swing(id, yaw, pitch);
        }

        let bodies = self.living_bodies();
        let events = self.airdrops.tick(self.now, &bodies);
        for spawned in events.spawned {
            self.events.push(MatchEvent::AirdropSpawned {
                site: spawned.site,
                crates: spawned.crates,
                heavy: spawned.heavy,
            });
        }
        for landed in events.landed {
            self.events.push(MatchEvent::AirdropLanded {
                site: landed.site,
                crates: landed.crates,
            });
        }
        for (player, count) in events.picked {
            self.events.push(MatchEvent::Picked { player, count });
        }

        for (id, edit) in edits {
            let result = self.submit_edit(id, &edit);
            if result.accepted {
                self.events.push(MatchEvent::Deposited {
                    player: id,
                    cell: [edit.x as i32, edit.y as i32, edit.z as i32],
                });
            }
        }

        self.step_crafting();
        self.step_buddies();
        self.state.advance();
        self.step_eliminations();
        self.state.outcome()
    }

    /// Validate and apply one edit, consuming a carried crate when it is a
    /// placement.
    ///
    /// The deposit happens before the pipeline because the pipeline owns the
    /// single line that mutates the world and the pad together. A refused
    /// placement refunds the crate, so an illegal click costs nothing but the
    /// round trip.
    pub fn submit_edit(&mut self, player: PlayerId, message: &Edit) -> EditResult {
        let refuse = |reason: u8| EditResult {
            seq: message.seq,
            accepted: false,
            reason,
            broadcast: None,
            column: None,
        };

        let Some(slot) = self.state.players().iter().find(|slot| slot.id == player) else {
            return refuse(edit_reject::OCCUPIED);
        };
        let Some(body) = self.state.body(player) else {
            return refuse(edit_reject::OCCUPIED);
        };
        if !slot.life.is_alive() || !body.is_up() {
            return refuse(edit_reject::OCCUPIED);
        }
        let team = slot.team;
        let pos = [message.x as i32, message.y as i32, message.z as i32];
        let eye = body.state.pos + Vec3::from_ints(0, 1, 0);
        let placing = message.action == edit_action::PLACE;

        if placing {
            let inside_own_pad = self.state.pads().owner_of(pos) == Some(team);
            if inside_own_pad {
                if self.airdrops.carried(player) == 0 {
                    return refuse(edit_reject::RATE_LIMITED);
                }
                if self.airdrops.deposit(player, self.now, true) == 0 {
                    return refuse(edit_reject::RATE_LIMITED);
                }
            }
            let request = EditRequest {
                player,
                team,
                eye,
                message,
            };
            let result =
                self.pipeline
                    .submit(self.now, &request, &mut self.world, self.state.pads_mut());
            if !result.accepted && inside_own_pad {
                self.airdrops.refund(player, 1);
            }
            return result;
        }

        let request = EditRequest {
            player,
            team,
            eye,
            message,
        };
        self.pipeline
            .submit(self.now, &request, &mut self.world, self.state.pads_mut())
    }

    fn collect_inputs(&mut self, human: &[(PlayerId, InputFrame)]) -> TickInputs {
        let mut frames = human.to_vec();
        let mut edits = Vec::new();

        let Self {
            bots,
            state,
            airdrops,
            world,
            now,
            ..
        } = self;
        for slot in state.players() {
            let id = slot.id;
            let Some(bot) = bots[id.0 as usize].as_mut() else {
                continue;
            };
            let Some(body) = state.body(id) else {
                continue;
            };
            let view = BotView {
                now: *now,
                me: id,
                body,
                state,
                airdrops,
                world,
            };
            let output = bot.decide(&view);
            frames.push((id, output.frame));
            if let Some(edit) = output.edit {
                edits.push((id, edit));
            }
        }
        (frames, edits)
    }

    fn living_bodies(&self) -> Vec<(PlayerId, Vec3)> {
        self.state
            .players()
            .iter()
            .zip(self.state.bodies())
            .filter(|(slot, body)| slot.life.is_alive() && body.is_up())
            .map(|(slot, body)| (slot.id, body.state.pos))
            .collect()
    }

    fn combatants(&self) -> Vec<Combatant> {
        let cfg = self.state.move_config();
        let mut out = Vec::with_capacity(MAX_PLAYERS + 18);
        for (slot, body) in self.state.players().iter().zip(self.state.bodies()) {
            if !slot.life.is_alive() || !body.is_up() {
                continue;
            }
            out.push(Combatant {
                id: EntityId::new(u32::from(slot.id.0), 0),
                team: slot.team,
                pos: body.state.pos,
                half_width: cfg.half_width,
                height: cfg.height,
                hp: body.hp,
                alive: true,
            });
        }
        for team in self.state.teams() {
            for buddy in &team.buddies {
                if !buddy.alive() {
                    continue;
                }
                out.push(Combatant {
                    id: buddy.id,
                    team: buddy.team,
                    pos: buddy.pos,
                    half_width: cfg.half_width,
                    height: cfg.height,
                    hp: buddy.hp,
                    alive: true,
                });
            }
        }
        out
    }

    fn resolve_swing(&mut self, id: PlayerId, yaw: u8, pitch: u8) {
        let index = id.0 as usize;
        if index >= MAX_PLAYERS || self.now.0 < self.next_swing[index].0 {
            return;
        }
        let Some(slot) = self.state.players().iter().find(|slot| slot.id == id) else {
            return;
        };
        let Some(body) = self.state.body(id) else {
            return;
        };
        if !slot.life.is_alive() || !body.is_up() {
            return;
        }
        let team = slot.team;
        let eye = body.state.pos + Vec3::from_ints(0, 1, 0);
        let look = input::look_direction(yaw, pitch);
        let weapon = tuning::embedded().weapons.fists;
        self.next_swing[index] = self.now.advance(weapon.cooldown_ticks);

        let combatants = self.combatants();
        let mut events: Vec<DamageEvent> = Vec::new();
        let hits = combat::melee(
            EntityId::new(u32::from(id.0), 0),
            team,
            eye,
            look,
            &weapon,
            &combatants,
            &mut events,
        );
        if hits > 0 {
            for event in &events {
                self.apply_damage(event);
            }
            return;
        }
        if let Some(victim) = self.core_in_reach(team, eye, &weapon) {
            let removed = self.state.damage_core_at(victim, weapon.damage, self.now);
            if removed > 0 {
                self.events.push(MatchEvent::CoreHit {
                    team: victim,
                    amount: removed,
                    by: id,
                });
            }
        }
    }

    fn core_in_reach(
        &self,
        attacker: TeamId,
        eye: Vec3,
        weapon: &sim::combat::WeaponStats,
    ) -> Option<TeamId> {
        let half = sim::Fixed::from_raw(1 << 15);
        let mut best: Option<(sim::Fixed, TeamId)> = None;
        for team in self.state.teams() {
            if team.id == attacker || team.base.is_eliminated() || !team.base.core.is_alive() {
                continue;
            }
            for cell in team.base.core.cells() {
                let centre = Vec3::new(
                    sim::Fixed::from_int(cell[0]) + half,
                    sim::Fixed::from_int(cell[1]) + half,
                    sim::Fixed::from_int(cell[2]) + half,
                );
                let distance = eye.distance(centre);
                if distance > weapon.reach {
                    continue;
                }
                if best.is_none_or(|(d, _)| distance < d) {
                    best = Some((distance, team.id));
                }
            }
        }
        best.map(|(_, team)| team)
    }

    fn apply_damage(&mut self, event: &DamageEvent) {
        if event.target.index() < MAX_PLAYERS as u32 {
            let id = PlayerId(event.target.index() as u8);
            let was_up = self.state.body(id).is_some_and(|body| body.is_up());
            let applied = self.state.damage_player(id, event.amount, self.now);
            if applied > 0 && was_up {
                let down = self.state.body(id).is_some_and(|body| !body.is_up());
                if down {
                    let pos = self
                        .state
                        .body(id)
                        .map(|body| body.state.pos)
                        .unwrap_or(Vec3::ZERO);
                    self.airdrops.on_death(id, pos, self.now);
                    self.events.push(MatchEvent::PlayerDown { player: id });
                }
            }
        } else {
            self.state.damage_buddy(event.target, event.amount);
        }
    }

    fn step_crafting(&mut self) {
        for index in 0..self.state.layout().teams as usize {
            let team = TeamId(index as u8);
            let Some(outcome) = self.state.craft(team, self.now) else {
                continue;
            };
            let CraftOutcome::Crafted(craft) = outcome else {
                continue;
            };
            let Some(recipe) = RecipeBook::embedded().get(craft.recipe) else {
                continue;
            };
            let buddy = match recipe.output {
                RecipeOutput::Buddy(kind) => {
                    self.spawn_buddy(team, kind);
                    Some(kind)
                }
                _ => None,
            };
            self.events.push(MatchEvent::Crafted {
                team,
                recipe: craft.recipe,
                buddy,
            });
        }
    }

    fn spawn_buddy(&mut self, team: TeamId, kind: BuddyKind) {
        if !self.state.can_add_buddy(team, kind) {
            return;
        }
        let Some(pad) = self.state.pads().get(team) else {
            return;
        };
        let pos = player::spawn_point(&self.world, self.state.move_config(), pad);
        let owner = self
            .state
            .players()
            .iter()
            .find(|slot| slot.team == team && slot.life.is_alive())
            .map(|slot| slot.id)
            .unwrap_or(PlayerId(0));
        let id = EntityId::new(self.next_buddy, 0);
        self.next_buddy = self.next_buddy.saturating_add(1);
        let buddy = Buddy::new(id, tuning::buddy(kind), team, owner, pos);
        let _ = self.state.add_buddy(buddy);
    }

    fn targets(&self) -> Vec<Target> {
        let max_hp = tuning::embedded().player.hp;
        let mut out = Vec::with_capacity(MAX_PLAYERS + 18);
        for (slot, body) in self.state.players().iter().zip(self.state.bodies()) {
            if !slot.life.is_alive() || !body.is_up() {
                continue;
            }
            out.push(Target {
                id: EntityId::new(u32::from(slot.id.0), 0),
                team: slot.team,
                pos: body.state.pos,
                hp: body.hp,
                max_hp,
                role: None,
            });
        }
        for team in self.state.teams() {
            for buddy in &team.buddies {
                if !buddy.alive() {
                    continue;
                }
                let stats = tuning::buddy(buddy.kind);
                out.push(Target {
                    id: buddy.id,
                    team: buddy.team,
                    pos: buddy.pos,
                    hp: buddy.hp,
                    max_hp: stats.hp,
                    role: Some(buddy.kind),
                });
            }
        }
        out
    }

    fn step_buddies(&mut self) {
        let targets = self.targets();
        let mut actions: Vec<(BuddyKind, BuddyAction)> = Vec::new();
        {
            let world = &self.world;
            let now = self.now;
            let teams = self.state.teams_mut();
            for team in teams.iter_mut() {
                let allies: Vec<Target> = targets
                    .iter()
                    .filter(|target| target.team == team.id)
                    .copied()
                    .collect();
                let enemies: Vec<Target> = targets
                    .iter()
                    .filter(|target| target.team != team.id)
                    .copied()
                    .collect();
                for buddy in &mut team.buddies {
                    if !buddy.alive() {
                        continue;
                    }
                    let owner = targets
                        .iter()
                        .find(|target| target.id == EntityId::new(u32::from(buddy.owner.0), 0))
                        .copied();
                    let ctx = BuddyContext {
                        now,
                        owner,
                        allies: &allies,
                        enemies: &enemies,
                    };
                    let intent: BuddyIntent = buddy.think(&ctx, world);
                    move_buddy(world, buddy, intent.move_to);
                    if !matches!(intent.action, BuddyAction::None) {
                        actions.push((buddy.kind, intent.action));
                    }
                }
            }
        }
        for (kind, action) in actions {
            self.apply_buddy_action(kind, action);
        }
    }

    fn apply_buddy_action(&mut self, kind: BuddyKind, action: BuddyAction) {
        let stats = tuning::buddy(kind);
        match action {
            BuddyAction::Attack(target) => {
                if target.index() < MAX_PLAYERS as u32 {
                    self.hit_player(PlayerId(target.index() as u8), stats.weapon.damage);
                } else {
                    self.state.damage_buddy(target, stats.weapon.damage);
                }
            }
            BuddyAction::Heal { target, amount } => {
                if target.index() < MAX_PLAYERS as u32 {
                    let id = PlayerId(target.index() as u8);
                    let max = tuning::embedded().player.hp;
                    if let Some(body) = self.state.body_mut(id) {
                        body.hp = (body.hp + amount).min(max);
                    }
                } else {
                    self.state.heal_buddy(target, amount);
                }
            }
            BuddyAction::None => {}
        }
    }

    fn hit_player(&mut self, id: PlayerId, amount: i32) {
        let was_up = self.state.body(id).is_some_and(|body| body.is_up());
        let applied = self.state.damage_player(id, amount, self.now);
        if applied > 0 && was_up && self.state.body(id).is_some_and(|body| !body.is_up()) {
            let pos = self
                .state
                .body(id)
                .map(|body| body.state.pos)
                .unwrap_or(Vec3::ZERO);
            self.airdrops.on_death(id, pos, self.now);
            self.events.push(MatchEvent::PlayerDown { player: id });
        }
    }

    fn step_eliminations(&mut self) {
        for index in 0..self.state.layout().teams as usize {
            let team = TeamId(index as u8);
            if self.seen_eliminated[index] {
                continue;
            }
            let Some(state) = self.state.team(team) else {
                continue;
            };
            if !state.base.is_eliminated() {
                continue;
            }
            self.seen_eliminated[index] = true;
            let bodies: Vec<(PlayerId, Vec3)> = self
                .state
                .players()
                .iter()
                .zip(self.state.bodies())
                .filter(|(slot, _)| slot.team == team)
                .map(|(slot, body)| (slot.id, body.state.pos))
                .collect();
            for (id, pos) in bodies {
                self.airdrops.on_death(id, pos, self.now);
            }
            self.events.push(MatchEvent::Eliminated { team });
        }
    }
}

/// Walk a buddy toward `goal`, stopping at any cell it cannot stand in.
///
/// Buddies have no gravity step of their own; they walk on the surface they
/// were spawned on and follow the pathfinder's horizontal waypoints. A buddy
/// that meets a wall stops rather than climbing it, which is a legible failure
/// and cannot put a unit inside terrain.
fn move_buddy(world: &dyn VoxelQuery, buddy: &mut Buddy, goal: Vec3) {
    let delta = Vec3::new(goal.x - buddy.pos.x, sim::ZERO, goal.z - buddy.pos.z);
    let distance = delta.horizontal_length();
    if distance <= sim::Fixed::from_raw(1_000) {
        return;
    }
    let speed = tuning::buddy(buddy.kind).speed * sim::movement::tick_dt();
    let step = speed.min(distance);
    let direction = delta.normalize();
    let next = buddy.pos + direction * step;
    let cell = next.floor_to_ints();
    let feet_y = buddy.pos.y.round_int();
    if !world.solid(cell[0], feet_y, cell[2]) && !world.solid(cell[0], feet_y + 1, cell[2]) {
        buddy.pos = Vec3::new(next.x, buddy.pos.y, next.z);
    }
}
