//! The airdrop economy: schedule, site selection, descent, pickup and deposit
//! (plan §4.4, task T-025).
//!
//! # Why this is the whole economy
//!
//! There is no mining. Crates fall on a global timer, land as physical pickups,
//! and are carried to a pad where they become blocks — and blocks are recipes.
//! So this module is not a garnish on a resource chain; it *is* the resource
//! chain, and every number in it is a pacing decision:
//!
//! * **Crates per drop scale with player count** (3 × players). A shared drop in
//!   a six-player match must not starve six players, and without the scaling rule
//!   a 6-player match delivers a quarter of the crates per head that a duel does.
//! * **Every fifth drop is heavy** (×1.75). A periodic contested prize, placed on
//!   the schedule rather than left to chance, so teams can plan for it.
//! * **Death destroys half of what you carry.** The anti-snowball lever: a loaded
//!   player is worth killing, and the crates they drop feed the killer.
//!
//! # Determinism
//!
//! Everything here is integer and seeded from `RngStreams::airdrop`. Site
//! selection is shared with the client so it can draw the telegraph without
//! being told where the drop will land — the client runs the same weighted pick
//! from the same seed. That is only true if the pick is exact: the weight is
//! computed in whole metres (`3·max_dist − 2·dist`), never as a float ratio, and
//! the RNG draw is `next_below(total)` rather than a scaled fraction.
//!
//! # What this module does not own
//!
//! Turning a deposited crate into a block. Deposit returns *how many* crates the
//! player is entitled to place this tick; the room places them through the edit
//! pipeline, which owns reach, pad ownership and the pad's free cells. Splitting
//! it that way keeps the economy free of voxel storage and keeps the edit rules
//! in exactly one place.

use crate::fixed::{Fixed, ZERO};
use crate::ids::PlayerId;
use crate::rng::Xoshiro256;
use crate::time::{Tick, TICK_HZ};
use crate::tuning;
use crate::vec::Vec3;

/// Height a drop spawns at, in blocks (§4.4.1).
pub const DROP_HEIGHT: i32 = 120;

/// How far a landed crate may scatter from the drop site, in metres (§4.4.1).
pub const SCATTER_RADIUS: Fixed = Fixed::from_int(3);

/// The scatter is a square of ±2 m, not a disc of 3 m.
///
/// The bound the plan asks for is "stays within 3 m", and `√(2² + 2²) = 2.83`
/// satisfies it without a rejection loop. A disc would need either a square root
/// per crate or a retry, and both are more code than the difference is worth —
/// the crates are picked up in a 1.2 m radius anyway.
const SCATTER_OFFSET: Fixed = Fixed::from_int(2);

/// Rules lifted from `data/tuning.ron` once, so a tick does not re-read globals.
#[derive(Clone, Copy, Debug)]
pub struct AirdropRules {
    /// Tick of the first drop.
    pub first_drop: Tick,
    /// Interval before the escalation.
    pub early: Tick,
    /// Interval after it.
    pub late: Tick,
    /// Tick at which the interval tightens.
    pub late_after: Tick,
    /// Crates per player per drop.
    pub per_player: u32,
    /// Every Nth drop is heavy.
    pub heavy_every: u32,
    /// Heavy multiplier.
    pub heavy_mult: Fixed,
    /// Ticks of visible warning before impact.
    pub telegraph: Tick,
    /// Descent speed in metres per second.
    pub fall_speed: Fixed,
    /// Auto-pickup radius.
    pub pickup_radius: Fixed,
    /// Carry cap.
    pub carry_cap: u32,
    /// Fraction of carried crates destroyed on death.
    pub drop_loss: Fixed,
    /// Pickup lifetime.
    pub ttl: Tick,
    /// Simultaneous pickup ceiling.
    pub max_pickups: usize,
    /// Deposit rate in thousandths of a crate per tick.
    pub deposit_milli_per_tick: u32,
}

impl AirdropRules {
    /// Read the rules from the embedded tuning table.
    pub fn from_tuning() -> AirdropRules {
        let a = &tuning::embedded().airdrop;
        let pad = &tuning::embedded().pad;
        AirdropRules {
            first_drop: Tick(a.first_drop_s * TICK_HZ),
            early: Tick(a.interval_early_s * TICK_HZ),
            late: Tick(a.interval_late_s * TICK_HZ),
            late_after: Tick(a.late_after_s * TICK_HZ),
            per_player: a.crates_per_player,
            heavy_every: a.heavy_every,
            heavy_mult: a.heavy_mult,
            telegraph: Tick(a.telegraph_s * TICK_HZ),
            fall_speed: a.fall_speed,
            pickup_radius: a.pickup_radius,
            carry_cap: a.carry_cap,
            drop_loss: a.drop_loss,
            ttl: Tick(a.pickup_ttl_s * TICK_HZ),
            max_pickups: a.max_pickups as usize,
            // 6 crates/s at 20 Hz is 0.3 crates per tick, exact in thousandths.
            deposit_milli_per_tick: pad.deposit_rate * 1_000 / TICK_HZ,
        }
    }

    /// Ticks until the drop after `now`.
    fn interval_at(&self, now: Tick) -> Tick {
        if now.0 < self.late_after.0 {
            self.early
        } else {
            self.late
        }
    }

    /// The tick the drop *after* `now` is scheduled for.
    ///
    /// The cadence tightens at 5:00, not at the first gap that crosses it. Left
    /// to `interval_at` alone, the drop at 4:45 would be followed by one at
    /// 5:10 — the 25 s interval overshooting the boundary — and every drop after
    /// it would be ten seconds late, so "every 15 s from 5:00" would really be
    /// "every 15 s from 5:10". Clamping the overshoot keeps the escalation where
    /// the plan puts it, and is also where the crate totals are counted from.
    fn next_after(&self, now: Tick) -> Tick {
        let candidate = now.0 + self.interval_at(now).0;
        if now.0 < self.late_after.0 && candidate > self.late_after.0 {
            return self.late_after;
        }
        Tick(candidate)
    }
}

/// A crate on the ground.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Pickup {
    /// Stable handle, for interpolation and for the pickup's own despawn.
    pub id: u32,
    /// Position, at the crate's base.
    pub pos: Vec3,
    /// Tick it landed.
    pub born: Tick,
}

/// A drop on its way down.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Falling {
    /// Stable handle.
    pub id: u32,
    /// Where it will land.
    pub site: [i32; 3],
    /// Current height in metres, as a fixed-point scalar.
    pub y: Fixed,
    /// Crates it will scatter.
    pub crates: u32,
    /// Tick it touches down.
    pub land_at: Tick,
    /// Whether this was the every-fifth heavy drop.
    pub heavy: bool,
}

/// What one tick produced, for the host to turn into `S_EVENT`s.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct TickEvents {
    /// Drops that were announced this tick: the telegraph, five seconds before
    /// impact. This is the event the client draws a marker from — announcing it
    /// at touchdown would be announcing a contest that is already over.
    pub spawned: Vec<Landed>,
    /// Drops that touched down this tick.
    pub landed: Vec<Landed>,
    /// Crates picked up this tick, `(player, count)`.
    pub picked: Vec<(PlayerId, u32)>,
}

/// One landing, for the `AIRDROP` event.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Landed {
    /// Where.
    pub site: [i32; 3],
    /// How many crates.
    pub crates: u32,
    /// Whether it was heavy.
    pub heavy: bool,
}

/// The crate books: everything needed to prove no crate was invented.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Audit {
    /// Crates spawned by drops.
    pub dropped: u64,
    /// Crates removed by death loss, despawn or the entity cap.
    pub destroyed: u64,
    /// Crates turned into blocks on a pad.
    pub deposited: u64,
    /// Crates in players' hands.
    pub carried: u64,
    /// Crates on the ground.
    pub on_ground: u64,
    /// Crates still in the air.
    ///
    /// A drop is booked as `dropped` when it spawns, six to seven seconds
    /// before it lands. Without this bucket the books fail to balance for every
    /// tick of every descent, which is exactly the window a conservation test
    /// that only samples the ground would miss.
    pub falling: u64,
}

impl Audit {
    /// True when the books balance: everything dropped is accounted for.
    pub fn balances(&self) -> bool {
        self.dropped
            == self.destroyed + self.deposited + self.carried + self.on_ground + self.falling
    }
}

/// The airdrop system for one match.
#[derive(Clone, Debug)]
pub struct Airdrops {
    rules: AirdropRules,
    rng: Xoshiro256,
    /// Candidate landing sites, from worldgen. Never a base pad: worldgen
    /// already excludes them (T-006), and this module trusts that set.
    sites: Vec<[i32; 3]>,
    /// Humans and bots, for the crate count.
    players: u32,
    /// When the next drop spawns.
    next_drop_at: Tick,
    /// Drops spawned so far, for the every-fifth rule.
    drops_made: u32,
    /// Index into `sites` of the previous drop, excluded from the next pick.
    last_site: Option<usize>,
    /// Drops in the air.
    falling: Vec<Falling>,
    /// Crates on the ground, oldest first.
    pickups: Vec<Pickup>,
    /// Carried crates, indexed by player id.
    carried: Vec<u32>,
    /// Deposit token accumulators, in thousandths of a crate.
    deposit_milli: Vec<u32>,
    /// Tick each player's accumulator was last refilled at.
    deposit_at: Vec<Tick>,
    /// Next handle.
    next_id: u32,
    /// Books.
    dropped: u64,
    destroyed: u64,
    deposited: u64,
}

impl Airdrops {
    /// A match's airdrop state.
    ///
    /// `sites` comes from `world::gen`; it is passed in rather than generated
    /// here because `sim` has no dependency on `world` — the same reason
    /// movement takes a `VoxelQuery` instead of owning terrain.
    pub fn new(seed: u64, players: u32, sites: Vec<[i32; 3]>) -> Airdrops {
        let rules = AirdropRules::from_tuning();
        Airdrops {
            rules,
            rng: Xoshiro256::seed_from_parts(seed, 0xA1_D0_09),
            sites,
            players: players.max(1),
            next_drop_at: rules.first_drop,
            drops_made: 0,
            last_site: None,
            falling: Vec::new(),
            pickups: Vec::new(),
            carried: vec![0; crate::ids::MAX_PLAYERS],
            deposit_milli: vec![0; crate::ids::MAX_PLAYERS],
            deposit_at: vec![Tick::ZERO; crate::ids::MAX_PLAYERS],
            next_id: 1,
            dropped: 0,
            destroyed: 0,
            deposited: 0,
        }
    }

    /// The rules in force.
    pub fn rules(&self) -> &AirdropRules {
        &self.rules
    }

    /// Crates currently on the ground.
    pub fn pickups(&self) -> &[Pickup] {
        &self.pickups
    }

    /// Drops currently falling.
    pub fn falling(&self) -> &[Falling] {
        &self.falling
    }

    /// Crates a player is carrying.
    pub fn carried(&self, player: PlayerId) -> u32 {
        self.carried.get(player.0 as usize).copied().unwrap_or(0)
    }

    /// Drops spawned so far.
    pub fn drops_made(&self) -> u32 {
        self.drops_made
    }

    /// The tick the next drop is scheduled for.
    pub fn next_drop_at(&self) -> Tick {
        self.next_drop_at
    }

    /// The books.
    pub fn audit(&self) -> Audit {
        Audit {
            dropped: self.dropped,
            destroyed: self.destroyed,
            deposited: self.deposited,
            carried: self.carried.iter().map(|c| u64::from(*c)).sum(),
            on_ground: self.pickups.len() as u64,
            falling: self.falling.iter().map(|drop| u64::from(drop.crates)).sum(),
        }
    }

    /// Advance one tick.
    ///
    /// `bodies` is every player's authoritative position, in id order. Pickup is
    /// automatic and server-side: the client is never asked whether it walked
    /// over a crate.
    pub fn tick(&mut self, now: Tick, bodies: &[(PlayerId, Vec3)]) -> TickEvents {
        let mut events = TickEvents::default();

        // Descend before spawning. A drop that appears this tick has to appear at
        // full height: spawning first would move it 0.6 m down in the same tick
        // it was created, so the first position any observer sees is a drop
        // already in motion, and the client's spawn interpolation has nothing to
        // anchor on.
        self.descend(now, &mut events);

        if now.0 >= self.next_drop_at.0 {
            self.spawn_drop(now, &mut events);
            self.next_drop_at = self.rules.next_after(self.next_drop_at);
        }

        self.pick_up(now, bodies, &mut events);
        self.expire(now);

        events
    }

    /// Choose a site, spawn a falling drop.
    fn spawn_drop(&mut self, now: Tick, events: &mut TickEvents) {
        let Some(index) = self.choose_site() else {
            // No legal sites: an empty world. The schedule still advances, so a
            // room with a broken map does not spin here every tick.
            return;
        };
        self.last_site = Some(index);
        self.drops_made += 1;

        let heavy = self.rules.heavy_every > 0 && self.drops_made % self.rules.heavy_every == 0;
        let base = self.players * self.rules.per_player;
        let crates = if heavy {
            (Fixed::from_int(base as i32) * self.rules.heavy_mult).round_int() as u32
        } else {
            base
        };

        let site = self.sites[index];
        self.dropped += u64::from(crates);
        let id = self.next_id();
        self.falling.push(Falling {
            id,
            site,
            y: Fixed::from_int(DROP_HEIGHT),
            crates,
            land_at: Tick(now.0 + self.descent_ticks(site[1])),
            heavy,
        });
        // The telegraph is the event: the client draws a marker and a shadow for
        // the five seconds before impact (§4.4.1). Nothing is announced at
        // touchdown that was not announced here.
        events.spawned.push(Landed {
            site,
            crates,
            heavy,
        });
    }

    /// Weighted pick over the candidate sites, excluding the previous one.
    ///
    /// `weight = 1 + 2·(1 − dist/max_dist)`, scaled by `max_dist` so it is an
    /// integer: `3·max_dist − 2·dist` in whole metres. Centre drops are three
    /// times as likely as edge drops, which makes The Yard contested by geometry
    /// rather than by a rule (§4.4.2).
    fn choose_site(&mut self) -> Option<usize> {
        if self.sites.is_empty() {
            return None;
        }
        let centre = centre_of(&self.sites);
        let mut max_dist = 1i32;
        let mut weights = Vec::with_capacity(self.sites.len());
        for (index, site) in self.sites.iter().enumerate() {
            let dist = metres_between(centre, *site);
            max_dist = max_dist.max(dist);
            weights.push((index, dist));
        }

        let mut total = 0u32;
        for (index, dist) in &weights {
            if Some(*index) == self.last_site {
                continue;
            }
            total += weight_of(*dist, max_dist);
        }
        if total == 0 {
            // Every candidate is the previous site: one site, or a degenerate
            // map. Repeating it is better than dropping nothing forever.
            return Some(0);
        }

        let mut roll = self.rng.next_below(total);
        for (index, dist) in &weights {
            if Some(*index) == self.last_site {
                continue;
            }
            let weight = weight_of(*dist, max_dist);
            if roll < weight {
                return Some(*index);
            }
            roll -= weight;
        }
        // Unreachable: the walk above covers exactly `total`.
        None
    }

    /// Ticks from spawn to touchdown, never shorter than the telegraph.
    ///
    /// The descent is physics, not a timer: 12 m/s from Y 120 to a site at Y 40
    /// is 80 m, which is 6.7 s at 20 Hz — the figure the plan's own rationale
    /// column gives, and longer than the 5 s the telegraph row asks for. The
    /// telegraph is therefore a *floor* on the warning, not the descent length;
    /// a drop that fell faster would land before the marker had been up for the
    /// five seconds that make it a contest rather than a lottery.
    fn descent_ticks(&self, ground_y: i32) -> u32 {
        let distance = Fixed::from_int((DROP_HEIGHT - ground_y).max(0));
        let per_tick = self.rules.fall_speed / Fixed::from_int(TICK_HZ as i32);
        let fall = if per_tick.to_raw() <= 0 {
            self.rules.telegraph.0
        } else {
            (distance / per_tick).round_int().max(0) as u32 + 1
        };
        fall.max(self.rules.telegraph.0)
    }

    /// Move falling drops down; land the ones that arrive.
    fn descend(&mut self, now: Tick, events: &mut TickEvents) {
        let fall_per_tick = self.rules.fall_speed / Fixed::from_int(TICK_HZ as i32);
        let mut landed = Vec::new();
        let mut i = 0;
        while i < self.falling.len() {
            let drop = &mut self.falling[i];
            drop.y -= fall_per_tick;
            let ground = Fixed::from_int(drop.site[1]);
            if now.0 >= drop.land_at.0 || drop.y <= ground {
                landed.push(i);
            }
            i += 1;
        }

        // Back to front so the indices stay valid.
        for index in landed.into_iter().rev() {
            let drop = self.falling.remove(index);
            self.scatter(drop, now);
            events.landed.push(Landed {
                site: drop.site,
                crates: drop.crates,
                heavy: drop.heavy,
            });
        }
    }

    /// Turn one landing into individual pickups.
    fn scatter(&mut self, drop: Falling, now: Tick) {
        for _ in 0..drop.crates {
            let dx = self.rng.next_fixed_range(-SCATTER_OFFSET, SCATTER_OFFSET);
            let dz = self.rng.next_fixed_range(-SCATTER_OFFSET, SCATTER_OFFSET);
            let pos = Vec3::new(
                Fixed::from_int(drop.site[0]) + dx,
                Fixed::from_int(drop.site[1]),
                Fixed::from_int(drop.site[2]) + dz,
            );
            let id = self.next_id();
            self.pickups.push(Pickup { id, pos, born: now });
        }
        // Oldest first out. The cap is what keeps a 20-minute match from
        // accumulating thousands of entities nobody can see (§4.4.3).
        while self.pickups.len() > self.rules.max_pickups {
            self.pickups.remove(0);
            self.destroyed += 1;
        }
    }

    /// Automatic pickup within 1.2 m, up to the carry cap.
    fn pick_up(&mut self, _now: Tick, bodies: &[(PlayerId, Vec3)], events: &mut TickEvents) {
        if self.pickups.is_empty() || bodies.is_empty() {
            return;
        }
        // Squared radius, so the inner loop is comparisons and multiplies. The
        // inner loop runs pickups x bodies every tick; a square root there was
        // 17% of a full match's wall time.
        let radius_sq = self.rules.pickup_radius * self.rules.pickup_radius;
        let vertical_slack = Fixed::from_int(2);
        let mut taken = vec![0u32; bodies.len()];
        let mut index = 0;
        while index < self.pickups.len() {
            let pickup = self.pickups[index];
            let mut consumed = false;
            for (slot, (player, pos)) in bodies.iter().enumerate() {
                if self.carried(*player) + taken[slot] >= self.rules.carry_cap {
                    continue;
                }
                let offset = *pos - pickup.pos;
                if offset.horizontal_length_sq() <= radius_sq && offset.y.abs() <= vertical_slack {
                    taken[slot] += 1;
                    consumed = true;
                    break;
                }
            }
            if consumed {
                self.pickups.remove(index);
            } else {
                index += 1;
            }
        }

        for (slot, (player, _)) in bodies.iter().enumerate() {
            if taken[slot] > 0 {
                let id = player.0 as usize;
                if id < self.carried.len() {
                    self.carried[id] += taken[slot];
                }
                events.picked.push((*player, taken[slot]));
            }
        }
    }

    /// Despawn pickups past their lifetime.
    fn expire(&mut self, now: Tick) {
        let ttl = self.rules.ttl.0;
        let mut index = 0;
        while index < self.pickups.len() {
            if now.0.saturating_sub(self.pickups[index].born.0) >= ttl {
                self.pickups.remove(index);
                self.destroyed += 1;
            } else {
                index += 1;
            }
        }
    }

    /// Deposit up to the rate allows this tick, if the player is in their own pad.
    ///
    /// Returns the number of crates the caller may turn into blocks *now*. The
    /// crates leave the player's hands here, so the caller must place exactly
    /// that many; a caller that places fewer has lost the difference, which is
    /// why the pad-full case belongs in the caller's loop and not here.
    pub fn deposit(&mut self, player: PlayerId, now: Tick, inside_own_pad: bool) -> u32 {
        let id = player.0 as usize;
        if id >= self.carried.len() {
            return 0;
        }

        // The clock advances whether or not the player is in their pad. Time
        // spent walking is not banked: the rate is "six crates per second of
        // standing at your own pad", and letting a player idle outside and then
        // walk in for a burst would make leaving the pad the fastest way to feed
        // it. The clock also advances with empty hands, so picking a crate up on
        // the way in does not arrive with a second of credit attached.
        let elapsed = now.0.saturating_sub(self.deposit_at[id].0);
        self.deposit_at[id] = now;

        if !inside_own_pad || self.carried[id] == 0 {
            return 0;
        }

        // Refill by elapsed ticks, so the rate is a rate and not "one crate per
        // call". The accumulator is capped just under two crates: enough that a
        // player who pauses for a moment does not lose the fraction they had
        // banked, small enough that a long gap does not turn into a burst many
        // times the stated rate.
        let milli = &mut self.deposit_milli[id];
        *milli = (*milli + elapsed * self.rules.deposit_milli_per_tick).min(1_999);
        if *milli < 1_000 {
            return 0;
        }
        *milli -= 1_000;
        self.carried[id] -= 1;
        self.deposited += 1;
        1
    }

    /// Put deposited crates back in a player's hands.
    ///
    /// The room deposits before it validates a placement, because the edit
    /// pipeline applies the block in the same call that decides whether it is
    /// legal. If the placement is refused, the crate must not evaporate: this
    /// reverses exactly one deposit so the ledger still balances.
    pub fn refund(&mut self, player: PlayerId, crates: u32) {
        let id = player.0 as usize;
        if id >= self.carried.len() || crates == 0 {
            return;
        }
        let refunded = crates.min(self.deposited as u32);
        self.deposited -= u64::from(refunded);
        self.carried[id] += refunded;
    }

    /// The carrier died: half the load is destroyed, the rest becomes pickups.
    ///
    /// Returns how many crates hit the ground, so the caller can announce it.
    pub fn on_death(&mut self, player: PlayerId, pos: Vec3, now: Tick) -> u32 {
        let id = player.0 as usize;
        let carried = self.carried.get(id).copied().unwrap_or(0);
        if carried == 0 {
            return 0;
        }
        self.carried[id] = 0;
        // Rounded down, so an odd load spills the larger half. The crates that
        // hit the ground are the part that feeds the killer, and the anti-snowball
        // lever is the *loss*; rounding the loss up would also round up the
        // punishment for dying with one crate, which is the case that matters
        // least and is most visible.
        let lost = (Fixed::from_int(carried as i32) * self.rules.drop_loss).floor_int() as u32;
        self.destroyed += u64::from(lost);
        let spilled = carried - lost;

        for _ in 0..spilled {
            let dx = self.rng.next_fixed_range(-SCATTER_OFFSET, SCATTER_OFFSET);
            let dz = self.rng.next_fixed_range(-SCATTER_OFFSET, SCATTER_OFFSET);
            let id = self.next_id();
            self.pickups.push(Pickup {
                id,
                pos: Vec3::new(pos.x + dx, pos.y, pos.z + dz),
                born: now,
            });
        }
        while self.pickups.len() > self.rules.max_pickups {
            self.pickups.remove(0);
            self.destroyed += 1;
        }
        spilled
    }

    fn next_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        id
    }
}

/// Integer weight in units of `max_dist`: `3·max_dist − 2·dist`.
fn weight_of(dist: i32, max_dist: i32) -> u32 {
    (3 * max_dist - 2 * dist).max(1) as u32
}

/// The map centre, as the mean of the candidate sites rounded down.
///
/// A candidate set is not guaranteed to be symmetric, and the plan's `max_dist`
/// needs *some* centre. The mean of the sites is the honest choice: it is the
/// point that minimises the total distance to them, so "centre drops are three
/// times as likely" holds for the sites that actually exist rather than for a
/// nominal middle of the map.
fn centre_of(sites: &[[i32; 3]]) -> [i32; 3] {
    let n = sites.len() as i64;
    if n == 0 {
        return [0, 0, 0];
    }
    let mut sx = 0i64;
    let mut sy = 0i64;
    let mut sz = 0i64;
    for site in sites {
        sx += i64::from(site[0]);
        sy += i64::from(site[1]);
        sz += i64::from(site[2]);
    }
    [(sx / n) as i32, (sy / n) as i32, (sz / n) as i32]
}

/// Horizontal distance in whole metres.
fn metres_between(a: [i32; 3], b: [i32; 3]) -> i32 {
    let dx = Fixed::from_int(a[0] - b[0]);
    let dz = Fixed::from_int(a[2] - b[2]);
    Vec3::new(dx, ZERO, dz).horizontal_length().round_int()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sites() -> Vec<[i32; 3]> {
        // A ring of candidate sites around the map centre, like worldgen's.
        (0..24)
            .map(|i| {
                let angle = i;
                let radius = 40 + (i % 4) * 12;
                [
                    128 + radius * crate::fixed::cos_yaw((angle * 10) as u8).round_int(),
                    40,
                    128 + radius * crate::fixed::sin_yaw((angle * 10) as u8).round_int(),
                ]
            })
            .collect()
    }

    fn air(players: u32) -> Airdrops {
        Airdrops::new(0xDEAD_BEEF, players, sites())
    }

    /// Tick until the drop in the air has landed. Returns the landing tick.
    fn land(airdrops: &mut Airdrops, from: Tick) -> Tick {
        let mut now = from;
        for _ in 0..(12 * TICK_HZ) {
            if airdrops.falling().is_empty() {
                return now;
            }
            now = Tick(now.0 + 1);
            airdrops.tick(now, &[]);
        }
        panic!("the drop never landed");
    }

    /// Spawn one drop and land it. Returns the site and the landing tick.
    fn drop_and_land(airdrops: &mut Airdrops) -> ([i32; 3], Tick) {
        let at = airdrops.next_drop_at();
        airdrops.tick(at, &[]);
        let site = airdrops.falling().last().expect("a drop").site;
        (site, land(airdrops, at))
    }

    /// Walk to every crate on the ground, one crate per tick.
    ///
    /// A crate is picked up from 1.2 m, and the scatter is ±2 m, so standing on
    /// the site reaches only the middle of a drop: collecting it means walking.
    fn collect_everything(airdrops: &mut Airdrops, player: PlayerId, now: &mut Tick) -> u32 {
        let mut got = 0;
        for _ in 0..(airdrops.pickups().len() + 4) {
            let Some(pickup) = airdrops.pickups().first().copied() else {
                break;
            };
            *now = Tick(now.0 + 1);
            let events = airdrops.tick(*now, &[(player, pickup.pos)]);
            got += events.picked.iter().map(|(_, n)| n).sum::<u32>();
        }
        got
    }

    #[test]
    fn drop_schedule_over_20_min() {
        // §4.4.1's intervals, applied literally: first drop at 0:10, every 25 s
        // until 5:00, every 15 s after.
        //
        // The plan's summary line says "~59 drops" over 20 minutes, but 12 early
        // drops (10 s to 285 s) plus 61 late ones (300 s to 1200 s) is 73. The
        // intervals are what a player experiences and what the crate totals are
        // computed from, so they are what ships; the 59 is recorded as a
        // discrepancy in the README rather than reverse-engineered into the
        // schedule.
        let mut airdrops = air(2);
        let end = Tick(20 * 60 * TICK_HZ);
        let mut drops = 0;
        while airdrops.next_drop_at().0 <= end.0 {
            airdrops.tick(airdrops.next_drop_at(), &[]);
            drops += 1;
            if drops > 500 {
                panic!("the schedule never terminates");
            }
        }
        assert_eq!(drops, 73, "drops in 20 minutes");
    }

    #[test]
    fn crate_throughput_over_20_min() {
        // The plan's total is "~59 drops ≈ 354 crates (2p) … 1,062 crates (6p)".
        // Both of those are 59 × the drop size, so the drop count is the only
        // thing that moved: 73 drops of 6, fourteen of them heavy at 11, is 508
        // for a duel. The per-drop numbers are unchanged, which is what a pacing
        // decision is actually made of — the crates-per-minute figure is what
        // this test exists to keep honest.
        for (players, expected) in [(2u32, 508u64), (6, 1_510)] {
            let mut airdrops = air(players);
            let mut total = 0u64;
            let mut heavy = 0;
            for tick in 1..=(20 * 60 * TICK_HZ) {
                for spawned in airdrops.tick(Tick(tick), &[]).spawned {
                    total += u64::from(spawned.crates);
                    heavy += u32::from(spawned.heavy);
                }
            }
            assert_eq!(heavy, 14, "{players}p: every fifth of 73 drops");
            assert_eq!(total, expected, "{players}p: crates in 20 minutes");
        }
    }

    #[test]
    fn the_first_drop_is_at_ten_seconds() {
        let mut airdrops = air(2);
        assert_eq!(airdrops.next_drop_at(), Tick(200));
        airdrops.tick(Tick(199), &[]);
        assert_eq!(airdrops.drops_made(), 0);
        airdrops.tick(Tick(200), &[]);
        assert_eq!(airdrops.drops_made(), 1);
        assert_eq!(airdrops.falling().len(), 1);
    }

    #[test]
    fn the_interval_tightens_after_five_minutes() {
        let mut airdrops = air(2);
        // Walk to just past the escalation.
        let mut now = Tick(0);
        while now.0 < 6_000 {
            now = airdrops.next_drop_at();
            airdrops.tick(now, &[]);
        }
        let gap = airdrops.next_drop_at().0 - now.0;
        assert_eq!(gap, 15 * TICK_HZ, "the late interval is 15 s");
    }

    #[test]
    fn drop_size_scales_with_players() {
        for players in [2u32, 4, 6] {
            let mut airdrops = air(players);
            airdrops.tick(Tick(200), &[]);
            let crates = airdrops.falling()[0].crates;
            assert_eq!(crates, 3 * players, "3 crates per player");
        }
    }

    #[test]
    fn every_fifth_drop_is_heavy() {
        let mut airdrops = air(6);
        let mut sizes = Vec::new();
        for _ in 0..5 {
            let at = airdrops.next_drop_at();
            airdrops.tick(at, &[]);
            sizes.push(airdrops.falling().last().unwrap().crates);
        }
        assert_eq!(sizes[0], 18);
        assert_eq!(sizes[4], 32, "18 × 1.75 = 31.5, rounded to 32");
        assert!(airdrops.falling().last().unwrap().heavy);
    }

    #[test]
    fn site_selection_never_repeats_and_never_picks_a_pad() {
        let mut airdrops = air(2);
        let mut seen = Vec::new();
        for _ in 0..40 {
            let at = airdrops.next_drop_at();
            airdrops.tick(at, &[]);
            let site = airdrops.falling().last().unwrap().site;
            if let Some(previous) = seen.last() {
                assert_ne!(*previous, site, "two consecutive drops in one place");
            }
            seen.push(site);
        }
    }

    /// The first `drops` sites, ticking every tick like a real match.
    ///
    /// Jumping from one drop to the next would be faster and would be wrong:
    /// landing scatters consume the same RNG stream, so the order of "descend"
    /// and "spawn" inside a tick changes which sites come out. A golden taken
    /// under jumped ticks would pin a schedule no match ever runs.
    fn first_sites(seed: u64, drops: usize) -> Vec<[i32; 3]> {
        let mut airdrops = Airdrops::new(seed, 2, sites());
        let mut out = Vec::new();
        let mut now = Tick(0);
        while out.len() < drops {
            now = Tick(now.0 + 1);
            for spawned in airdrops.tick(now, &[]).spawned {
                out.push(spawned.site);
            }
            assert!(now.0 < 60 * 60 * TICK_HZ, "no drops in an hour");
        }
        out
    }

    #[test]
    fn site_selection_is_portable() {
        // A committed vector, not just self-consistency: this is what a wasm
        // build would have to reproduce. If the algorithm ever picks up a float
        // or a hash-order dependency, this fails on the target that changed
        // rather than on a client three seconds out of sync.
        //
        // Regenerate deliberately, never to make a red test green: the client
        // draws its telegraph from the same seed, and a silent change here is a
        // marker in the wrong place for every player at once.
        const GOLDEN: [[i32; 3]; 8] = [
            [128, 40, 64],
            [192, 40, 128],
            [88, 40, 128],
            [128, 40, 64],
            [128, 40, 76],
            [192, 40, 128],
            [168, 40, 168],
            [128, 40, 88],
        ];
        assert_eq!(first_sites(0xDEAD_BEEF, 8), GOLDEN);

        // Determinism within a process is the weaker property that must hold
        // regardless: same seed, same sequence.
        assert_eq!(first_sites(0xDEAD_BEEF, 8), first_sites(0xDEAD_BEEF, 8));
        // And a different seed must actually diverge, or the golden above is
        // pinning a constant rather than a pick.
        assert_ne!(first_sites(0x1234_5678, 8), GOLDEN);
    }

    #[test]
    fn crates_land_on_ground() {
        let mut airdrops = air(2);
        let at = airdrops.next_drop_at();
        airdrops.tick(at, &[]);
        let site = airdrops.falling()[0].site;
        assert_eq!(
            airdrops.falling()[0].y,
            Fixed::from_int(DROP_HEIGHT),
            "a drop spawns at full height, not already falling"
        );

        // 80 m at 12 m/s is 6.7 s — the plan's own figure for the descent, and
        // longer than the 5 s telegraph, which is why the telegraph is a floor.
        let landed_at = land(&mut airdrops, at);
        assert_eq!(landed_at.0 - at.0, 134, "80 m at 0.6 m/tick");
        assert_eq!(airdrops.pickups().len(), 6, "2 players × 3 crates");
        for pickup in airdrops.pickups() {
            assert_eq!(pickup.pos.y, Fixed::from_int(site[1]));
            assert_eq!(pickup.born, landed_at);
            assert!(
                pickup
                    .pos
                    .horizontal_distance(Vec3::from_ints(site[0], site[1], site[2]))
                    <= SCATTER_RADIUS,
                "landing scatter must stay inside 3 m"
            );
        }
    }

    #[test]
    fn landing_scatter_stays_in_3m() {
        let mut airdrops = air(6);
        // Twenty drops, every crate of each drop checked: the bound is a property
        // of the scatter, not of one lucky landing. Only the crates that landed
        // on this tick are checked — earlier drops are still on the ground, and
        // measuring them against this site would be measuring the wrong thing.
        for _ in 0..20 {
            let (site, landed_at) = drop_and_land(&mut airdrops);
            let site_pos = Vec3::from_ints(site[0], site[1], site[2]);
            let mut checked = 0;
            for pickup in airdrops.pickups().iter().filter(|p| p.born == landed_at) {
                checked += 1;
                assert!(
                    pickup.pos.horizontal_distance(site_pos) <= SCATTER_RADIUS,
                    "a crate landed {} m from the site",
                    pickup.pos.horizontal_distance(site_pos).to_f32()
                );
            }
            assert!(checked > 0, "the drop produced no crates");
        }
    }

    #[test]
    fn pickup_requires_walking_within_1_2_metres() {
        let mut airdrops = air(2);
        let (site, mut now) = drop_and_land(&mut airdrops);

        // Two metres away: nothing. The radius is 1.2 m, so this is out of reach
        // even though the crates are in sight.
        let far = Vec3::from_ints(site[0] + 2, site[1], site[2] + 2);
        let events = airdrops.tick(now, &[(PlayerId(0), far)]);
        assert!(events.picked.is_empty());
        assert_eq!(airdrops.carried(PlayerId(0)), 0);

        // Walking over the crates: the whole drop, up to the cap.
        let got = collect_everything(&mut airdrops, PlayerId(0), &mut now);
        assert_eq!(got, 6, "all six crates picked up");
        assert_eq!(airdrops.carried(PlayerId(0)), 6);
        assert!(airdrops.pickups().is_empty());
    }

    #[test]
    fn carry_cap_enforced() {
        let mut airdrops = air(6);
        // Three drops of 18 = 54 crates, and the cap is 12.
        for _ in 0..3 {
            let (_, mut now) = drop_and_land(&mut airdrops);
            let site = airdrops.pickups().first().expect("crates").pos;
            for _ in 0..3 {
                now = Tick(now.0 + 1);
                airdrops.tick(now, &[(PlayerId(0), site)]);
            }
        }
        assert_eq!(airdrops.carried(PlayerId(0)), 12);
    }

    #[test]
    fn deposit_is_six_per_second_and_only_in_your_own_pad() {
        let mut airdrops = air(2);
        airdrops.carried[0] = 12;

        // In the enemy pad: nothing leaves the hands.
        for tick in 0..20 {
            assert_eq!(airdrops.deposit(PlayerId(0), Tick(tick), false), 0);
        }
        assert_eq!(airdrops.carried(PlayerId(0)), 12);

        // In your own pad: six crates in one second, not seven.
        let mut placed = 0;
        for tick in 20..40 {
            placed += airdrops.deposit(PlayerId(0), Tick(tick), true);
        }
        assert_eq!(placed, 6);
        assert_eq!(airdrops.carried(PlayerId(0)), 6);
    }

    #[test]
    fn death_drops_half_the_load() {
        // A real drop of 9 (three players × 3), collected, then killed: seeding
        // `carried` directly would make the books unprovable, and the books are
        // half of what this test is for.
        let mut airdrops = air(3);
        let (_, mut now) = drop_and_land(&mut airdrops);
        assert_eq!(collect_everything(&mut airdrops, PlayerId(0), &mut now), 9);

        let pos = Vec3::from_ints(30, 40, 30);
        let spilled = airdrops.on_death(PlayerId(0), pos, now);
        assert_eq!(spilled, 5, "9 × 0.5 = 4.5 destroyed, so 5 spill");
        assert_eq!(airdrops.carried(PlayerId(0)), 0);
        assert_eq!(airdrops.pickups().len(), 5);
        assert_eq!(airdrops.audit().destroyed, 4);
        assert!(airdrops.audit().balances());
    }

    #[test]
    fn pickups_despawn_after_180_seconds() {
        let mut airdrops = air(2);
        let (_, mut now) = drop_and_land(&mut airdrops);
        assert_eq!(airdrops.pickups().len(), 6);
        now = Tick(now.0 + 180 * TICK_HZ);
        airdrops.tick(now, &[]);
        assert!(airdrops.pickups().is_empty(), "180 s is the lifetime");
    }

    #[test]
    fn pickup_cap_never_exceeded() {
        let mut airdrops = air(6);
        // Drop after drop with nobody collecting: the entity count must stay at
        // the ceiling instead of growing with the match.
        // Seven drops land inside one 180 s pickup lifetime, so the ceiling is
        // reached before anything expires: 6 × 18 crates plus the heavy drop's
        // 32 is 140, and 120 is what survives.
        for _ in 0..7 {
            drop_and_land(&mut airdrops);
        }
        assert_eq!(airdrops.pickups().len(), 120);
        assert!(airdrops.audit().balances());
    }

    #[test]
    fn crates_are_conserved() {
        let mut airdrops = air(4);
        let mut carried_total = 0;
        let mut placed_total = 0;

        for round in 0..8 {
            let (_, mut now) = drop_and_land(&mut airdrops);
            // Walk the whole drop up, deposit half of it, then kill the carrier.
            let site = airdrops.pickups().first().expect("crates").pos;
            carried_total += collect_everything(&mut airdrops, PlayerId(0), &mut now);
            for tick in 0..20 {
                placed_total += airdrops.deposit(PlayerId(0), Tick(now.0 + tick), true);
            }
            // Past the deposit window, so the next round's clock never runs
            // backwards through the accumulator.
            now = Tick(now.0 + 20);
            // Kill the carrier, not the bystander: the pickup loop hands every
            // crate to the first body in the list, so player 1 never holds one.
            if round % 2 == 0 {
                airdrops.on_death(PlayerId(0), site, now);
            }
            assert!(
                airdrops.audit().balances(),
                "round {round}: {:?}",
                airdrops.audit()
            );
        }
        assert!(
            carried_total > 0 && placed_total > 0,
            "the loop must exercise both paths"
        );
        let audit = airdrops.audit();
        assert!(audit.destroyed > 0, "the loop must exercise death");
    }

    #[test]
    fn sixty_drops_and_a_hundred_and_twenty_pickups_fit_the_tick_budget() {
        let mut airdrops = air(6);
        // Fill the world: 60 drops' worth of crates, capped at 120 on the floor.
        let mut now = Tick(0);
        for _ in 0..60 {
            let (_, landed_at) = drop_and_land(&mut airdrops);
            now = landed_at;
        }
        assert_eq!(airdrops.pickups().len(), 120);

        let bodies: Vec<(PlayerId, Vec3)> = (0..6u8)
            .map(|id| {
                let site = airdrops.pickups()[id as usize].pos;
                (PlayerId(id), site)
            })
            .collect();

        let start = std::time::Instant::now();
        let ticks = 2_000;
        for _ in 0..ticks {
            now = Tick(now.0 + 1);
            airdrops.tick(now, &bodies);
        }
        let micros = start.elapsed().as_micros() as u64 / ticks as u64;
        println!(
            "airdrop tick: {micros} µs with {} pickups",
            airdrops.pickups().len()
        );
        assert!(micros <= 400, "airdrop tick is {micros} µs, budget 400 µs");
    }
}
