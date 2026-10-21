//! Deterministic bot brains (plan §4.7 D-05, task T-023).
//!
//! A bot is a function from what it can *see* to what a player could *press*.
//! It owns no state that a player does not have: it reads the match, the
//! airdrops and the voxel field, and it returns an [`InputFrame`] plus, when it
//! is building, a `C_EDIT` request. The match loop applies both through exactly
//! the same paths a human's packet takes. That is the whole safety argument for
//! shipping bots in the same room as humans: there is no back door to write
//! state, so a bug in a bot is a bug in a *player's* inputs, not a corruption of
//! the simulation.
//!
//! # Why the brain is not clever
//!
//! It has to do four things, in this order of importance: get crates, turn them
//! into a recipe, stay alive, and hit the enemy core. Anything else is polish.
//! The policy is therefore a small priority ladder with no randomness, no
//! learning and no hidden state beyond a path and a build plan. Two bots with
//! the same view make the same decision, which is what makes a bot match a
//! reproducible fixture rather than a flaky one.
//!
//! # Building
//!
//! A bot deposits by aiming at a pad cell and placing a crate, the same
//! `C_EDIT` a human sends. It picks the most valuable recipe it can afford:
//! nine crates for a swordsman, six for a sword, four for a bandage. The plan is
//! anchored at the pad's `(0, 0)` corner, which is outside the core's protected
//! volume, so every shape in this ladder is legal by construction.

use sim::airdrop::Airdrops;
use sim::ids::{PlayerId, TeamId, MAX_PLAYERS};
use sim::input::{buttons, InputFrame};
use sim::player::PlayerBody;
use sim::state::MatchState;
use sim::time::Tick;
use sim::vec::Vec3;
use sim::VoxelQuery;
use world::block::CRATE;

/// How close a bot has to be to a pickup before it stops pathing directly.
const ARRIVE_RADIUS: sim::Fixed = sim::Fixed::from_int(2);

/// How far a bot will engage a player that is not its current goal.
const ENGAGE_RADIUS: sim::Fixed = sim::Fixed::from_int(2);

/// Ticks between path recomputations. Thirty is the buddy AI's cadence and is
/// slow enough that six bots do not pathfind on every tick.
const REPATH_TICKS: u32 = 40;

/// How far from the pad's corner a bot stands while building.
const BUILD_STANDOFF: i32 = 1;

/// A recipe the bot knows how to lay out, as pad-local `(x, z)` cells.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Plan {
    /// `R04_SWORDSMAN`: a 3 x 3 slab.
    Buddy,
    /// `R01_SWORD`: a 3 x 2 slab.
    Sword,
    /// `R03_BANDAGE`: a 2 x 2 slab.
    Bandage,
}

impl Plan {
    fn for_crates(carried: u32) -> Option<Plan> {
        if carried >= 9 {
            Some(Plan::Buddy)
        } else if carried >= 6 {
            Some(Plan::Sword)
        } else if carried >= 4 {
            Some(Plan::Bandage)
        } else {
            None
        }
    }

    fn cells(self) -> &'static [[i32; 2]] {
        match self {
            Plan::Buddy => &BUDDY_CELLS,
            Plan::Sword => &SWORD_CELLS,
            Plan::Bandage => &BANDAGE_CELLS,
        }
    }

    fn cost(self) -> u32 {
        self.cells().len() as u32
    }
}

const BUDDY_CELLS: [[i32; 2]; 9] = [
    [0, 0],
    [1, 0],
    [2, 0],
    [0, 1],
    [1, 1],
    [2, 1],
    [0, 2],
    [1, 2],
    [2, 2],
];
const SWORD_CELLS: [[i32; 2]; 6] = [[0, 0], [1, 0], [2, 0], [0, 1], [1, 1], [2, 1]];
const BANDAGE_CELLS: [[i32; 2]; 4] = [[0, 0], [1, 0], [0, 1], [1, 1]];

/// Everything a bot is allowed to know.
///
/// Deliberately immutable. A bot that could write to any of these fields would
/// be a second authority, and the acceptance criterion for T-023 is that it is
/// not: it only produces inputs.
pub struct BotView<'a> {
    /// The tick the decision is for.
    pub now: Tick,
    /// This bot's seat.
    pub me: PlayerId,
    /// This bot's body.
    pub body: &'a PlayerBody,
    /// The authoritative match.
    pub state: &'a MatchState,
    /// The crate economy.
    pub airdrops: &'a Airdrops,
    /// Terrain and placed crates, for collision and pathing.
    pub world: &'a crate::edit::EditWorld,
}

/// What a bot wants to do this tick.
#[derive(Clone, Copy, Debug)]
pub struct BotOutput {
    /// The frame the match loop applies through `MatchState::apply_input`.
    pub frame: InputFrame,
    /// A block placement, if the bot is building. `seq` is the bot's own
    /// monotonically increasing edit sequence, exactly like a client's.
    pub edit: Option<protocol::Edit>,
}

/// One bot's memory: a path, a build plan and a stuck timer. Nothing here is
/// unavailable to a human client; it is the same kind of cache a client keeps.
#[derive(Clone, Debug)]
pub struct Bot {
    id: PlayerId,
    seq: u16,
    path: Vec<[i32; 3]>,
    path_index: usize,
    repath_at: Tick,
    last_pos: Vec3,
    stuck: u32,
    plan: Option<Plan>,
}

impl Bot {
    /// A bot in seat `id`.
    pub fn new(id: PlayerId) -> Bot {
        Bot {
            id,
            seq: 1,
            path: Vec::new(),
            path_index: 0,
            repath_at: Tick::ZERO,
            last_pos: Vec3::ZERO,
            stuck: 0,
            plan: None,
        }
    }

    /// The seat this brain drives.
    pub fn id(&self) -> PlayerId {
        self.id
    }

    /// Decide one tick.
    pub fn decide(&mut self, view: &BotView<'_>) -> BotOutput {
        let idle = self.idle(view.now);
        if !view.body.is_up() {
            self.plan = None;
            return idle;
        }

        // A downed or eliminated bot does not keep a partial plan: the pad may
        // be about to be sealed, and resuming it after respawn would walk back
        // to a base that is no longer in the match.
        let carried = view.airdrops.carried(self.id);
        if let Some(plan) = self.plan {
            if pad_is_empty(view, self.id) {
                // Before the first block is placed the plan is still just an
                // intention, so upgrade it if the bot collected a better haul
                // on the way home. Once a cell is down, switching would leave a
                // half-built shape that matches nothing.
                if carried < plan.cost() {
                    self.plan = None;
                } else if let Some(better) = Plan::for_crates(carried) {
                    self.plan = Some(better);
                }
            }
        }
        if self.plan.is_none() {
            self.plan = Plan::for_crates(carried);
        }

        let pad = view.state.pads().get(team_of(view, self.id));
        let standoff = pad.map(|p| standoff_point(p.origin()));
        // The highest priority is finishing a build that is already underway,
        // but only one bot per team may build at a time. Two bots laying
        // different shapes into one pad produce a cluster that matches no
        // recipe, and the stability timer resets on every block: the team can
        // stand there forever holding crates. The builder is the teammate with
        // the most crates, ties to the lowest id.
        let may_build = self.is_primary_builder(view);
        if may_build {
            if let (Some(plan), Some(pad), Some(standoff)) = (self.plan, pad, standoff) {
                let complete = plan.cells().iter().all(|c| pad.get([c[0], 0, c[1]]));
                if complete {
                    // Wait for the 10-tick stability window. Standing still is the
                    // correct input: the matcher fires on its own.
                    self.stuck = 0;
                    return BotOutput {
                        frame: self.frame_toward(view, standoff),
                        edit: None,
                    };
                }
                if carried > 0 {
                    if view.body.state.pos.horizontal_distance(standoff) <= ARRIVE_RADIUS {
                        let edit = self.next_placement(view, plan, pad.origin());
                        let face = edit
                            .map(|e| Vec3::from_ints(e.x as i32, e.y as i32, e.z as i32))
                            .unwrap_or(standoff);
                        return BotOutput {
                            frame: self.frame_steering(view, standoff, face),
                            edit,
                        };
                    }
                    return BotOutput {
                        frame: self.frame_toward(view, standoff),
                        edit: None,
                    };
                }
            }
        }

        // Self-defence: a player inside fist range is more urgent than any
        // errand. The bot keeps walking while it swings.
        if let Some(enemy) = nearest_enemy(view, self.id, ENGAGE_RADIUS) {
            let move_goal = self.goal(view).unwrap_or(view.body.state.pos);
            let mut frame = self.frame_steering(view, move_goal, enemy);
            if view.now.0 % 10 == 0 {
                frame.buttons |= buttons::FIRE;
            }
            return BotOutput { frame, edit: None };
        }

        // Collect. Pickups first; a drop still in the air is next best, because
        // camping its site is how a bot wins the race to it.
        let goal = self.goal(view);
        let mut frame = match goal {
            Some(goal) => self.frame_toward(view, goal),
            None => self.idle(view.now).frame,
        };

        // Punch the core if it is in reach. The match loop owns the actual
        // damage; the bot only supplies the button and the facing.
        if let Some(core) = enemy_core_centre(view, self.id) {
            if view.body.state.pos.distance(core) <= sim::tuning::embedded().weapons.fists.reach
                && view.now.0 % 10 == 0
            {
                frame.yaw = yaw_toward(view.body.state.pos, core);
                frame.buttons |= buttons::FIRE;
            }
        }
        BotOutput { frame, edit: None }
    }

    /// True when this bot is its team's current builder.
    ///
    /// Only the bot with the largest load builds. That is deterministic, keeps
    /// one shape in the pad at a time, and gives the biggest haul to the best
    /// recipe instead of letting whoever arrived first spend it on a bandage.
    fn is_primary_builder(&self, view: &BotView<'_>) -> bool {
        let my_team = team_of(view, self.id);
        let mut best: Option<(u32, PlayerId)> = None;
        for slot in view.state.players() {
            if slot.team != my_team || !slot.life.is_alive() {
                continue;
            }
            let carried = view.airdrops.carried(slot.id);
            if carried < 4 {
                continue;
            }
            let better = match best {
                None => true,
                Some((amount, id)) => carried > amount || (carried == amount && slot.id < id),
            };
            if better {
                best = Some((carried, slot.id));
            }
        }
        best.is_some_and(|(_, id)| id == self.id)
    }

    /// Where this bot wants to walk: a pickup, a falling drop, the pad, or the
    /// enemy core.
    fn goal(&self, view: &BotView<'_>) -> Option<Vec3> {
        let carried = view.airdrops.carried(self.id);

        // Once a bot is armed enough to matter or its team has fielded a
        // buddy, the core outranks another crate. Without this the map's
        // continuous drop schedule is an infinite errand: there is always one
        // more pickup, so nobody ever attacks.
        let armed = carried >= 9 || team_has_buddy(view, self.id);

        if !armed {
            if let Some(pos) = nearest_pickup(view, self.id) {
                return Some(pos);
            }
        }
        if carried >= 4 && self.is_primary_builder(view) {
            if let Some(pad) = view.state.pads().get(team_of(view, self.id)) {
                return Some(standoff_point(pad.origin()));
            }
        }
        if armed {
            if let Some(core) = enemy_core_centre(view, self.id) {
                return Some(core);
            }
        }
        if let Some(pos) = nearest_pickup(view, self.id) {
            return Some(pos);
        }
        if let Some(site) = nearest_falling(view, self.id) {
            return Some(site);
        }
        enemy_core_centre(view, self.id)
    }

    /// The next crate cell in the plan that the pad does not already hold.
    ///
    /// The `seq` is the bot's own counter, so two placements in one tick cannot
    /// collide even if the match loop ever batches them.
    fn next_placement(
        &mut self,
        view: &BotView<'_>,
        plan: Plan,
        origin: [i32; 3],
    ) -> Option<protocol::Edit> {
        for cell in plan.cells() {
            let local = [cell[0], 0, cell[1]];
            let pad = view.state.pads().get(team_of(view, self.id))?;
            if pad.get(local) || pad.is_blocked(local) {
                continue;
            }
            let seq = self.seq;
            self.seq = self.seq.wrapping_add(1);
            return Some(protocol::Edit {
                seq,
                x: (origin[0] + local[0]) as i16,
                y: (origin[1] + local[1]) as i16,
                z: (origin[2] + local[2]) as i16,
                block: CRATE.0,
                action: protocol::edit_action::PLACE,
            });
        }
        None
    }

    /// Build a frame that moves toward `move_goal` while facing `face`.
    ///
    /// Facing and movement are separate: a bot that turned its body at an enemy
    /// while still pressing "forward" walked away from its goal, turned back,
    /// walked in, and repeated. The fix is to keep the world-space movement
    /// direction and turn it into yaw-relative axes, so attacking and travelling
    /// can happen at the same time.
    fn frame_steering(&mut self, view: &BotView<'_>, move_goal: Vec3, face: Vec3) -> InputFrame {
        let pos = view.body.state.pos;
        if pos.horizontal_distance(self.last_pos) < sim::Fixed::from_raw(3_000) {
            self.stuck += 1;
        } else {
            self.stuck = 0;
        }
        self.last_pos = pos;

        let distance = pos.horizontal_distance(move_goal);
        let waypoint = if distance <= sim::Fixed::from_int(3) {
            self.path.clear();
            self.path_index = 0;
            move_goal
        } else {
            self.follow_path(view, move_goal)
        };

        let yaw = yaw_toward(pos, face);
        let forward = sim::input::forward_from_yaw(yaw);
        let right = sim::input::right_from_yaw(yaw);
        let direction = Vec3::new(waypoint.x - pos.x, sim::ZERO, waypoint.z - pos.z);
        let direction = if direction.horizontal_length() > sim::Fixed::from_raw(1_000) {
            direction.normalize()
        } else {
            Vec3::ZERO
        };

        let threshold = sim::Fixed::from_raw(20_000);
        let ahead = direction.dot(forward);
        let side = direction.dot(right);
        let mut move_z = if ahead > threshold {
            1
        } else if ahead < -threshold {
            -1
        } else {
            0
        };
        let mut move_x = if side > threshold {
            1
        } else if side < -threshold {
            -1
        } else {
            0
        };

        // Backing out of a pocket is a movement decision, not a facing one.
        // Before backing all the way out, try sliding along the obstacle: a
        // blocked body still moves on the free axis, which is how a corridor
        // with one blocked cell is passed without a full retreat.
        if self.stuck > 6 && self.stuck <= 35 {
            move_z = 1;
            move_x = if (self.stuck / 8) % 2 == 0 { 1 } else { -1 };
        }
        if self.stuck > 35 && self.stuck <= 60 {
            move_z = -move_z.max(1);
            move_x = -move_x;
        }

        let mut buttons = 0;
        if self.stuck > 20 && view.body.state.on_ground && self.stuck <= 35 {
            buttons |= buttons::JUMP;
        }
        if self.stuck > 60 {
            self.path.clear();
            self.repath_at = view.now;
            self.stuck = 0;
        }

        InputFrame {
            tick: view.now,
            move_x,
            move_z,
            yaw,
            pitch: 0,
            buttons,
        }
    }

    /// Build a frame that walks toward `goal`, facing where it walks.
    fn frame_toward(&mut self, view: &BotView<'_>, goal: Vec3) -> InputFrame {
        self.frame_steering(view, goal, goal)
    }

    /// Follow the cached path, returning the world position to steer at.
    fn follow_path(&mut self, view: &BotView<'_>, goal: Vec3) -> Vec3 {
        let now = view.now;
        // A* caps its search box at 56 m on each axis, so a goal on the far
        // side of the map is not a failed path, it is an unasked question. Walk
        // in 40 m legs toward the goal instead; each leg is a fresh search, and
        // the last one is the real goal.
        let distance = view.body.state.pos.horizontal_distance(goal);
        let aim = if distance > sim::Fixed::from_int(20) {
            let direction = Vec3::new(
                goal.x - view.body.state.pos.x,
                sim::ZERO,
                goal.z - view.body.state.pos.z,
            )
            .normalize();
            let x = view.body.state.pos.x + direction.x * sim::Fixed::from_int(20);
            let z = view.body.state.pos.z + direction.z * sim::Fixed::from_int(20);
            // Aim at the ground under the leg, not at the player's current
            // altitude: the map drops away, and an endpoint in mid-air has no
            // floor, so the pathfinder refuses it. Shift off a tree trunk if
            // one happens to stand on the endpoint.
            ground_goal(view, x.round_int(), z.round_int())
        } else {
            goal
        };

        if now.0 >= self.repath_at.0 || self.path.is_empty() {
            let start = view.body.state.pos.floor_to_ints();
            let end = aim.floor_to_ints();
            self.path =
                sim::pathfind::find_path(view.world, start, end, bot_limits()).unwrap_or_default();
            self.path_index = 0;
            self.repath_at = now.advance(REPATH_TICKS);
        }

        while self.path_index < self.path.len() {
            let cell = self.path[self.path_index];
            let centre = Vec3::new(
                sim::Fixed::from_int(cell[0]) + sim::Fixed::from_raw(32_768),
                sim::Fixed::from_int(cell[1]),
                sim::Fixed::from_int(cell[2]) + sim::Fixed::from_raw(32_768),
            );
            if view.body.state.pos.horizontal_distance(centre) < sim::Fixed::from_int(1) {
                self.path_index += 1;
            } else {
                return centre;
            }
        }
        goal
    }

    fn idle(&self, now: Tick) -> BotOutput {
        BotOutput {
            frame: InputFrame {
                tick: now,
                move_x: 0,
                move_z: 0,
                yaw: 0,
                pitch: 0,
                buttons: 0,
            },
            edit: None,
        }
    }
}

/// Path settings for bots: no big drops into pits, and enough vertical room to
/// route over a hill rather than trying to walk through it.
fn bot_limits() -> sim::pathfind::PathLimits {
    sim::pathfind::PathLimits {
        budget: sim::pathfind::EXPANSION_BUDGET,
        max_fall: 1,
        y_margin: 20,
    }
}

/// A standoff point one metre outside the pad's south-west corner.
///
/// The core occupies the middle of the pad and its protected volume covers
/// `(3..5, 0..3, 3..5)`; standing outside the corner keeps every plan cell in
/// the ladder within the 6 m reach without ever standing inside a crate.
/// The nearest standable ground cell to `(x, z)`, or the nominal one if the
/// search finds nothing. The pathfinder requires a floor and rejects a goal
/// inside a trunk, which would otherwise turn every leg through a forest into
/// "no path" and leave the bot steering into the tree.
fn ground_goal(view: &BotView<'_>, x: i32, z: i32) -> Vec3 {
    let y = view.world.surface_height(x, z) + 1;
    if !view.world.solid(x, y, z) && !view.world.solid(x, y + 1, z) {
        return cell_centre([x, y, z]);
    }
    for radius in 1i32..=4 {
        for dz in -radius..=radius {
            for dx in -radius..=radius {
                if dx.abs().max(dz.abs()) != radius {
                    continue;
                }
                let gx = x + dx;
                let gz = z + dz;
                let gy = view.world.surface_height(gx, gz) + 1;
                if !view.world.solid(gx, gy, gz) && !view.world.solid(gx, gy + 1, gz) {
                    return cell_centre([gx, gy, gz]);
                }
            }
        }
    }
    Vec3::from_ints(x, y, z)
}

fn standoff_point(origin: [i32; 3]) -> Vec3 {
    cell_centre([
        origin[0] + BUILD_STANDOFF,
        origin[1],
        origin[2] - BUILD_STANDOFF,
    ])
}

/// The centre of a cell. Walking to a corner puts the 0.6 m body against the
/// neighbouring cell's wall, which the pathfinder does not model; every goal a
/// bot steers at must be a centre.
fn cell_centre(cell: [i32; 3]) -> Vec3 {
    Vec3::new(
        sim::Fixed::from_int(cell[0]) + sim::Fixed::from_raw(32_768),
        sim::Fixed::from_int(cell[1]),
        sim::Fixed::from_int(cell[2]) + sim::Fixed::from_raw(32_768),
    )
}

fn team_has_buddy(view: &BotView<'_>, player: PlayerId) -> bool {
    view.state
        .team(team_of(view, player))
        .is_some_and(|team| !team.buddies.is_empty())
}

fn pad_is_empty(view: &BotView<'_>, player: PlayerId) -> bool {
    view.state
        .pads()
        .get(team_of(view, player))
        .is_some_and(|pad| pad.is_empty())
}

fn team_of(view: &BotView<'_>, player: PlayerId) -> TeamId {
    view.state
        .players()
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.team)
        .unwrap_or(TeamId(0))
}

fn nearest_pickup(view: &BotView<'_>, me: PlayerId) -> Option<Vec3> {
    let body = view.body;
    let mut best: Option<(sim::Fixed, u32, Vec3)> = None;
    for pickup in view.airdrops.pickups() {
        let Some(stand) = pickup_stand(view, pickup.pos) else {
            continue;
        };
        let distance = body.state.pos.distance(stand);
        let better = match best {
            None => true,
            Some((best_distance, best_id, _)) => {
                distance < best_distance || (distance == best_distance && pickup.id < best_id)
            }
        };
        if better {
            best = Some((distance, pickup.id, stand));
        }
    }
    let _ = me;
    best.map(|(_, _, pos)| pos)
}

/// A cell the bot can stand in and still be within the 1.2 m pickup radius.
///
/// Scatter is ±2 m and the drop site is flattened only on average, so a crate
/// can end up inside a slope or a trunk. Standing on the exact crate cell would
/// then be impossible; standing on the adjacent cell is enough for the pickup
/// test, which is the only thing that matters.
fn pickup_stand(view: &BotView<'_>, pos: Vec3) -> Option<Vec3> {
    let cx = pos.x.round_int();
    let cz = pos.z.round_int();
    let radius = sim::tuning::embedded().airdrop.pickup_radius;
    for ring in 0..=1i32 {
        for dz in -ring..=ring {
            for dx in -ring..=ring {
                if dx.abs().max(dz.abs()) != ring {
                    continue;
                }
                let x = cx + dx;
                let z = cz + dz;
                let y = view.world.surface_height(x, z) + 1;
                let stand = cell_centre([x, y, z]);
                if view.world.solid(x, y, z) || view.world.solid(x, y + 1, z) {
                    continue;
                }
                if stand.horizontal_distance(pos) <= radius
                    && (stand.y - pos.y).abs() <= sim::Fixed::from_int(2)
                {
                    return Some(stand);
                }
            }
        }
    }
    let y = pos.y.round_int();
    let stand = cell_centre([cx, y, cz]);
    if !view.world.solid(cx, y, cz) && !view.world.solid(cx, y + 1, cz) {
        Some(stand)
    } else {
        None
    }
}

fn nearest_falling(view: &BotView<'_>, me: PlayerId) -> Option<Vec3> {
    let mut best: Option<(sim::Fixed, Vec3)> = None;
    for drop in view.airdrops.falling() {
        let pos = Vec3::from_ints(drop.site[0], drop.site[1], drop.site[2]);
        let distance = view.body.state.pos.horizontal_distance(pos);
        if best.is_none_or(|(d, _)| distance < d) {
            best = Some((distance, pos));
        }
    }
    let _ = me;
    best.map(|(_, pos)| pos)
}

fn nearest_enemy(view: &BotView<'_>, me: PlayerId, radius: sim::Fixed) -> Option<Vec3> {
    let my_team = team_of(view, me);
    let mut best: Option<(sim::Fixed, PlayerId, Vec3)> = None;
    for (slot, body) in view.state.players().iter().zip(view.state.bodies()) {
        if slot.team == my_team || !slot.life.is_alive() || !body.is_up() {
            continue;
        }
        let distance = view.body.state.pos.distance(body.state.pos);
        if distance > radius {
            continue;
        }
        let better = match best {
            None => true,
            Some((best_distance, best_id, _)) => {
                distance < best_distance || (distance == best_distance && slot.id < best_id)
            }
        };
        if better {
            best = Some((distance, slot.id, body.state.pos));
        }
    }
    best.map(|(_, _, pos)| pos)
}

/// The centre of the nearest enemy core, as a point to walk at and punch.
fn enemy_core_centre(view: &BotView<'_>, me: PlayerId) -> Option<Vec3> {
    let my_team = team_of(view, me);
    let mut best: Option<(sim::Fixed, Vec3)> = None;
    for team in view.state.teams() {
        if team.id == my_team || team.base.is_eliminated() || !team.base.core.is_alive() {
            continue;
        }
        let origin = team.base.core.origin();
        let pos = Vec3::from_ints(
            origin[0] + sim::base::CORE_FOOTPRINT / 2,
            origin[1],
            origin[2] + sim::base::CORE_FOOTPRINT / 2,
        );
        let distance = view.body.state.pos.distance(pos);
        if best.is_none_or(|(d, _)| distance < d) {
            best = Some((distance, pos));
        }
    }
    best.map(|(_, pos)| pos)
}

/// Yaw that faces from `from` toward `to` (0 = +Z, 64 = +X).
pub fn yaw_toward(from: Vec3, to: Vec3) -> u8 {
    let rad = sim::fixed::atan2(to.x - from.x, to.z - from.z);
    let turns = (i64::from(rad.0) * 256) / i64::from(sim::fixed::TAU.0);
    turns.rem_euclid(256) as u8
}

/// Seats a bot may drive: every seat in the room.
pub const MAX_BOT_SEATS: u8 = MAX_PLAYERS as u8;

#[cfg(test)]
mod tests {
    use super::*;
    use sim::match_rules::Mode;
    use sim::state::RoomSetup;

    #[test]
    fn a_bot_moves_through_input_frames() {
        let setup = RoomSetup {
            mode: Mode::Ffa,
            teams: None,
            humans: 1,
            bots: 1,
        };
        let mut m = crate::play::Match::new(0x00C0_FFEE, setup).expect("match");
        assert!(m.add_bot(PlayerId(0)));
        let start = m.state().body(PlayerId(0)).unwrap().state.pos;
        for _ in 0..300 {
            m.tick(&[]);
        }
        let end = m.state().body(PlayerId(0)).unwrap().state.pos;
        assert_ne!(start, end, "a bot must walk on its own input frames");
    }

    #[test]
    fn a_bot_collects_crates_and_builds_a_recipe() {
        let setup = RoomSetup {
            mode: Mode::Ffa,
            teams: None,
            humans: 1,
            bots: 1,
        };
        let mut m = crate::play::Match::new(0x00C0_FFEE, setup).expect("match");
        assert!(m.add_bot(PlayerId(0)));

        let mut crafted = false;
        for _ in 0..4_000 {
            m.tick(&[]);
            if m.events()
                .iter()
                .any(|event| matches!(event, crate::play::MatchEvent::Crafted { .. }))
            {
                crafted = true;
                break;
            }
            assert!(!m.is_over());
        }
        assert!(
            crafted,
            "the bot never built a recipe: carried {} pad {}",
            m.airdrops().carried(PlayerId(0)),
            m.state().pads().get(TeamId(0)).map_or(0, |pad| pad.count())
        );
    }

    #[test]
    fn bot_uses_only_input_frames() {
        // Two matches from one seed. In the first the seat is a bot; in the
        // second the brain is run by the test and its output is submitted as a
        // human frame. If the bot had any other channel into the state, the
        // hashes would diverge. 800 ticks is before the first drop lands, so
        // the window is edit-free and movement alone has to prove the point.
        let setup = RoomSetup {
            mode: Mode::Ffa,
            teams: None,
            humans: 2,
            bots: 0,
        };
        let me = PlayerId(0);
        let mut with_bot = crate::play::Match::new(0x00C0_FFEE, setup).expect("match");
        assert!(with_bot.add_bot(me));
        let mut replay = crate::play::Match::new(0x00C0_FFEE, setup).expect("match");
        let mut brain = Bot::new(me);

        for _ in 0..800 {
            // `Match::tick` advances the clock before it asks the bot, so the
            // standalone decision has to be for the same tick.
            let now = replay.now().next();
            let view = BotView {
                now,
                me,
                body: replay.state().body(me).unwrap(),
                state: replay.state(),
                airdrops: replay.airdrops(),
                world: replay.world(),
            };
            let output = brain.decide(&view);
            assert!(
                output.edit.is_none(),
                "the first drop cannot have landed this early"
            );

            with_bot.tick(&[]);
            replay.tick(&[(me, output.frame)]);

            assert_eq!(
                with_bot.state().state_hash(),
                replay.state().state_hash(),
                "bot and human paths diverged at tick {}",
                replay.now().0
            );
            assert_eq!(with_bot.airdrops().audit(), replay.airdrops().audit());
        }
    }

    #[test]
    fn deciding_does_not_touch_authoritative_state() {
        let setup = RoomSetup {
            mode: Mode::Ffa,
            teams: None,
            humans: 1,
            bots: 1,
        };
        let mut m = crate::play::Match::new(0x00C0_FFEE, setup).expect("match");
        m.add_bot(PlayerId(0));
        m.tick(&[]);
        let mut bot = Bot::new(PlayerId(0));
        let before = m.state().state_hash();
        let view = BotView {
            now: m.now(),
            me: PlayerId(0),
            body: m.state().body(PlayerId(0)).unwrap(),
            state: m.state(),
            airdrops: m.airdrops(),
            world: m.world(),
        };
        let _ = bot.decide(&view);
        assert_eq!(before, m.state().state_hash(), "deciding must be read-only");
    }

    #[test]
    fn bots_can_win_against_bots() {
        // Six bot-driven bodies in a 3v3. The seats are marked human so D-05
        // does not end the match when one of them is eliminated: this test is
        // about whether bots can play and win, not about the last-human rule.
        // `d05_ends_a_bot_match_when_the_last_human_dies` covers that rule.
        let setup = RoomSetup {
            mode: Mode::Teams,
            teams: Some(2),
            humans: 6,
            bots: 0,
        };
        let mut m = crate::play::Match::new(0x00C0_FFEE, setup).expect("match");
        for seat in 0..6u8 {
            assert!(m.add_bot(PlayerId(seat)));
        }
        let mut ticks = 0;
        let mut crafted = 0;
        let mut core_hits = 0;
        while !m.is_over() && ticks < 30_000 {
            m.tick(&[]);
            for event in m.events() {
                match event {
                    crate::play::MatchEvent::Crafted { .. } => crafted += 1,
                    crate::play::MatchEvent::CoreHit { .. } => core_hits += 1,
                    _ => {}
                }
            }
            ticks += 1;
        }
        assert!(m.is_over(), "bots did not finish the match in 25 minutes");
        assert!(
            m.outcome().winner().is_some(),
            "a bot match ended without a winner: {:?}",
            m.outcome()
        );
        assert!(core_hits > 0, "no bot ever hit a core");
        assert!(crafted > 0, "no bot ever completed a recipe");
    }

    #[test]
    fn d05_ends_a_bot_match_when_the_last_human_dies() {
        let setup = RoomSetup {
            mode: Mode::Ffa,
            teams: None,
            humans: 1,
            bots: 5,
        };
        let mut m = crate::play::Match::new(0x00C0_FFEE, setup).expect("match");
        for seat in 1..6u8 {
            assert!(m.add_bot(PlayerId(seat)));
        }
        // Destroy the human's core. D-05 must fire on the same settle, before
        // "last team standing" can hand the win to a bot.
        m.damage_core(TeamId(0), 1_000_000);
        m.tick(&[]);
        assert!(m.is_over());
        assert_eq!(m.outcome().winner(), None);
        assert_eq!(
            m.outcome().reason(),
            Some(sim::match_rules::EndReason::NoHumans)
        );
    }
}
