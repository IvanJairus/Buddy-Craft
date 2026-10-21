//! The match: teams, cores, players and the rules that tie them together.
//!
//! # What this module is, and what it is not
//!
//! This is the *match-level* state: who is on which team, which cores are
//! standing, how much overtime decay has been applied, and whether the match is
//! over. It is not the simulation — movement, projectiles, airdrops and crate
//! carry live in their own modules and are ticked by the room loop (M2+). The
//! split is what lets the rules here be tested in a few milliseconds without
//! standing up a world.
//!
//! # Why the outcome is recomputed rather than asserted
//!
//! Nothing sets `outcome` directly. [`MatchState::settle`] runs the eliminations
//! that are due, then asks [`check_outcome`] what the state now implies. That
//! ordering matters: if the outcome were computed before elimination ran, a
//! player whose core just fell would still count as a live human for one tick,
//! and the D-05 "no humans" check would fire a tick late — or not at all, if
//! the core and the last human died together.
//!
//! # The state hash
//!
//! [`MatchState::state_hash`] covers every field that can affect gameplay. It
//! is the thing T-015's golden fixtures compare, so a field that affects play
//! but not the hash is a desync that no test would catch — which is why the
//! mutation test in this module walks every field rather than sampling a few.

use crate::base::{Base, PlayerLife, PlayerSlot};
use crate::buddy::Buddy;
use crate::ids::{PlayerId, TeamId};
use crate::input::InputFrame;
use crate::match_rules::{check_outcome, Layout, MatchRules, Mode, Outcome, TeamStatus};
use crate::movement::MoveConfig;
use crate::pad::Pads;
use crate::player::{self, NoBody, PlayerBody, PlayerTick};
use crate::recipe::{self, CraftOutcome, RecipeBook};
use crate::time::{Tick, TICK_HZ};
use crate::tuning;
use crate::vec::Vec3;
use crate::world_query::{EmptyWorld, VoxelQuery};

/// Everything needed to seat a room.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RoomSetup {
    /// FFA or Teams.
    pub mode: Mode,
    /// Team count for Teams mode. Ignored (and must be `None` or equal to the
    /// player count) in FFA.
    pub teams: Option<u8>,
    /// Human players.
    pub humans: u8,
    /// Bots filling the remaining slots (D-05).
    pub bots: u8,
}

impl RoomSetup {
    /// A room with only humans.
    pub fn humans(mode: Mode, teams: Option<u8>, count: u8) -> RoomSetup {
        RoomSetup {
            mode,
            teams,
            humans: count,
            bots: 0,
        }
    }

    /// Total players.
    pub fn players(&self) -> u8 {
        self.humans + self.bots
    }
}

/// Why a room could not be created.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SetupError {
    /// The `(mode, players, teams)` triple is not a row of §4.7.
    NotALegalShape {
        /// Requested mode.
        mode: Mode,
        /// Total players requested.
        players: u8,
        /// Team count requested, or the player count in FFA.
        teams: u8,
    },
    /// A room needs at least one human; a bot-only match has nobody to play it.
    NoHumans,
    /// Pad origins were not supplied for every team.
    MissingPadOrigins {
        /// Teams the layout requires.
        expected: usize,
        /// Origins supplied.
        got: usize,
    },
}

impl core::fmt::Display for SetupError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SetupError::NotALegalShape {
                mode,
                players,
                teams,
            } => write!(
                f,
                "{players} players over {teams} teams is not a legal {} room",
                mode.name()
            ),
            SetupError::NoHumans => f.write_str("a room needs at least one human"),
            SetupError::MissingPadOrigins { expected, got } => {
                write!(f, "expected {expected} pad origins, got {got}")
            }
        }
    }
}

impl std::error::Error for SetupError {}

/// One team's match-level state.
#[derive(Clone, Debug)]
pub struct TeamState {
    /// Which team.
    pub id: TeamId,
    /// The core and its elimination state.
    pub base: Base,
    /// Living buddies. Dead ones are removed, never resurrected (§4.6).
    pub buddies: Vec<Buddy>,
}

impl TeamState {
    /// Buddies of one role.
    pub fn count_kind(&self, kind: crate::buddy::BuddyKind) -> usize {
        self.buddies.iter().filter(|b| b.kind == kind).count()
    }
}

/// A running match.
///
/// # The two player lists
///
/// `players` is the seat — what elimination sees, and what the protocol reports.
/// `bodies` is the body — what moves. They are index-parallel and, by
/// construction, `players[i].id == PlayerId(i)`: [`Layout::seat`] numbers the
/// seats in order and [`MatchState::new`] preserves that order. The invariant is
/// what lets [`MatchState::body`] be an index rather than a search, and it is
/// asserted at construction so a future reordering fails loudly instead of
/// silently addressing the wrong body.
#[derive(Clone, Debug)]
pub struct MatchState {
    mode: Mode,
    layout: Layout,
    elapsed: Tick,
    teams: Vec<TeamState>,
    players: Vec<PlayerSlot>,
    /// Index-parallel with `players`. See the struct docs.
    bodies: Vec<PlayerBody>,
    pads: Pads,
    outcome: Outcome,
    rules: MatchRules,
    /// Resolved once at construction: movement reads eleven `Fixed`s per tick
    /// and re-parsing the tuning table per player per tick would dominate the
    /// tick budget.
    cfg: MoveConfig,
    /// Total decay applied so far, so the cumulative schedule can be applied
    /// as a difference rather than re-applied every tick.
    decay_applied: i32,
}

impl MatchState {
    /// Seat a room and build its bases.
    ///
    /// `pad_origins` is one world position per team, in team order. Worldgen
    /// owns the real positions (T-006); [`ring_pad_origins`] is the
    /// deterministic stand-in used by tests and offline play.
    pub fn new(setup: RoomSetup, pad_origins: &[[i32; 3]]) -> Result<MatchState, SetupError> {
        let players = setup.players();
        let layout = Layout::resolve(setup.mode, players, setup.teams).ok_or(
            SetupError::NotALegalShape {
                mode: setup.mode,
                players,
                teams: setup.teams.unwrap_or(players),
            },
        )?;
        if setup.humans == 0 {
            return Err(SetupError::NoHumans);
        }
        if pad_origins.len() != layout.teams as usize {
            return Err(SetupError::MissingPadOrigins {
                expected: layout.teams as usize,
                got: pad_origins.len(),
            });
        }

        let mut pads = Pads::new();
        let mut teams = Vec::with_capacity(layout.teams as usize);
        for (i, origin) in pad_origins.iter().enumerate() {
            let id = pads.push(crate::pad::Pad::new(*origin));
            debug_assert_eq!(id, TeamId(i as u8));
            let base = Base::new(id, Base::core_origin_for_pad(*origin));
            base.stamp_protected_volume(pads.get_mut(id).expect("pad was just pushed"));
            teams.push(TeamState {
                id,
                base,
                buddies: Vec::new(),
            });
        }

        let seats = layout.seat(setup.humans, setup.bots);
        let players: Vec<PlayerSlot> = seats
            .iter()
            .enumerate()
            .map(|(i, seat)| {
                let id = PlayerId(i as u8);
                if seat.bot {
                    PlayerSlot::bot(id, seat.team)
                } else {
                    PlayerSlot::alive(id, seat.team)
                }
            })
            .collect();

        let cfg = MoveConfig::embedded();

        // Spawn points need the pad, which needs the world to know whether a
        // column is clear. `new` has no world, so it uses `EmptyWorld` and
        // produces the *nominal* point — the first pad column outside the core
        // that has no crate in it. The room calls `place_spawns` with the real
        // world when the match goes live, and that is where terrain starts to
        // matter. Building it here anyway means a match state is never without
        // a body, which is what lets `state_hash` cover positions from tick 0.
        let bodies: Vec<PlayerBody> = players
            .iter()
            .map(|slot| {
                let pos = match pads.get(slot.team) {
                    Some(pad) => player::spawn_point(&EmptyWorld, &cfg, pad),
                    None => Vec3::ZERO,
                };
                PlayerBody::spawned(pos)
            })
            .collect();

        debug_assert!(
            players
                .iter()
                .enumerate()
                .all(|(i, slot)| slot.id == PlayerId(i as u8)),
            "seat order and player id must agree; `MatchState::body` relies on it"
        );

        Ok(MatchState {
            mode: setup.mode,
            layout,
            elapsed: Tick::ZERO,
            teams,
            players,
            bodies,
            pads,
            outcome: Outcome::Running,
            rules: MatchRules::from_tuning(),
            cfg,
            decay_applied: 0,
        })
    }

    // -- accessors ---------------------------------------------------------

    /// The room's mode.
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// The resolved room shape.
    pub fn layout(&self) -> Layout {
        self.layout
    }

    /// Ticks since the match started.
    pub fn elapsed(&self) -> Tick {
        self.elapsed
    }

    /// The current outcome.
    pub fn outcome(&self) -> Outcome {
        self.outcome
    }

    /// True once the match is over.
    pub fn is_over(&self) -> bool {
        self.outcome.is_over()
    }

    /// Every player slot, humans and bots, in seat order.
    pub fn players(&self) -> &[PlayerSlot] {
        &self.players
    }

    /// Every player body, index-parallel with [`MatchState::players`].
    pub fn bodies(&self) -> &[PlayerBody] {
        &self.bodies
    }

    /// One player's body.
    pub fn body(&self, id: PlayerId) -> Option<&PlayerBody> {
        self.bodies.get(self.index_of(id)?)
    }

    /// One player's body, mutably. Damage and respawn go through the methods
    /// below rather than here, so the two ways of dying stay in one place.
    pub fn body_mut(&mut self, id: PlayerId) -> Option<&mut PlayerBody> {
        let index = self.index_of(id)?;
        self.bodies.get_mut(index)
    }

    /// The resolved movement configuration.
    pub fn move_config(&self) -> &MoveConfig {
        &self.cfg
    }

    /// The index of `id` in the seat list, or `None` if there is no such seat.
    fn index_of(&self, id: PlayerId) -> Option<usize> {
        let index = id.0 as usize;
        match self.players.get(index) {
            Some(slot) if slot.id == id => Some(index),
            _ => None,
        }
    }

    /// Every team, in team order.
    pub fn teams(&self) -> &[TeamState] {
        &self.teams
    }

    /// The pads.
    pub fn pads(&self) -> &Pads {
        &self.pads
    }

    /// Mutable pads. Only the match loop and the recipe matcher use this;
    /// players go through `Pads::place`, which enforces ownership.
    pub fn pads_mut(&mut self) -> &mut Pads {
        &mut self.pads
    }

    /// One team.
    pub fn team(&self, id: TeamId) -> Option<&TeamState> {
        self.teams.get(id.0 as usize)
    }

    /// Mutable teams, for the tick loop that drives buddy AI and the bases.
    ///
    /// Match-level rules stay on `MatchState`; this exists so a caller does not
    /// have to copy buddy state out and back every tick.
    pub fn teams_mut(&mut self) -> &mut [TeamState] {
        &mut self.teams
    }

    /// The match rules in force.
    pub fn rules(&self) -> &MatchRules {
        &self.rules
    }

    // -- simulation --------------------------------------------------------

    /// Advance the clock by one tick, applying decay and settling the outcome.
    ///
    /// Returns the outcome so a caller cannot forget to check it.
    pub fn advance(&mut self) -> Outcome {
        if self.outcome.is_over() {
            return self.outcome;
        }
        self.elapsed = self.elapsed.advance(1);
        self.apply_decay();
        self.settle();
        self.outcome
    }

    /// Apply raw damage to a team's core, settling the outcome if it falls.
    pub fn damage_core(&mut self, team: TeamId, raw: i32) -> i32 {
        if self.outcome.is_over() {
            return 0;
        }
        let Some(state) = self.teams.get_mut(team.0 as usize) else {
            return 0;
        };
        let removed = state.base.damage(raw, self.elapsed);
        if state.base.is_destroyed() {
            self.settle();
        }
        removed
    }

    // -- players -----------------------------------------------------------

    /// Apply one resolved input frame to one player's body.
    ///
    /// Returns the movement result, or why the frame was dropped. Dropping is
    /// normal, not an error: a player waiting out a respawn still sends input
    /// (they are still connected), and an eliminated player is still in the
    /// room until the match ends.
    ///
    /// The buttons are recorded even when the frame is dropped, so a player who
    /// dies holding fire does not come back and fire instantly — the edge is
    /// consumed against what they were actually holding.
    pub fn apply_input<W: VoxelQuery + ?Sized>(
        &mut self,
        id: PlayerId,
        frame: &InputFrame,
        world: &W,
    ) -> Result<PlayerTick, NoBody> {
        let index = self.index_of(id).ok_or(NoBody::NoSuchPlayer)?;

        let Self {
            players,
            bodies,
            cfg,
            ..
        } = self;

        let slot = players.get(index).ok_or(NoBody::NoSuchPlayer)?;
        let body = bodies.get_mut(index).ok_or(NoBody::NoSuchPlayer)?;

        let previous = body.prev_buttons;
        body.prev_buttons = frame.buttons;

        if !slot.life.is_alive() {
            return Err(NoBody::Eliminated);
        }
        if !body.is_up() {
            return Err(NoBody::Down);
        }

        Ok(player::apply(body, frame, previous, world, cfg))
    }

    /// Damage one player, honouring spawn protection.
    ///
    /// Returns the HP actually removed. Zero means "nothing happened", which is
    /// the right answer for three different reasons — protected, already down,
    /// or eliminated — because in all three cases the attacker's hit did nothing
    /// and the client should draw no hit marker.
    pub fn damage_player(&mut self, id: PlayerId, amount: i32, now: Tick) -> i32 {
        if amount <= 0 {
            return 0;
        }
        let Some(index) = self.index_of(id) else {
            return 0;
        };
        if !self.players[index].life.is_alive() {
            return 0;
        }
        let body = &mut self.bodies[index];
        if !body.is_up() || body.is_protected(now) {
            return 0;
        }
        let applied = amount.min(body.hp);
        body.hp -= applied;
        if body.hp == 0 {
            // Inlined rather than delegating to `kill_player`: the body is
            // already mutably borrowed, and a second lookup would have to
            // re-check everything this function has just checked.
            player::down(body, now);
        }
        applied
    }

    /// Kill one player outright, starting their respawn timer.
    ///
    /// Returns false when the player cannot die: already down, already
    /// eliminated, or not seated.
    pub fn kill_player(&mut self, id: PlayerId, now: Tick) -> bool {
        let Some(index) = self.index_of(id) else {
            return false;
        };
        if !self.players[index].life.is_alive() {
            return false;
        }
        let body = &mut self.bodies[index];
        if !body.is_up() {
            return false;
        }
        player::down(body, now);
        true
    }

    /// Bring back every player whose timer has expired.
    ///
    /// Called at the *start* of a tick, before input is applied, so a player
    /// whose 8 s expires on tick N can act on tick N rather than N+1.
    ///
    /// Eliminated players are skipped even if their timer somehow fires: `life`
    /// is one-way, and a respawn that ignored it would resurrect a team that
    /// D-02 removed from the match.
    pub fn respawn_due<W: VoxelQuery + ?Sized>(&mut self, now: Tick, world: &W) -> usize {
        let Self {
            players,
            bodies,
            cfg,
            ..
        } = self;

        let mut revived = 0;
        for (index, body) in bodies.iter_mut().enumerate() {
            let Some(slot) = players.get(index) else {
                continue;
            };
            if !slot.life.is_alive() {
                continue;
            }
            let Some(at) = body.respawn_at else {
                continue;
            };
            if now.0 < at.0 {
                continue;
            }
            // The stored spawn point may have been built over during the
            // player's death, so it is re-checked rather than trusted.
            let pos = player::nearest_free(world, cfg, body.spawn);
            body.respawn(pos, now);
            revived += 1;
        }
        revived
    }

    /// Recompute every spawn point from its team's pad, using `world`.
    ///
    /// Called once when a match goes live. [`MatchState::new`] has no world, so
    /// it can only produce nominal spawn points; this is where terrain, water
    /// and anything already standing on the pad are taken into account.
    ///
    /// Returns how many bodies moved, so a caller can log a map whose pads are
    /// unexpectedly obstructed.
    pub fn place_spawns<W: VoxelQuery + ?Sized>(&mut self, world: &W) -> usize {
        let Self {
            players,
            bodies,
            pads,
            cfg,
            ..
        } = self;

        let mut moved = 0;
        for (index, body) in bodies.iter_mut().enumerate() {
            let Some(slot) = players.get(index) else {
                continue;
            };
            let Some(pad) = pads.get(slot.team) else {
                continue;
            };
            let pos = player::spawn_point(world, cfg, pad);
            if body.spawn != pos {
                moved += 1;
            }
            body.spawn = pos;
            if body.is_up() {
                // A body that is already standing is moved too: at match start
                // the nominal point and the real one are usually the same, but
                // when they differ the player must not start inside a block.
                body.state = crate::movement::MoveState::at(pos);
            }
        }
        moved
    }

    /// Override one player's spawn point and put them there.
    ///
    /// The escape hatch for a harness that wants bodies somewhere specific —
    /// the determinism scenario places players where their scripted inputs will
    /// actually meet, which a ring of pads 40 m apart would not do. Returns
    /// false for a seat that does not exist.
    pub fn set_spawn(&mut self, id: PlayerId, pos: Vec3) -> bool {
        let Some(index) = self.index_of(id) else {
            return false;
        };
        let body = &mut self.bodies[index];
        body.spawn = pos;
        body.state = crate::movement::MoveState::at(pos);
        true
    }

    /// Apply the overtime decay that is due at the current tick.
    fn apply_decay(&mut self) {
        let max_hp = tuning::embedded().core.hp;
        let due = self.rules.cumulative_decay(self.elapsed, max_hp);
        let delta = due - self.decay_applied;
        if delta <= 0 {
            return;
        }
        self.decay_applied = due;
        let now = self.elapsed;
        for team in &mut self.teams {
            team.base.decay(delta, now);
        }
    }

    /// Run any eliminations that are due, then recompute the outcome.
    ///
    /// Idempotent: calling it twice in a tick does nothing the second time,
    /// because `Base::eliminate` is idempotent and `check_outcome` is a pure
    /// function of the state it leaves behind.
    pub fn settle(&mut self) {
        let Self {
            teams,
            players,
            pads,
            bodies,
            ..
        } = self;

        for team in teams.iter_mut() {
            if team.base.is_destroyed() && !team.base.is_eliminated() {
                let pad = pads
                    .get_mut(team.id)
                    .expect("every team has a pad by construction");
                let record = team.base.eliminate(pad, players);
                // "Their buddy slots are freed" (§4.8): the team is out, so its
                // units leave the simulation rather than standing around dead.
                for buddy in &mut team.buddies {
                    buddy.kill();
                }
                team.buddies.clear();
                // D-02 kills the players, and a killed player's body has to
                // agree: a team that is out of the match must not leave six
                // bodies at full health for the snapshot to report.
                for id in &record.players_killed {
                    if let Some(body) = bodies.get_mut(id.0 as usize) {
                        player::eliminate(body);
                    }
                }
            }
        }

        if self.outcome.is_over() {
            return;
        }
        let statuses: Vec<TeamStatus> = teams
            .iter()
            .map(|team| TeamStatus {
                id: team.id,
                core_alive: !team.base.is_destroyed(),
                human_alive: players
                    .iter()
                    .any(|p| p.team == team.id && p.is_live_human()),
            })
            .collect();
        self.outcome = check_outcome(&statuses);
    }

    // -- buddies -----------------------------------------------------------

    /// Buddies this team may still field.
    pub fn buddy_headroom(&self, team: TeamId) -> usize {
        let used = self.team(team).map_or(0, |t| t.buddies.len());
        (self.layout.buddy_cap as usize).saturating_sub(used)
    }

    /// True when `team` may add one more buddy of `kind`.
    ///
    /// Two caps apply and both are per team (§4.7): the roster's per-role cap
    /// and the mode's total. A role cap above the total is rejected at load, so
    /// the two can never contradict each other.
    pub fn can_add_buddy(&self, team: TeamId, kind: crate::buddy::BuddyKind) -> bool {
        let Some(state) = self.team(team) else {
            return false;
        };
        if self.buddy_headroom(team) == 0 {
            return false;
        }
        state.count_kind(kind) < tuning::buddy(kind).cap as usize
    }

    /// Add a buddy, or report which cap refused it.
    pub fn add_buddy(&mut self, buddy: Buddy) -> Result<(), BuddyCapReached> {
        if !self.can_add_buddy(buddy.team, buddy.kind) {
            return Err(BuddyCapReached {
                team: buddy.team,
                kind: buddy.kind,
            });
        }
        let team = buddy.team;
        self.teams[team.0 as usize].buddies.push(buddy);
        Ok(())
    }

    // -- crafting ----------------------------------------------------------

    /// Run one crafting pass for `team`, enforcing the caps.
    ///
    /// A recipe whose output would exceed a cap is refused as
    /// [`CraftOutcome::Blocked`] and **consumes nothing** (§4.5.4), so a player
    /// standing at the cap keeps their crates.
    pub fn craft(&mut self, team: TeamId, now: Tick) -> Option<CraftOutcome> {
        if self.outcome.is_over() {
            return None;
        }
        let book = RecipeBook::embedded();
        let elapsed_s = now.0 / TICK_HZ;
        let allowed: Vec<bool> = book
            .recipes()
            .iter()
            .map(|r| self.may_craft(team, r, elapsed_s))
            .collect();
        let pads = &mut self.pads;
        let pad = pads.get_mut(team)?;
        recipe::tick_with(pad, book, now, elapsed_s, |id| {
            allowed.get(id.0 as usize).copied().unwrap_or(false)
        })
    }

    /// Whether `team` may craft `recipe` right now.
    fn may_craft(&self, team: TeamId, recipe: &recipe::Recipe, elapsed_s: u32) -> bool {
        if !recipe.unlocked_at(elapsed_s) {
            return false;
        }
        match recipe.output.buddy() {
            None => true,
            Some(kind) => self.can_add_buddy(team, kind),
        }
    }

    // -- hashing -----------------------------------------------------------

    /// A hash of everything that can affect gameplay.
    ///
    /// Compared by T-015's golden fixtures. Adding a field here without adding
    /// it to the hash is the bug this function exists to make impossible to
    /// miss: `hash_changes_on_every_mutation` walks each one.
    pub fn state_hash(&self) -> u64 {
        let mut h = Hash::new();
        h.u8(match self.mode {
            Mode::Ffa => 0,
            Mode::Teams => 1,
        });
        h.u8(self.layout.teams);
        h.u8(self.layout.per_team);
        h.u8(self.layout.buddy_cap);
        h.u32(self.elapsed.0);
        h.i32(self.decay_applied);
        match self.outcome {
            Outcome::Running => h.u8(0),
            Outcome::Ended { winner, reason } => {
                h.u8(1);
                h.u8(match winner {
                    None => 0xFF,
                    Some(t) => t.0,
                });
                h.u8(match reason {
                    crate::match_rules::EndReason::LastTeamStanding => 0,
                    crate::match_rules::EndReason::Draw => 1,
                    crate::match_rules::EndReason::NoHumans => 2,
                });
            }
        }

        for team in &self.teams {
            h.u8(team.id.0);
            h.i32(team.base.core.hp());
            h.u8(u8::from(team.base.is_destroyed()));
            h.u8(u8::from(team.base.is_eliminated()));
            h.u32(team.base.destroyed_at().map_or(u32::MAX, |t| t.0));
            h.u32(team.base.eliminated_at().map_or(u32::MAX, |t| t.0));
            if let Some(pad) = self.pads.get(team.id) {
                h.u64(pad.hash());
                h.u8(u8::from(pad.is_sealed()));
            }
            h.u16(team.buddies.len() as u16);
            for buddy in &team.buddies {
                h.u32(buddy.id.index());
                h.u16(buddy.id.generation());
                h.u8(buddy.kind as u8);
                h.u8(buddy.team.0);
                h.i32(buddy.hp);
                h.u8(buddy.state as u8);
                h.u8(buddy.stance as u8);
                h.vec(buddy.pos);
            }
        }

        h.u16(self.players.len() as u16);
        for player in &self.players {
            h.u8(player.id.0);
            h.u8(player.team.0);
            h.u8(match player.life {
                PlayerLife::Alive => 0,
                PlayerLife::Dead => 1,
            });
            h.u8(u8::from(player.bot));
        }

        // Bodies. `prev_buttons` is here on purpose: it decides whether the
        // *next* tick sees a jump or a shot, so two states that differ only in
        // that byte will diverge one tick later — which is exactly the kind of
        // divergence a checkpoint taken before the fact has to catch.
        h.u16(self.bodies.len() as u16);
        for body in &self.bodies {
            h.vec(body.state.pos);
            h.vec(body.state.vel);
            h.u8(u8::from(body.state.on_ground));
            h.u8(u8::from(body.state.in_water));
            h.i32(body.hp);
            h.vec(body.spawn);
            h.u32(body.respawn_at.map_or(u32::MAX, |t| t.0));
            h.u32(body.protect_until.map_or(u32::MAX, |t| t.0));
            h.u8(body.yaw);
            h.u8(body.pitch);
            h.u8(body.prev_buttons);
        }

        h.finish()
    }
}

/// A cap refused a buddy.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BuddyCapReached {
    /// The team at its cap.
    pub team: TeamId,
    /// The role that was refused.
    pub kind: crate::buddy::BuddyKind,
}

impl core::fmt::Display for BuddyCapReached {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "team {} is at its {:?} cap", self.team.0, self.kind)
    }
}

/// Deterministic pad origins on a circle, for tests and offline play.
///
/// Worldgen owns the real base sites; this exists so a match can be built
/// without one, and so the result is identical on every target (integer yaw,
/// no trigonometry beyond the fixed-point table).
pub fn ring_pad_origins(layout: &Layout, radius: i32, ground_y: i32) -> Vec<[i32; 3]> {
    let mut origins = Vec::with_capacity(layout.teams as usize);
    for i in 0..layout.teams {
        let angle = (256 / layout.teams as u16 * i as u16) as u8;
        let r = crate::fixed::Fixed::from_int(radius);
        origins.push([
            (r * crate::fixed::cos_yaw(angle)).round_int(),
            ground_y,
            (r * crate::fixed::sin_yaw(angle)).round_int(),
        ]);
    }
    origins
}

/// FNV-1a, one field at a time.
///
/// Not `Hash`: the standard library's `Hasher` contract does not promise a
/// stable output across releases, and this hash is compared against committed
/// fixtures. FNV-1a is eight lines and will not change under us.
struct Hash(u64);

impl Hash {
    fn new() -> Hash {
        Hash(0xcbf2_9ce4_8422_2325)
    }

    fn u8(&mut self, v: u8) {
        self.0 ^= u64::from(v);
        self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
    }

    fn u16(&mut self, v: u16) {
        self.u8((v & 0xFF) as u8);
        self.u8((v >> 8) as u8);
    }

    fn u32(&mut self, v: u32) {
        for i in 0..4 {
            self.u8((v >> (8 * i)) as u8);
        }
    }

    fn i32(&mut self, v: i32) {
        self.u32(v as u32);
    }

    fn u64(&mut self, v: u64) {
        for i in 0..8 {
            self.u8((v >> (8 * i)) as u8);
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
    use crate::buddy::{BuddyKind, BuddyState, Stance};
    use crate::ids::EntityId;

    fn setup(mode: Mode, teams: Option<u8>, humans: u8, bots: u8) -> RoomSetup {
        RoomSetup {
            mode,
            teams,
            humans,
            bots,
        }
    }

    fn room(mode: Mode, teams: Option<u8>, humans: u8, bots: u8) -> MatchState {
        let layout = Layout::resolve(mode, humans + bots, teams).expect("legal");
        let origins = ring_pad_origins(&layout, 96, 41);
        MatchState::new(setup(mode, teams, humans, bots), &origins).expect("room")
    }

    fn duel() -> MatchState {
        room(Mode::Ffa, None, 2, 0)
    }

    fn buddy(team: TeamId, kind: BuddyKind, id: u32) -> Buddy {
        Buddy::new(
            EntityId::new(id, 0),
            tuning::buddy(kind),
            team,
            crate::ids::PlayerId(0),
            Vec3::from_ints(10 + id as i32, 41, 10),
        )
    }

    /// Enough raw damage to destroy a 10,000 HP core through the multiplier.
    const KILL_RAW: i32 = 30_000;

    #[test]
    fn a_room_seats_the_right_shape() {
        let state = room(Mode::Teams, Some(2), 2, 2);
        assert_eq!(state.layout().teams, 2);
        assert_eq!(state.layout().per_team, 2);
        assert_eq!(state.layout().buddy_cap, 5);
        assert_eq!(state.teams().len(), 2);
        assert_eq!(state.players().len(), 4);
        assert_eq!(state.pads().len(), 2);
        // One human per team, then the bots fill.
        let seats: Vec<(TeamId, bool)> = state.players().iter().map(|p| (p.team, p.bot)).collect();
        assert_eq!(
            seats,
            vec![
                (TeamId(0), false),
                (TeamId(1), false),
                (TeamId(0), true),
                (TeamId(1), true)
            ]
        );
        assert_eq!(state.outcome(), Outcome::Running);
    }

    #[test]
    fn illegal_rooms_are_rejected() {
        let origins = [[0, 41, 0], [10, 41, 0], [20, 41, 0]];
        // 3 players cannot split into 2 teams.
        let err = MatchState::new(setup(Mode::Teams, Some(2), 3, 0), &origins).unwrap_err();
        assert!(matches!(err, SetupError::NotALegalShape { players: 3, .. }));
        // A bot-only room has nobody to play it.
        let err = MatchState::new(setup(Mode::Ffa, None, 0, 2), &origins).unwrap_err();
        assert_eq!(err, SetupError::NoHumans);
        // Wrong number of pad origins.
        let err = MatchState::new(setup(Mode::Ffa, None, 2, 0), &origins).unwrap_err();
        assert_eq!(
            err,
            SetupError::MissingPadOrigins {
                expected: 2,
                got: 3
            }
        );
    }

    #[test]
    fn the_core_volume_is_protected_from_the_first_tick() {
        let mut state = duel();
        // §4.5.1 rule 5: nothing can be built on the core, from tick zero.
        let origin = state.pads().get(TeamId(0)).unwrap().origin();
        let core = Base::core_origin_for_pad(origin);
        assert_eq!(
            state.pads_mut().place(TeamId(0), core, true),
            Err(crate::pad::PlaceError::Protected)
        );
    }

    #[test]
    fn core_damage_eliminates_and_ends_a_duel() {
        let mut state = duel();
        assert_eq!(state.damage_core(TeamId(0), KILL_RAW), 10_000);
        assert!(state.team(TeamId(0)).unwrap().base.is_eliminated());
        assert_eq!(
            state.outcome(),
            Outcome::Ended {
                winner: Some(TeamId(1)),
                reason: crate::match_rules::EndReason::LastTeamStanding,
            }
        );
        assert!(state.players()[0].life == PlayerLife::Dead);
        assert!(state.players()[1].life == PlayerLife::Alive);
    }

    #[test]
    fn damage_after_the_match_is_over_does_nothing() {
        let mut state = duel();
        state.damage_core(TeamId(0), KILL_RAW);
        assert!(state.is_over());
        assert_eq!(state.damage_core(TeamId(1), KILL_RAW), 0);
        assert!(state.team(TeamId(1)).unwrap().base.core.is_alive());
    }

    #[test]
    fn a_buddy_cap_refuses_the_craft_and_keeps_the_crates() {
        // AC: "a craft attempt at cap consumes nothing".
        let mut state = room(Mode::Ffa, None, 2, 0);
        let team = TeamId(0);
        let cap = state.layout().buddy_cap as usize;
        assert_eq!(cap, 3, "§4.7 FFA with 2 players");

        for i in 0..cap {
            state
                .add_buddy(buddy(team, BuddyKind::Swordsman, i as u32))
                .expect("under the cap");
        }
        assert_eq!(state.team(team).unwrap().buddies.len(), cap);
        assert_eq!(state.buddy_headroom(team), 0);
        assert!(!state.can_add_buddy(team, BuddyKind::Swordsman));

        let err = state
            .add_buddy(buddy(team, BuddyKind::Archer, 99))
            .unwrap_err();
        assert_eq!(
            err,
            BuddyCapReached {
                team,
                kind: BuddyKind::Archer
            }
        );
        assert_eq!(state.team(team).unwrap().buddies.len(), cap);
        // The other team is unaffected.
        assert_eq!(state.buddy_headroom(TeamId(1)), cap);
    }

    #[test]
    fn the_per_role_cap_binds_before_the_team_cap() {
        // §4.6 caps snipers at 2 per team; §4.7 caps the team at 5 in 2v2.
        let mut state = room(Mode::Teams, Some(2), 4, 0);
        let team = TeamId(0);
        assert_eq!(state.layout().buddy_cap, 5, "§4.7 2v2");
        state
            .add_buddy(buddy(team, BuddyKind::Sniper, 0))
            .expect("first sniper");
        state
            .add_buddy(buddy(team, BuddyKind::Sniper, 1))
            .expect("second sniper");
        assert!(
            !state.can_add_buddy(team, BuddyKind::Sniper),
            "the sniper cap is 2 even though the team has 3 slots left"
        );
        assert_eq!(state.buddy_headroom(team), 3);
        // A different role still fits.
        assert!(state.can_add_buddy(team, BuddyKind::Swordsman));
    }

    #[test]
    fn craft_at_cap_returns_blocked_and_consumes_nothing() {
        let mut state = room(Mode::Ffa, None, 2, 0);
        let team = TeamId(0);
        let cap = state.layout().buddy_cap as usize;
        for i in 0..cap {
            state
                .add_buddy(buddy(team, BuddyKind::Swordsman, i as u32))
                .expect("under the cap");
        }

        // Build R04_SWORDSMAN: a 3x3 slab of 9 crates on the pad, positioned
        // from the pad's own origin rather than a guessed world coordinate.
        let origin = state.pads().get(team).expect("pad").origin();
        for x in 0..3 {
            for z in 0..3 {
                let cell = [origin[0] + x, origin[1], origin[2] + z];
                assert_eq!(
                    state.pads_mut().place(team, cell, true),
                    Ok(true),
                    "{cell:?}"
                );
            }
        }
        let crates_before = state.pads().get(team).unwrap().count();
        assert_eq!(crates_before, 9);

        // Settle the stability window, then attempt the craft.
        let mut outcome = None;
        for tick in 0..40 {
            if let Some(o) = state.craft(team, Tick(tick)) {
                outcome = Some(o);
                break;
            }
        }
        let outcome = outcome.expect("the pad should stabilise and match");
        assert!(
            matches!(outcome, CraftOutcome::Blocked(_)),
            "expected Blocked, got {outcome:?}"
        );
        assert!(!outcome.crafted());
        assert_eq!(
            state.pads().get(team).unwrap().count(),
            9,
            "a blocked craft must consume nothing"
        );
        assert_eq!(state.team(team).unwrap().buddies.len(), cap);
    }

    #[test]
    fn overtime_decay_eventually_ends_a_stalemate() {
        // Nobody attacks; the clock has to do it.
        let mut state = duel();
        let bound = state.rules().decay_only_duration_s();
        let mut ticks = 0u32;
        while !state.is_over() && ticks <= bound * TICK_HZ + TICK_HZ {
            state.advance();
            ticks += 1;
        }
        assert!(state.is_over(), "the match never terminated");
        // Both cores fall on the same tick, so this is a draw.
        assert_eq!(
            state.outcome(),
            Outcome::Ended {
                winner: None,
                reason: crate::match_rules::EndReason::Draw,
            }
        );
        assert_eq!(
            state.teams()[0].base.destroyed_at(),
            state.teams()[1].base.destroyed_at()
        );
    }

    #[test]
    fn decay_does_not_apply_before_overtime() {
        let mut state = duel();
        for _ in 0..(22 * 60 * TICK_HZ) {
            state.advance();
        }
        assert_eq!(
            state.teams()[0].base.core.hp(),
            10_000,
            "decay started early"
        );
        assert_eq!(state.teams()[1].base.core.hp(), 10_000);
    }

    #[test]
    fn match_always_terminates() {
        // AC: 100 random matches all end within 25 simulated minutes.
        //
        // The random part is the room shape and each team's damage rate. One
        // seed in ten is a pure stalemate — every team deals zero damage — so
        // the decay path is exercised rather than assumed. A stalemate cannot
        // be decided by combat at all, so it is the decay bound that has to
        // hold there, and the README records why that bound is 47 minutes
        // rather than 25.
        let decay_bound = {
            let probe = room(Mode::Ffa, None, 2, 0);
            probe.rules().decay_only_duration_s()
        };
        let mut stalemates = 0;
        let mut longest_contested = 0u32;

        for seed in 0..100u64 {
            let mut rng = seed;
            let teams = 2 + (lcg(&mut rng) % 5) as u8;
            let forced_stalemate = seed % 10 == 0;
            // Raw damage per second: 20..=140, or zero for a team that never
            // attacks. A zero here is a turtle, not an error.
            let dps: Vec<i32> = (0..teams)
                .map(|_| {
                    if forced_stalemate || lcg(&mut rng) % 10 == 0 {
                        0
                    } else {
                        20 + (lcg(&mut rng) % 121) as i32
                    }
                })
                .collect();

            // A team is unkillable when *its* attacker is a turtle — not when
            // it is one. Two or more survivors means no amount of combat can
            // end the match, and only decay can.
            let survivors = (0..teams as usize)
                .filter(|i| dps[(i + teams as usize - 1) % teams as usize] == 0)
                .count();
            let stalemate = survivors >= 2;

            let mut state = room(Mode::Ffa, None, teams, 0);
            let mut ticks = 0u32;
            let cap = (decay_bound + 60) * TICK_HZ;
            while !state.is_over() && ticks < cap {
                // One second's worth of damage, applied at the second boundary
                // so per-hit rounding stays close to the continuous rate.
                if ticks % TICK_HZ == 0 {
                    for (i, damage) in dps.iter().enumerate() {
                        if *damage > 0 {
                            let victim = TeamId(((i + 1) % teams as usize) as u8);
                            state.damage_core(victim, *damage);
                        }
                    }
                }
                state.advance();
                ticks += 1;
            }

            assert!(
                state.is_over(),
                "seed {seed} ({teams} teams, dps {dps:?}) never terminated; \
                 decay is supposed to be a guarantee"
            );
            let minutes = ticks as f32 / (TICK_HZ as f32 * 60.0);
            assert!(
                minutes <= decay_bound as f32 / 60.0,
                "seed {seed} outlived the decay bound at {minutes:.1} min"
            );

            if stalemate {
                stalemates += 1;
                // The surviving cores decay together, so this is a draw and it
                // lands on the bound rather than before it.
                assert_eq!(
                    state.outcome().reason(),
                    Some(crate::match_rules::EndReason::Draw),
                    "seed {seed}: {survivors} unkillable teams must end in a draw"
                );
                assert!(
                    minutes > 25.0,
                    "seed {seed}: a stalemate ended at {minutes:.1} min, so something \
                     other than decay decided it"
                );
            } else {
                longest_contested = longest_contested.max(ticks);
                assert!(
                    minutes <= 25.0,
                    "seed {seed} could be decided by combat but took {minutes:.1} min \
                     (dps {dps:?}, {survivors} unkillable)"
                );
            }
        }

        println!(
            "100 matches: longest contested {:.1} min, {stalemates} stalemates decided by decay",
            longest_contested as f32 / (TICK_HZ as f32 * 60.0)
        );
        assert!(stalemates > 0, "the decay branch was never exercised");
        assert!(stalemates < 100, "the combat branch was never exercised");
    }

    /// A deterministic 64-bit LCG, so a failing seed replays exactly.
    fn lcg(state: &mut u64) -> u64 {
        *state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        *state >> 33
    }

    #[test]
    fn hash_is_stable_and_covers_every_field() {
        let state = room(Mode::Teams, Some(2), 2, 2);
        let base = state.state_hash();
        assert_eq!(base, state.state_hash(), "the hash must be a pure function");

        // Every mutation below must move the hash. If one does not, the field
        // it touches is invisible to the determinism harness (T-015).
        /// One named mutation of a match state.
        type Mutation = (&'static str, Box<dyn Fn(&mut MatchState)>);
        let mutations: Vec<Mutation> = vec![
            (
                "elapsed",
                Box::new(|s: &mut MatchState| s.elapsed = Tick(1)),
            ),
            (
                "decay_applied",
                Box::new(|s: &mut MatchState| s.decay_applied = 200),
            ),
            (
                "layout.buddy_cap",
                Box::new(|s: &mut MatchState| s.layout.buddy_cap += 1),
            ),
            (
                "outcome",
                Box::new(|s: &mut MatchState| {
                    s.outcome = Outcome::Ended {
                        winner: None,
                        reason: crate::match_rules::EndReason::Draw,
                    }
                }),
            ),
            (
                "core hp",
                Box::new(|s: &mut MatchState| {
                    s.teams[0].base.damage(25, Tick(0));
                }),
            ),
            (
                "core destroyed",
                Box::new(|s: &mut MatchState| {
                    s.teams[0].base.damage(KILL_RAW, Tick(7));
                }),
            ),
            (
                "pad",
                Box::new(|s: &mut MatchState| {
                    s.pads.get_mut(TeamId(0)).unwrap().set([1, 0, 1], true);
                }),
            ),
            (
                "pad sealed",
                Box::new(|s: &mut MatchState| {
                    s.pads.get_mut(TeamId(0)).unwrap().seal();
                }),
            ),
            (
                "player life",
                Box::new(|s: &mut MatchState| s.players[0].life = PlayerLife::Dead),
            ),
            (
                "player bot",
                Box::new(|s: &mut MatchState| s.players[0].bot = !s.players[0].bot),
            ),
            (
                "buddy count",
                Box::new(|s: &mut MatchState| {
                    s.teams[0]
                        .buddies
                        .push(buddy(TeamId(0), BuddyKind::Mage, 5));
                }),
            ),
            (
                "buddy hp",
                Box::new(|s: &mut MatchState| {
                    s.teams[0].buddies[0].hp -= 1;
                }),
            ),
            (
                "buddy state",
                Box::new(|s: &mut MatchState| {
                    s.teams[0].buddies[0].state = BuddyState::Engage;
                }),
            ),
            (
                "buddy stance",
                Box::new(|s: &mut MatchState| {
                    s.teams[0].buddies[0].stance = Stance::Assault;
                }),
            ),
            (
                "buddy position",
                Box::new(|s: &mut MatchState| {
                    s.teams[0].buddies[0].pos = Vec3::from_ints(99, 41, 99);
                }),
            ),
            (
                "player position",
                Box::new(|s: &mut MatchState| {
                    s.bodies[0].state.pos = Vec3::from_ints(7, 41, 7);
                }),
            ),
            (
                "player velocity",
                Box::new(|s: &mut MatchState| {
                    s.bodies[0].state.vel = Vec3::from_ints(0, -4, 0);
                }),
            ),
            (
                "player on_ground",
                Box::new(|s: &mut MatchState| {
                    s.bodies[0].state.on_ground = !s.bodies[0].state.on_ground;
                }),
            ),
            (
                "player in_water",
                Box::new(|s: &mut MatchState| {
                    s.bodies[0].state.in_water = !s.bodies[0].state.in_water;
                }),
            ),
            (
                "player hp",
                Box::new(|s: &mut MatchState| {
                    s.bodies[0].hp -= 1;
                }),
            ),
            (
                "player spawn",
                Box::new(|s: &mut MatchState| {
                    s.bodies[0].spawn = Vec3::from_ints(1, 41, 1);
                }),
            ),
            (
                "player respawn_at",
                Box::new(|s: &mut MatchState| {
                    s.bodies[0].respawn_at = Some(Tick(500));
                }),
            ),
            (
                "player protect_until",
                Box::new(|s: &mut MatchState| {
                    s.bodies[0].protect_until = Some(Tick(500));
                }),
            ),
            (
                "player prev_buttons",
                Box::new(|s: &mut MatchState| {
                    s.bodies[0].prev_buttons = 0b1010;
                }),
            ),
            (
                "player yaw",
                Box::new(|s: &mut MatchState| {
                    s.bodies[0].yaw = 200;
                }),
            ),
            (
                "player pitch",
                Box::new(|s: &mut MatchState| {
                    s.bodies[0].pitch = 40;
                }),
            ),
        ];

        for (name, mutate) in mutations {
            let mut copy = state.clone();
            // The buddy-dependent mutations need a buddy to exist.
            if name.starts_with("buddy") && name != "buddy count" {
                copy.teams[0]
                    .buddies
                    .push(buddy(TeamId(0), BuddyKind::Mage, 5));
                let with_buddy = copy.state_hash();
                mutate(&mut copy);
                assert_ne!(with_buddy, copy.state_hash(), "{name} is not hashed");
            } else {
                mutate(&mut copy);
                assert_ne!(base, copy.state_hash(), "{name} is not hashed");
            }
        }
    }

    #[test]
    fn the_same_setup_always_hashes_the_same() {
        // Determinism starts here: two identical rooms, built independently,
        // must be indistinguishable.
        let a = room(Mode::Teams, Some(3), 4, 2);
        let b = room(Mode::Teams, Some(3), 4, 2);
        assert_eq!(a.state_hash(), b.state_hash());
        assert_eq!(a.layout(), b.layout());
        // And a different shape differs.
        let c = room(Mode::Teams, Some(2), 4, 2);
        assert_ne!(a.state_hash(), c.state_hash());
    }

    // -- player bodies -----------------------------------------------------

    /// A flat floor at `y = 40`, which is one block under every pad the test
    /// helper lays out at `y = 41`.
    fn flat_world() -> crate::world_query::VoxelGrid {
        let mut w = crate::world_query::VoxelGrid::new([-112, 38, -112], [224, 8, 224]);
        w.fill_layer(40, true);
        w
    }

    fn forward(tick: u32) -> InputFrame {
        InputFrame {
            tick: Tick(tick),
            move_z: 1,
            ..Default::default()
        }
    }

    #[test]
    fn a_frame_moves_the_body() {
        let mut state = duel();
        let world = flat_world();
        let before = state.body(PlayerId(0)).unwrap().state.pos;

        for tick in 0..20 {
            let out = state
                .apply_input(PlayerId(0), &forward(tick), &world)
                .expect("a seated, living player accepts input");
            assert!(out.flags.on_ground, "the pad surface is solid ground");
        }

        let after = state.body(PlayerId(0)).unwrap().state.pos;
        let travelled = after.horizontal_distance(before).to_f64();
        // 20 ticks at 4.5 m/s is 4.5 m; the body spends the first tick
        // settling onto the floor, so allow a little slack.
        assert!(
            (3.5..=4.6).contains(&travelled),
            "expected about 4.5 m of travel, got {travelled:.2} m"
        );
    }

    #[test]
    fn an_unknown_seat_is_refused() {
        let mut state = duel();
        let world = flat_world();
        assert_eq!(
            state.apply_input(PlayerId(9), &forward(0), &world),
            Err(NoBody::NoSuchPlayer)
        );
    }

    #[test]
    fn an_eliminated_player_never_moves_again() {
        // D-02 is one-way: no input, no respawn, no exception.
        let mut state = duel();
        let world = flat_world();
        state.damage_core(TeamId(0), KILL_RAW);
        assert_eq!(state.players()[0].life, PlayerLife::Dead);
        assert!(
            !state.body(PlayerId(0)).unwrap().is_up(),
            "elimination must down the body, not only the seat"
        );

        assert_eq!(
            state.apply_input(PlayerId(0), &forward(0), &world),
            Err(NoBody::Eliminated)
        );

        // Even if something armed a timer on the body, `respawn_due` refuses:
        // `life` is checked before `respawn_at`.
        state.bodies[0].respawn_at = Some(Tick(1));
        assert_eq!(state.respawn_due(Tick(1_000), &world), 0);
        assert!(!state.body(PlayerId(0)).unwrap().is_up());
        assert_eq!(state.body(PlayerId(0)).unwrap().hp, 0);
    }

    #[test]
    fn a_down_player_is_ignored_but_their_buttons_are_recorded() {
        let mut state = duel();
        let world = flat_world();
        state.kill_player(PlayerId(0), Tick(0));
        assert!(!state.body(PlayerId(0)).unwrap().is_up());

        let held = InputFrame {
            tick: Tick(1),
            buttons: crate::input::buttons::FIRE,
            ..Default::default()
        };
        assert_eq!(
            state.apply_input(PlayerId(0), &held, &world),
            Err(NoBody::Down)
        );
        assert_eq!(
            state.body(PlayerId(0)).unwrap().prev_buttons,
            crate::input::buttons::FIRE,
            "a dropped frame must still advance the button history, or the \
             first shot after a respawn is eaten"
        );
    }

    #[test]
    fn respawn_puts_the_body_back_at_its_spawn_on_the_tick_the_timer_expires() {
        let mut state = duel();
        let world = flat_world();
        let spawn = state.body(PlayerId(0)).unwrap().spawn;

        state.kill_player(PlayerId(0), Tick(10));
        let due = 10 + crate::player::respawn_ticks();
        assert_eq!(state.body(PlayerId(0)).unwrap().respawn_at, Some(Tick(due)));

        // One tick early: nothing.
        assert_eq!(state.respawn_due(Tick(due - 1), &world), 0);
        assert!(!state.body(PlayerId(0)).unwrap().is_up());

        // On the tick: back, at the spawn point, protected.
        assert_eq!(state.respawn_due(Tick(due), &world), 1);
        let body = state.body(PlayerId(0)).unwrap();
        assert!(body.is_up());
        assert_eq!(body.state.pos, spawn);
        assert!(body.is_protected(Tick(due)));
        assert!(!body.is_protected(Tick(due + crate::player::protect_ticks())));
    }

    #[test]
    fn spawn_protection_absorbs_damage_until_it_expires() {
        let mut state = duel();
        let world = flat_world();
        state.kill_player(PlayerId(0), Tick(0));
        let due = crate::player::respawn_ticks();
        state.respawn_due(Tick(due), &world);
        assert!(state.body(PlayerId(0)).unwrap().is_protected(Tick(due)));

        assert_eq!(
            state.damage_player(PlayerId(0), 40, Tick(due)),
            0,
            "a protected player takes nothing"
        );
        assert_eq!(state.body(PlayerId(0)).unwrap().hp, 100);

        let after = due + crate::player::protect_ticks();
        assert_eq!(
            state.damage_player(PlayerId(0), 40, Tick(after)),
            40,
            "and takes everything once the window closes"
        );
    }

    #[test]
    fn lethal_damage_starts_the_respawn_timer() {
        let mut state = duel();
        let hp = state.body(PlayerId(0)).unwrap().hp;
        assert_eq!(state.damage_player(PlayerId(0), hp + 50, Tick(7)), hp);
        let body = state.body(PlayerId(0)).unwrap();
        assert!(!body.is_up());
        assert_eq!(
            body.respawn_at,
            Some(Tick(7 + crate::player::respawn_ticks()))
        );
        // The seat is still Alive: being shot is not being eliminated.
        assert_eq!(state.players()[0].life, PlayerLife::Alive);
        assert_eq!(
            state.damage_player(PlayerId(0), 10, Tick(8)),
            0,
            "a corpse takes no further damage"
        );
    }

    #[test]
    fn place_spawns_moves_a_body_out_of_a_solid_column() {
        // The pad centre column is solid, so the nominal spawn point — chosen
        // without a world — is inside a block. `place_spawns` is what fixes it.
        let mut state = duel();
        let mut world = flat_world();
        let nominal = state.body(PlayerId(0)).unwrap().spawn;
        let cell = nominal.floor_to_ints();
        world.fill_box(
            [cell[0], cell[1], cell[2]],
            [cell[0], cell[1] + 2, cell[2]],
            true,
        );
        assert!(crate::movement::collides(
            &world,
            state.move_config(),
            nominal
        ));

        let moved = state.place_spawns(&world);
        assert!(moved >= 1, "the obstructed body should have been relocated");
        let placed = state.body(PlayerId(0)).unwrap().spawn;
        assert!(
            !crate::movement::collides(&world, state.move_config(), placed),
            "place_spawns left the body inside geometry at {placed:?}"
        );
    }

    #[test]
    fn a_harness_can_place_a_body_where_it_wants() {
        let mut state = duel();
        assert!(state.set_spawn(PlayerId(1), Vec3::from_ints(5, 41, 5)));
        let body = state.body(PlayerId(1)).unwrap();
        assert_eq!(body.spawn, Vec3::from_ints(5, 41, 5));
        assert_eq!(body.state.pos, Vec3::from_ints(5, 41, 5));
        assert!(!state.set_spawn(PlayerId(9), Vec3::ZERO));
    }
}
