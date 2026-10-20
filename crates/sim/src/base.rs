//! The core: the only structure whose HP ends a match.
//!
//! # One number does all the pacing work
//!
//! 10,000 HP behind a 0.35 damage multiplier means a bare-handed player needs
//! about 28 minutes of uninterrupted hitting, while a four-player push with a
//! ram needs about 80 seconds (§4.8). That gap is the game: chip damage is
//! real but never decisive, so the win condition is *presence at the core*,
//! which is what makes the airdrop and the delivery trip matter.
//!
//! Regeneration is zero on purpose. If the core healed, chip damage would be
//! free to ignore and the late game would flatten out; with zero regen, every
//! scratch a defender allows is still missing five minutes later.
//!
//! # Elimination is a single tick, by construction
//!
//! Decision D-02: when a core reaches zero, every living player on that team
//! dies on the *same tick*. There is no spectator mode, no "dying" state and no
//! grace period. [`Base::eliminate`] is written as one function taking the whole
//! team, so "same tick" is a property of the call graph rather than a promise
//! the match loop has to keep. [`PlayerLife`] has exactly two variants, so a
//! third one — `Spectator`, `Eliminated`, `Downed` — cannot be introduced
//! without breaking the exhaustive match in this module's tests.
//!
//! The alternative was tempting and is worth recording: a spectator mode would
//! have kept losing players in the room, which is good for retention, but it
//! costs a spectator camera, a spectator HUD, a separate snapshot stream, and
//! an information-leak question (spectators see enemy positions). Dropping it
//! removed all four and made defeat legible as a *result* rather than a state.

use crate::fixed::{Fixed, ZERO};
use crate::ids::{PlayerId, TeamId};
use crate::pad::{Pad, PAD_SIZE};
use crate::time::Tick;
use crate::tuning;

/// Core footprint along X and Z, in cells.
///
/// The plan fixes the core's *position* (centre of the base's 24 x 24 platform)
/// but not its size. Two cells keeps a 6-wide buildable ring around it inside
/// the 8 x 8 pad, which is enough for every recipe in §4.5.3 that is meant to
/// fit beside a core.
pub const CORE_FOOTPRINT: i32 = 2;

/// Core height in cells. Three is one more than a player's eye line, so a core
/// cannot be shot over from the pad surface.
pub const CORE_HEIGHT: i32 = 3;

/// Whether a player is still in the match.
///
/// Two variants, deliberately. See the module docs: a third variant would be a
/// spectator mode by another name, and `no_spectator_state_exists` is written
/// as an exhaustive match so adding one fails to compile.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PlayerLife {
    /// Playing, or waiting to respawn.
    Alive,
    /// Out of the match for good. There is no transition back.
    Dead,
}

impl PlayerLife {
    /// True while the player is still in the match.
    pub fn is_alive(self) -> bool {
        matches!(self, PlayerLife::Alive)
    }
}

/// A player as elimination sees them.
///
/// Deliberately minimal: elimination's rule is "everyone on this team who is
/// still alive", and it must not depend on anything else. A player mid-swing,
/// mid-jump or mid-air dies on the same tick as one standing still — the AC
/// asks for that explicitly, and the way to guarantee it is for the function to
/// have nothing else to look at.
///
/// `bot` is the one field here that elimination does not consult. It exists
/// because D-05's "bots cannot win" rule needs to know, at match level, which
/// players are human — and duplicating the player list into a second struct
/// just to carry one bool would give the match two places to disagree about who
/// is alive.
#[derive(Clone, Copy, Debug)]
pub struct PlayerSlot {
    /// Stable handle.
    pub id: PlayerId,
    /// Team the player is on.
    pub team: TeamId,
    /// Whether they are still in the match.
    pub life: PlayerLife,
    /// True when this slot is filled by a bot (D-05).
    pub bot: bool,
}

impl PlayerSlot {
    /// A living human on `team`.
    pub fn alive(id: PlayerId, team: TeamId) -> PlayerSlot {
        PlayerSlot {
            id,
            team,
            life: PlayerLife::Alive,
            bot: false,
        }
    }

    /// A living bot on `team`.
    pub fn bot(id: PlayerId, team: TeamId) -> PlayerSlot {
        PlayerSlot {
            id,
            team,
            life: PlayerLife::Alive,
            bot: true,
        }
    }

    /// True for a human still in the match.
    pub fn is_live_human(&self) -> bool {
        !self.bot && self.life.is_alive()
    }
}

/// What one elimination did, for the match log and the defeat screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Elimination {
    /// The team that lost its core.
    pub team: TeamId,
    /// The tick the core reached zero. Every death below happened here.
    pub at: Tick,
    /// Players killed by this elimination, in ascending id order.
    pub players_killed: Vec<PlayerId>,
    /// Crates destroyed when the pad was swept.
    pub crates_destroyed: usize,
}

impl Elimination {
    /// An elimination that did nothing, because the team was already out.
    fn nothing(team: TeamId, at: Tick) -> Elimination {
        Elimination {
            team,
            at,
            players_killed: Vec::new(),
            crates_destroyed: 0,
        }
    }

    /// True when this call actually changed anything.
    pub fn is_empty(&self) -> bool {
        self.players_killed.is_empty() && self.crates_destroyed == 0
    }
}

/// A team's core.
#[derive(Clone, Copy, Debug)]
pub struct Core {
    hp: i32,
    max_hp: i32,
    /// World cell of the core's minimum corner.
    origin: [i32; 3],
}

impl Core {
    /// A core at full health, with its minimum corner at `origin`.
    pub fn new(origin: [i32; 3]) -> Core {
        let max_hp = tuning::embedded().core.hp;
        Core {
            hp: max_hp,
            max_hp,
            origin,
        }
    }

    /// Current HP. Zero once destroyed; it never goes negative.
    #[inline]
    pub fn hp(&self) -> i32 {
        self.hp
    }

    /// Maximum HP, from tuning.
    #[inline]
    pub fn max_hp(&self) -> i32 {
        self.max_hp
    }

    /// World cell of the core's minimum corner.
    #[inline]
    pub fn origin(&self) -> [i32; 3] {
        self.origin
    }

    /// True while the core stands.
    #[inline]
    pub fn is_alive(&self) -> bool {
        self.hp > 0
    }

    /// Remaining HP as a fraction of maximum, for the HUD and the state hash.
    pub fn fraction(&self) -> Fixed {
        if self.max_hp <= 0 {
            return ZERO;
        }
        Fixed::from_int(self.hp) / Fixed::from_int(self.max_hp)
    }

    /// Apply `raw` incoming damage, returning the HP actually removed.
    ///
    /// The 0.35 multiplier is applied **per hit**, not per second, so a 25-damage
    /// sword removes 9 (8.75 rounded) rather than accumulating fractional
    /// damage. That is a deliberate choice: accumulating a fraction would make
    /// the core's HP depend on the order hits arrive in within a tick, which is
    /// exactly the kind of thing two clients can disagree about.
    ///
    /// Damage after destruction is ignored rather than clamped, so several
    /// attackers landing on the same tick all report "hit a dead core" instead
    /// of the last one looking like the killer.
    pub fn damage(&mut self, raw: i32) -> i32 {
        if !self.is_alive() || raw <= 0 {
            return 0;
        }
        let scaled = Fixed::from_int(raw) * tuning::embedded().core.damage_mult;
        let effective = scaled.round_int().max(0);
        let removed = effective.min(self.hp);
        self.hp -= removed;
        removed
    }

    /// Remove HP directly, with no multiplier. The only caller is overtime
    /// decay, via [`Base::decay`].
    pub(crate) fn remove_hp(&mut self, hp: i32) {
        self.hp = (self.hp - hp.max(0)).max(0);
    }

    /// The cells this core occupies, in world coordinates.
    ///
    /// Used to stamp the pad's blocked mask (§4.5.1 rule 5) and to test whether
    /// a projectile is aimed at the core rather than at a wall.
    pub fn cells(&self) -> impl Iterator<Item = [i32; 3]> + '_ {
        let o = self.origin;
        (0..CORE_HEIGHT).flat_map(move |y| {
            (0..CORE_FOOTPRINT)
                .flat_map(move |z| (0..CORE_FOOTPRINT).map(move |x| [o[0] + x, o[1] + y, o[2] + z]))
        })
    }
}

/// A team's base: its core, and the pad it builds on.
///
/// The pad itself lives in [`crate::pad::Pads`] rather than here, because crate
/// placement is enforced by ownership across *all* pads and a `Base` cannot see
/// its neighbours. The split costs one parameter on [`Base::eliminate`] and
/// keeps "crates may only go in your own pad" a single check in one place.
#[derive(Clone, Debug)]
pub struct Base {
    /// The team this base belongs to.
    pub team: TeamId,
    /// The core.
    pub core: Core,
    /// Tick the core reached zero, if it has.
    destroyed_at: Option<Tick>,
    /// Tick the elimination ran, if it has.
    eliminated_at: Option<Tick>,
}

impl Base {
    /// A base whose core's minimum corner is at `core_origin`.
    pub fn new(team: TeamId, core_origin: [i32; 3]) -> Base {
        Base {
            team,
            core: Core::new(core_origin),
            destroyed_at: None,
            eliminated_at: None,
        }
    }

    /// The core's minimum corner for a pad whose local origin is `pad_origin`.
    ///
    /// Centred on the pad, which is what "core at centre" in §4.3 means once
    /// the pad is the 8 x 8 buildable area inside the 24 x 24 platform.
    pub fn core_origin_for_pad(pad_origin: [i32; 3]) -> [i32; 3] {
        let offset = (PAD_SIZE - CORE_FOOTPRINT) / 2;
        [
            pad_origin[0] + offset,
            pad_origin[1],
            pad_origin[2] + offset,
        ]
    }

    /// Apply raw damage, recording the tick the core reaches zero.
    ///
    /// Returns the HP actually removed. Overtime decay (T-014) does **not** go
    /// through here: decay is not incoming damage, so it bypasses the 0.35
    /// multiplier by design. See [`Base::decay`].
    pub fn damage(&mut self, raw: i32, now: Tick) -> i32 {
        let removed = self.core.damage(raw);
        if removed > 0 && !self.core.is_alive() && self.destroyed_at.is_none() {
            self.destroyed_at = Some(now);
        }
        removed
    }

    /// Remove HP directly, bypassing the damage multiplier.
    ///
    /// This exists for exactly one caller: overtime decay (§4.2), which is a
    /// property of the clock rather than of an attacker. Routing it through
    /// [`Base::damage`] would make a 2%-per-30 s rule mean 0.7% per 30 s, which
    /// is a different — and much slower — guarantee of termination.
    pub fn decay(&mut self, hp: i32, now: Tick) -> i32 {
        if hp <= 0 || !self.core.is_alive() {
            return 0;
        }
        let removed = hp.min(self.core.hp());
        self.core.remove_hp(removed);
        if !self.core.is_alive() && self.destroyed_at.is_none() {
            self.destroyed_at = Some(now);
        }
        removed
    }

    /// True once the core has reached zero.
    pub fn is_destroyed(&self) -> bool {
        self.destroyed_at.is_some()
    }

    /// Tick the core reached zero.
    pub fn destroyed_at(&self) -> Option<Tick> {
        self.destroyed_at
    }

    /// True once D-02 has run for this team.
    pub fn is_eliminated(&self) -> bool {
        self.eliminated_at.is_some()
    }

    /// Tick the elimination ran.
    pub fn eliminated_at(&self) -> Option<Tick> {
        self.eliminated_at
    }

    /// Stamp the core's footprint into `pad` as blocked (§4.5.1 rule 5).
    ///
    /// Called once during base layout, before the match starts. Doing it at
    /// layout rather than on every placement is why the blocked mask can be
    /// kept out of the stability hash — it is static geometry, not a change.
    pub fn stamp_protected_volume(&self, pad: &mut Pad) -> usize {
        let mut stamped = 0;
        for cell in self.core.cells() {
            if pad.block(pad.to_local(cell), true) {
                stamped += 1;
            }
        }
        stamped
    }

    /// Run D-02: sweep the pad, kill every living player on the team.
    ///
    /// Idempotent. A second call — two damage events in one tick, or a match
    /// loop that processes the same elimination twice — returns an empty record
    /// and changes nothing. That matters because `Elimination` feeds the kill
    /// feed and the defeat screen; emitting it twice would double both.
    ///
    /// Players are killed in one loop over the whole team, so there is no
    /// intermediate state where some of the team is dead and some is not.
    pub fn eliminate(&mut self, pad: &mut Pad, players: &mut [PlayerSlot]) -> Elimination {
        let at = match (self.destroyed_at, self.eliminated_at) {
            // Already out, or the core is still standing: nothing to do.
            (_, Some(already)) => return Elimination::nothing(self.team, already),
            (None, None) => return Elimination::nothing(self.team, Tick::ZERO),
            (Some(at), None) => at,
        };

        let crates_destroyed = pad.count();
        pad.seal();

        let mut players_killed = Vec::new();
        for player in players.iter_mut() {
            if player.team == self.team && player.life.is_alive() {
                player.life = PlayerLife::Dead;
                players_killed.push(player.id);
            }
        }
        players_killed.sort_unstable();

        self.eliminated_at = Some(at);
        Elimination {
            team: self.team,
            at,
            players_killed,
            crates_destroyed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::WeaponStats;

    /// A stand-in for an attacker that fires one weapon at the core forever.
    ///
    /// This is not the combat system — it is the *damage pipeline* the AC is
    /// about: a weapon's damage arriving at the core at its own cadence, with
    /// the multiplier and the per-hit rounding applied. Pulling in the full
    /// melee/projectile path here would test T-011 again and obscure which
    /// number broke if the pacing drifts.
    struct Striker {
        damage: i32,
        period: u32,
        wait: u32,
    }

    impl Striker {
        fn new(weapon: &WeaponStats) -> Striker {
            Striker {
                damage: weapon.damage,
                period: weapon.cooldown_ticks,
                wait: 0,
            }
        }

        fn strike(&mut self) -> Option<i32> {
            if self.wait == 0 {
                // `period - 1`, not `period`: the decrement below runs on the
                // tick after the strike, so a full `period` would fire every
                // period + 1 ticks and quietly make every weapon 10% slower.
                self.wait = self.period.saturating_sub(1);
                Some(self.damage)
            } else {
                self.wait -= 1;
                None
            }
        }
    }

    /// Run `attackers` against `base` for at most `max_ticks`; return the tick
    /// the core reached zero, or `max_ticks` if it survived.
    fn run(base: &mut Base, attackers: &mut [Striker], max_ticks: u32) -> u32 {
        for tick in 0..max_ticks {
            for attacker in attackers.iter_mut() {
                if let Some(damage) = attacker.strike() {
                    base.damage(damage, Tick(tick));
                }
            }
            if base.is_destroyed() {
                return tick + 1;
            }
        }
        max_ticks
    }

    /// Raw damage that destroys a 10,000 HP core through the 0.35 multiplier.
    ///
    /// 10,000 raw is only 3,500 effective — the multiplier applies to every
    /// source, including the test's.
    const KILL_RAW: i32 = 30_000;

    fn team() -> TeamId {
        TeamId(0)
    }

    fn base() -> Base {
        Base::new(team(), [3, 41, 3])
    }

    fn pad() -> Pad {
        Pad::new([0, 40, 0])
    }

    fn weapon(name: &str) -> WeaponStats {
        let t = tuning::embedded();
        match name {
            "fists" => t.weapons.fists,
            "sword" => t.weapons.sword,
            "swordsman_blade" => t.weapons.swordsman_blade,
            "archer_bow" => t.weapons.archer_bow,
            "mage_bolt" => t.weapons.mage_bolt,
            _ => panic!("unknown weapon {name}"),
        }
    }

    /// The ram from `R10_RAM`: 250 structure damage every 2.0 s.
    fn ram() -> Striker {
        Striker {
            damage: 250,
            period: 40,
            wait: 0,
        }
    }

    fn ticks_to_seconds(ticks: u32) -> f32 {
        ticks as f32 / 20.0
    }

    #[test]
    fn core_matches_the_documented_numbers() {
        let b = base();
        assert_eq!(b.core.max_hp(), 10_000);
        assert_eq!(b.core.hp(), 10_000);
        assert!(b.core.is_alive());
        assert!(!b.is_destroyed());

        let t = tuning::embedded();
        assert_eq!(t.core.hp, 10_000);
        assert_eq!(t.core.regen, ZERO, "regen is zero by design");
    }

    #[test]
    fn the_multiplier_applies_to_every_source() {
        // §4.8: "all sources". A sword hit, an arrow and a ram impact all take
        // the same 0.35, because the multiplier models the core's armour, not
        // the attacker's weapon type.
        for raw in [8, 12, 20, 25, 30, 40, 250] {
            let mut b = base();
            let removed = b.damage(raw, Tick(0));
            let expected = (Fixed::from_int(raw) * tuning::embedded().core.damage_mult).round_int();
            assert_eq!(removed, expected, "raw {raw}");
        }
    }

    #[test]
    fn per_hit_rounding_is_deterministic_and_not_accumulated() {
        // 25 * 0.35 = 8.75, which rounds to 9 every time. Ten hits remove 90,
        // not 87 and not 88: no fractional damage is carried between hits, so
        // hit order inside a tick cannot change the result.
        let mut b = base();
        for _ in 0..10 {
            assert_eq!(b.damage(25, Tick(0)), 9);
        }
        assert_eq!(b.core.hp(), 10_000 - 90);

        // And a hit too small to round up to 1 does nothing at all.
        let mut b = base();
        assert_eq!(b.damage(1, Tick(0)), 0, "1 * 0.35 rounds to 0");
        assert_eq!(b.core.hp(), 10_000);
    }

    #[test]
    fn bare_hands_cannot_win() {
        // AC: 1 player, fists, 25 simulated minutes → core HP > 0.
        // §4.8 says 29.8 min; per-hit rounding gives 27.8. Both are past the
        // 25-minute mark, which is the property that matters: a lone player
        // without a pickup cannot end the match by holding down attack.
        let mut b = base();
        let mut attackers = [Striker::new(&weapon("fists"))];
        let ticks = run(&mut b, &mut attackers, 25 * 60 * 20);
        let minutes = ticks_to_seconds(ticks) / 60.0;
        assert!(
            b.core.hp() > 0,
            "fists destroyed a 10,000 HP core in {minutes:.1} minutes"
        );
        println!(
            "fists: {} HP left after 25 min ({:.1} min to destroy)",
            b.core.hp(),
            10_000.0 / (10_000 - b.core.hp()) as f32 * 25.0
        );
    }

    #[test]
    fn squad_push_takes_70_to_90s() {
        // AC: 4 players + ram, uncontested. §4.8's table says 83 s.
        let mut b = base();
        let sword = weapon("sword");
        let mut attackers = vec![
            Striker::new(&sword),
            Striker::new(&sword),
            Striker::new(&sword),
            Striker::new(&sword),
            ram(),
        ];
        let ticks = run(&mut b, &mut attackers, 20 * 60 * 5);
        let seconds = ticks_to_seconds(ticks);
        println!("4 players + ram: {seconds:.1} s");
        assert!(
            (70.0..=90.0).contains(&seconds),
            "expected 70-90 s, got {seconds:.1} s"
        );
    }

    #[test]
    fn four_buddies_shorten_the_push_to_about_67s() {
        // The T-013 AC names "4 players + ram + 4 buddies" and puts it in the
        // same 70-90 s window as the ram-only push, but §4.8's own table gives
        // 67 s for that composition — adding attackers cannot make a push
        // slower. The table has the arithmetic behind it, so this test asserts
        // the table's number and the deviation is recorded in the README.
        let mut b = base();
        let sword = weapon("sword");
        let mut attackers = vec![
            Striker::new(&sword),
            Striker::new(&sword),
            Striker::new(&sword),
            Striker::new(&sword),
            ram(),
            // Four tier-2 buddies: two swordsmen, an archer and a mage.
            Striker::new(&weapon("swordsman_blade")),
            Striker::new(&weapon("swordsman_blade")),
            Striker::new(&weapon("archer_bow")),
            Striker::new(&weapon("mage_bolt")),
        ];
        let ticks = run(&mut b, &mut attackers, 20 * 60 * 5);
        let seconds = ticks_to_seconds(ticks);
        println!("4 players + ram + 4 buddies: {seconds:.1} s");
        assert!(
            (60.0..=70.0).contains(&seconds),
            "expected ~67 s per §4.8, got {seconds:.1} s"
        );
    }

    #[test]
    fn the_core_never_heals() {
        // There is no heal method, and no tick function: HP only ever falls.
        // The test states the property rather than the absence of a method.
        let mut b = base();
        b.damage(25, Tick(0));
        let after = b.core.hp();
        for tick in 1..10_000 {
            b.damage(0, Tick(tick));
        }
        assert_eq!(b.core.hp(), after, "HP changed without any damage");
    }

    #[test]
    fn damage_after_destruction_is_ignored() {
        let mut b = base();
        b.damage(KILL_RAW, Tick(0));
        assert_eq!(b.core.hp(), 0);
        assert!(b.is_destroyed());
        assert_eq!(b.damage(25, Tick(1)), 0, "a dead core takes no damage");
        assert_eq!(b.core.hp(), 0, "and never goes negative");
        assert_eq!(
            b.destroyed_at(),
            Some(Tick(0)),
            "the destruction tick is the first one, not the last"
        );
    }

    #[test]
    fn elimination_kills_all_living_players_same_tick() {
        // AC: 3 players alive, 2 mid-swing, all DEAD on the tick the core
        // reaches zero, with no intermediate state.
        let mut b = base();
        let mut p = pad();
        p.set([0, 0, 0], true);
        p.set([1, 0, 0], true);

        let mut players = vec![
            PlayerSlot::alive(PlayerId(0), team()),
            PlayerSlot::alive(PlayerId(1), team()),
            PlayerSlot::alive(PlayerId(2), team()),
        ];
        // Two of them are in the middle of an attack. Elimination takes no
        // notice of that: it has nothing but `life` and `team` to look at.
        let mid_swing = [false, true, true];
        assert_eq!(mid_swing.iter().filter(|s| **s).count(), 2);

        b.damage(KILL_RAW, Tick(700));
        assert!(b.is_destroyed());
        let e = b.eliminate(&mut p, &mut players);

        assert_eq!(e.at, Tick(700), "every death happened on this tick");
        assert_eq!(
            e.players_killed,
            vec![PlayerId(0), PlayerId(1), PlayerId(2)],
            "all three, including both mid-swing players"
        );
        assert!(
            players.iter().all(|p| p.life == PlayerLife::Dead),
            "no player may be left in an intermediate state"
        );
        assert_eq!(e.crates_destroyed, 2);
        assert!(b.is_eliminated());
        assert_eq!(b.eliminated_at(), Some(Tick(700)));
    }

    #[test]
    fn elimination_ignores_other_teams() {
        let mut b = base();
        let mut p = pad();
        let mut players = vec![
            PlayerSlot::alive(PlayerId(0), team()),
            PlayerSlot::alive(PlayerId(1), TeamId(1)),
        ];
        b.damage(KILL_RAW, Tick(5));
        let e = b.eliminate(&mut p, &mut players);
        assert_eq!(e.players_killed, vec![PlayerId(0)]);
        assert_eq!(players[1].life, PlayerLife::Alive);
    }

    #[test]
    fn elimination_is_idempotent() {
        let mut b = base();
        let mut p = pad();
        p.set([0, 0, 0], true);
        let mut players = vec![PlayerSlot::alive(PlayerId(0), team())];
        b.damage(KILL_RAW, Tick(10));

        let first = b.eliminate(&mut p, &mut players);
        assert_eq!(first.players_killed.len(), 1);
        assert_eq!(first.crates_destroyed, 1);

        let second = b.eliminate(&mut p, &mut players);
        assert!(
            second.is_empty(),
            "a second elimination must not re-report kills or crates: {second:?}"
        );
        assert_eq!(second.at, Tick(10), "and it keeps the original tick");
        assert_eq!(p.count(), 0);
    }

    #[test]
    fn elimination_does_nothing_while_the_core_stands() {
        let mut b = base();
        let mut p = pad();
        let mut players = vec![PlayerSlot::alive(PlayerId(0), team())];
        let e = b.eliminate(&mut p, &mut players);
        assert!(e.is_empty());
        assert_eq!(players[0].life, PlayerLife::Alive);
        assert!(!p.is_sealed());
    }

    #[test]
    fn no_spectator_state_exists() {
        // The AC asks for a compile-time assertion, and this is it: the match
        // below is exhaustive over PlayerLife. Introducing a third variant —
        // Spectator, Eliminated, Downed — makes this function fail to compile,
        // which is a stronger guarantee than any runtime check.
        fn label(life: PlayerLife) -> &'static str {
            match life {
                PlayerLife::Alive => "alive",
                PlayerLife::Dead => "dead",
            }
        }
        assert_eq!(label(PlayerLife::Alive), "alive");
        assert_eq!(label(PlayerLife::Dead), "dead");
        // And the only way out of Alive is Dead, in one step.
        assert!(PlayerLife::Alive.is_alive());
        assert!(!PlayerLife::Dead.is_alive());
    }

    #[test]
    fn the_pad_is_sealed_after_elimination() {
        let mut pads = crate::pad::Pads::new();
        let t = pads.push(pad());
        let mut players = vec![PlayerSlot::alive(PlayerId(0), t)];
        pads.get_mut(t).unwrap().set([0, 0, 0], true);

        let mut b = base();
        b.damage(KILL_RAW, Tick(1));
        let mut pad_copy = *pads.get(t).unwrap();
        b.eliminate(&mut pad_copy, &mut players);
        *pads.get_mut(t).unwrap() = pad_copy;

        assert!(pads.get(t).unwrap().is_sealed());
        assert_eq!(pads.get(t).unwrap().count(), 0, "crates are destroyed");
        assert_eq!(
            pads.place(t, [2, 0, 2], true),
            Err(crate::pad::PlaceError::Eliminated),
            "the volume is dead ground"
        );
    }

    #[test]
    fn cannot_build_on_core() {
        // §4.5.1 rule 5: no cluster member may overlap the core's volume.
        let pad_origin = [0, 40, 0];
        let mut pads = crate::pad::Pads::new();
        let t = pads.push(Pad::new(pad_origin));
        let b = Base::new(t, Base::core_origin_for_pad(pad_origin));

        let stamped = b.stamp_protected_volume(pads.get_mut(t).unwrap());
        assert_eq!(
            stamped,
            (CORE_FOOTPRINT * CORE_FOOTPRINT * CORE_HEIGHT) as usize,
            "the whole core volume must be blocked"
        );

        // Every cell of the core rejects a crate.
        for cell in b.core.cells() {
            assert_eq!(
                pads.place(t, cell, true),
                Err(crate::pad::PlaceError::Protected),
                "{cell:?} is inside the core"
            );
        }
        // A cell just outside the footprint is buildable, so the block is not
        // a whole-pad lock.
        let outside = [pad_origin[0], pad_origin[1], pad_origin[2]];
        assert_eq!(pads.place(t, outside, true), Ok(true));
    }

    #[test]
    fn the_core_sits_at_the_centre_of_its_pad() {
        let origin = Base::core_origin_for_pad([0, 40, 0]);
        assert_eq!(origin, [3, 40, 3], "2x2 centred in an 8x8 pad");
        let b = Base::new(team(), origin);
        let cells: Vec<[i32; 3]> = b.core.cells().collect();
        assert_eq!(cells.len(), 12);
        assert_eq!(cells[0], [3, 40, 3]);
        assert_eq!(cells[11], [4, 42, 4]);
    }
}
