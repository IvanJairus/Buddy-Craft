//! Match-level rules: modes, caps, win conditions and overtime decay.
//!
//! Everything here is a pure function of its arguments. No clocks, no state, no
//! tuning reads except through [`MatchRules::from_tuning`]. That is deliberate:
//! these are the rules a player can argue about ("why did the match end?"), so
//! each one has to be testable in isolation and readable without a simulation.
//!
//! # The cap matrix is a table, not a formula
//!
//! §4.7 gives nine rows. A formula could reproduce them — FFA caps at 3 for up
//! to three players and 2 beyond, Teams at 5 unless there are three teams — but
//! a formula would also silently accept a tenth combination nobody designed.
//! The table is the specification, so it is written out and the lookup is an
//! exact match: an unlisted `(mode, players, teams)` is an error, not a guess.
//!
//! # Why the win check is ordered the way it is
//!
//! Three conditions can be true at once when every core dies on the same tick.
//! The order below is the one that produces the *most informative* reason:
//!
//! 1. **No cores standing** → draw. This is reachable in normal play: overtime
//!    decay hits every core at once, so two turtle teams die together.
//! 2. **No humans left** → `NoHumans`, winner `None` (D-05). This must be
//!    checked before "last team standing", otherwise a bot team that outlives
//!    the humans would be declared the winner.
//! 3. **One core standing** → that team wins.
//!
//! Condition 1 makes condition 2 unreachable when every core is down, which is
//! correct: a mutual destruction is a draw regardless of who was a bot.

use crate::fixed::{Fixed, ONE};
use crate::ids::{TeamId, MAX_TEAMS};
use crate::time::{Tick, TICK_HZ};
use crate::tuning;

/// Team composition mode (§4.7).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Every player is their own team.
    Ffa,
    /// Players are grouped into two or three teams.
    Teams,
}

impl Mode {
    /// Human-readable name, for logs and the results screen.
    pub fn name(self) -> &'static str {
        match self {
            Mode::Ffa => "FFA",
            Mode::Teams => "Teams",
        }
    }
}

/// One row of the §4.7 cap matrix.
#[derive(Clone, Copy, Debug)]
pub struct CapRow {
    /// Mode this row applies to.
    pub mode: Mode,
    /// Total players in the room.
    pub players: u8,
    /// Number of teams.
    pub teams: u8,
    /// Buddies allowed per team.
    pub buddies_per_team: u8,
}

impl CapRow {
    /// Players on each team.
    pub fn per_team(&self) -> u8 {
        self.players / self.teams
    }

    /// Buddies across the whole match.
    pub fn total_buddies(&self) -> u16 {
        u16::from(self.buddies_per_team) * u16::from(self.teams)
    }

    /// Every entity the server has to simulate: players plus buddies.
    pub fn total_entities(&self) -> u16 {
        u16::from(self.players) + self.total_buddies()
    }
}

/// §4.7, row for row. This *is* the specification.
pub const CAP_MATRIX: [CapRow; 9] = [
    CapRow {
        mode: Mode::Ffa,
        players: 2,
        teams: 2,
        buddies_per_team: 3,
    },
    CapRow {
        mode: Mode::Ffa,
        players: 3,
        teams: 3,
        buddies_per_team: 3,
    },
    CapRow {
        mode: Mode::Ffa,
        players: 4,
        teams: 4,
        buddies_per_team: 2,
    },
    CapRow {
        mode: Mode::Ffa,
        players: 5,
        teams: 5,
        buddies_per_team: 2,
    },
    CapRow {
        mode: Mode::Ffa,
        players: 6,
        teams: 6,
        buddies_per_team: 2,
    },
    CapRow {
        mode: Mode::Teams,
        players: 2,
        teams: 2,
        buddies_per_team: 4,
    },
    CapRow {
        mode: Mode::Teams,
        players: 4,
        teams: 2,
        buddies_per_team: 5,
    },
    CapRow {
        mode: Mode::Teams,
        players: 6,
        teams: 2,
        buddies_per_team: 5,
    },
    CapRow {
        mode: Mode::Teams,
        players: 6,
        teams: 3,
        buddies_per_team: 4,
    },
];

/// The largest entity count any row produces (§4.7's worst case: 18).
pub const MAX_ENTITIES: u16 = 18;

/// Look up the cap row for a room shape, or `None` if it is not a legal one.
pub fn cap_row(mode: Mode, players: u8, teams: u8) -> Option<CapRow> {
    CAP_MATRIX
        .iter()
        .copied()
        .find(|r| r.mode == mode && r.players == players && r.teams == teams)
}

/// Buddies allowed per team for a room shape.
pub fn buddy_cap(mode: Mode, players: u8, teams: u8) -> Option<u8> {
    cap_row(mode, players, teams).map(|r| r.buddies_per_team)
}

/// A resolved room shape: how many teams, and how big each one is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Layout {
    /// The mode this layout was resolved for.
    pub mode: Mode,
    /// Number of teams.
    pub teams: u8,
    /// Players on each team. Always divides `players` evenly.
    pub per_team: u8,
    /// Buddies allowed per team.
    pub buddy_cap: u8,
}

impl Layout {
    /// Resolve `(mode, players, teams)` into a layout, or `None` if illegal.
    ///
    /// `teams` is ignored in FFA, where every player is their own team: passing
    /// a conflicting number is a caller bug, so it is rejected rather than
    /// silently corrected.
    pub fn resolve(mode: Mode, players: u8, teams: Option<u8>) -> Option<Layout> {
        let teams = match mode {
            Mode::Ffa => {
                if teams.is_some_and(|t| t != players) {
                    return None;
                }
                players
            }
            Mode::Teams => teams.unwrap_or(2),
        };
        let row = cap_row(mode, players, teams)?;
        Some(Layout {
            mode,
            teams: row.teams,
            per_team: row.per_team(),
            buddy_cap: row.buddies_per_team,
        })
    }

    /// Total players this layout seats.
    pub fn players(&self) -> u8 {
        self.teams * self.per_team
    }

    /// Assign player slots, in join order, to teams.
    ///
    /// FFA is one player per team; Teams fills teams in join order, so the
    /// first `per_team` players are team 0. That is the "auto-balance" of
    /// §4.10 — a manual swap happens in the lobby, before the match, and is
    /// applied to the same slot list.
    pub fn assign(&self) -> Vec<TeamId> {
        self.seat(self.players(), 0)
            .into_iter()
            .map(|s| s.team)
            .collect()
    }

    /// Seat `humans` humans and `bots` bots, balancing both.
    ///
    /// Humans are spread across teams first (`i % teams`), because D-05 rule 1
    /// says bots only ever *top up* a team that already has a human on it.
    /// Bots then fill whichever team is currently smallest, lowest team id on a
    /// tie — so a 4-player 2v2 with one human and three bots puts the human on
    /// team 0 and then balances 2 v 2 rather than stacking bots on team 1.
    ///
    /// Returns exactly `humans + bots` seats; the caller validates the total
    /// against [`Layout::players`].
    pub fn seat(&self, humans: u8, bots: u8) -> Vec<Seat> {
        let mut seats = Vec::with_capacity((humans + bots) as usize);
        let mut counts = [0u8; MAX_TEAMS];
        for i in 0..humans {
            let team = TeamId(i % self.teams.max(1));
            counts[team.0 as usize] += 1;
            seats.push(Seat { team, bot: false });
        }
        for _ in 0..bots {
            let mut best = 0usize;
            for t in 1..self.teams as usize {
                if counts[t] < counts[best] {
                    best = t;
                }
            }
            counts[best] += 1;
            seats.push(Seat {
                team: TeamId(best as u8),
                bot: true,
            });
        }
        seats
    }
}

/// One player's place in the room.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Seat {
    /// Assigned team.
    pub team: TeamId,
    /// True when this seat is filled by a bot.
    pub bot: bool,
}

/// One team, as the win check sees it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TeamStatus {
    /// Which team.
    pub id: TeamId,
    /// True while the team's core stands.
    pub core_alive: bool,
    /// True while at least one human on the team is still in the match.
    pub human_alive: bool,
}

/// Why a match ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EndReason {
    /// One team's core is the only one left.
    LastTeamStanding,
    /// Every core fell on the same tick.
    Draw,
    /// The last human was eliminated (D-05). Bots do not win.
    NoHumans,
}

impl EndReason {
    /// Text for the results screen.
    pub fn describe(self) -> &'static str {
        match self {
            EndReason::LastTeamStanding => "last core standing",
            EndReason::Draw => "all cores destroyed",
            EndReason::NoHumans => "no human survivor",
        }
    }
}

/// The match's outcome.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// Still being played.
    Running,
    /// Over. `winner` is `None` for a draw and for `NoHumans`.
    Ended {
        /// The winning team, or `None`.
        winner: Option<TeamId>,
        /// Why it ended.
        reason: EndReason,
    },
}

impl Outcome {
    /// True once the match is over.
    pub fn is_over(&self) -> bool {
        matches!(self, Outcome::Ended { .. })
    }

    /// The winning team, if there is one.
    pub fn winner(&self) -> Option<TeamId> {
        match self {
            Outcome::Running => None,
            Outcome::Ended { winner, .. } => *winner,
        }
    }

    /// Why the match ended, if it has.
    pub fn reason(&self) -> Option<EndReason> {
        match self {
            Outcome::Running => None,
            Outcome::Ended { reason, .. } => Some(*reason),
        }
    }
}

/// Decide whether the match is over. See the module docs for the ordering.
pub fn check_outcome(teams: &[TeamStatus]) -> Outcome {
    let cores = teams.iter().filter(|t| t.core_alive).count();

    // 1. Mutual destruction is a draw, whoever was a bot.
    if cores == 0 {
        return Outcome::Ended {
            winner: None,
            reason: EndReason::Draw,
        };
    }

    // 2. D-05: the last human leaving ends the match, and bots cannot inherit
    //    the win. Checked before "last standing" for exactly that reason.
    if !teams.iter().any(|t| t.human_alive) {
        return Outcome::Ended {
            winner: None,
            reason: EndReason::NoHumans,
        };
    }

    // 3. One core left wins it.
    if cores == 1 {
        let winner = teams.iter().find(|t| t.core_alive).map(|t| t.id);
        return Outcome::Ended {
            winner,
            reason: EndReason::LastTeamStanding,
        };
    }

    Outcome::Running
}

/// Seconds between overtime decay steps (§4.2).
pub const DECAY_PERIOD_S: u32 = 30;

/// Overtime decay and the match clock.
#[derive(Clone, Copy, Debug)]
pub struct MatchRules {
    /// Nominal match length, from tuning. Informational: the match ends when a
    /// core does, not when the clock says so.
    pub target_minutes: u32,
    /// Seconds at which decay starts.
    pub overtime_start_s: u32,
    /// Fraction of max HP removed per [`DECAY_PERIOD_S`] step.
    pub decay_per_step: Fixed,
}

impl MatchRules {
    /// Load the rules from the embedded tuning.
    pub fn from_tuning() -> MatchRules {
        let t = tuning::embedded();
        MatchRules {
            target_minutes: t.match_cfg.target_minutes,
            overtime_start_s: t.match_cfg.overtime_start_s,
            decay_per_step: t.match_cfg.decay_per_30s,
        }
    }

    /// True once the clock has passed the overtime threshold.
    pub fn is_overtime(&self, now: Tick) -> bool {
        now.0 / TICK_HZ >= self.overtime_start_s
    }

    /// Whole decay steps elapsed at `now`.
    pub fn decay_steps(&self, now: Tick) -> u32 {
        let elapsed_s = now.0 / TICK_HZ;
        if elapsed_s < self.overtime_start_s {
            return 0;
        }
        (elapsed_s - self.overtime_start_s) / DECAY_PERIOD_S
    }

    /// Total HP a core should have lost to decay by `now`.
    ///
    /// Cumulative rather than incremental, so the caller stores how much it has
    /// already applied and applies the difference. That makes the function
    /// idempotent: calling it twice in a tick cannot double the decay, which a
    /// "decay is due this tick" predicate would allow.
    pub fn cumulative_decay(&self, now: Tick, max_hp: i32) -> i32 {
        let steps = self.decay_steps(now);
        if steps == 0 || max_hp <= 0 {
            return 0;
        }
        (Fixed::from_int(max_hp) * self.decay_per_step * Fixed::from_int(steps as i32)).round_int()
    }

    /// Seconds until a full-health core is destroyed by decay alone.
    ///
    /// This is the bound that makes `match_always_terminates` true, and it is
    /// much longer than the match's target length — see the README.
    pub fn decay_only_duration_s(&self) -> u32 {
        let raw = self.decay_per_step.0;
        if raw <= 0 {
            return u32::MAX;
        }
        // ceil(1 / fraction) in raw units, so no fixed-point division is
        // needed: a 2% step is 1311 raw, and (65536 + 1310) / 1311 = 51 — the
        // 51st step is the one that takes the last HP.
        let steps = ((ONE.0 + raw - 1) / raw).max(1) as u32;
        self.overtime_start_s + steps * DECAY_PERIOD_S
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(id: u8, core_alive: bool, human_alive: bool) -> TeamStatus {
        TeamStatus {
            id: TeamId(id),
            core_alive,
            human_alive,
        }
    }

    #[test]
    fn cap_matrix() {
        // The AC: all nine rows of §4.7, enforced exactly.
        let expected = [
            (Mode::Ffa, 2, 2, 1, 3, 6, 8),
            (Mode::Ffa, 3, 3, 1, 3, 9, 12),
            (Mode::Ffa, 4, 4, 1, 2, 8, 12),
            (Mode::Ffa, 5, 5, 1, 2, 10, 15),
            (Mode::Ffa, 6, 6, 1, 2, 12, 18),
            (Mode::Teams, 2, 2, 1, 4, 8, 10),
            (Mode::Teams, 4, 2, 2, 5, 10, 14),
            (Mode::Teams, 6, 2, 3, 5, 10, 16),
            (Mode::Teams, 6, 3, 2, 4, 12, 18),
        ];
        assert_eq!(expected.len(), CAP_MATRIX.len());

        for (mode, players, teams, per_team, cap, total, entities) in expected {
            let row = cap_row(mode, players, teams).unwrap_or_else(|| {
                panic!("{mode:?} {players}p {teams}t missing from the cap matrix")
            });
            assert_eq!(
                row.per_team(),
                per_team,
                "{mode:?} {players}p {teams}t per-team"
            );
            assert_eq!(
                row.buddies_per_team, cap,
                "{mode:?} {players}p {teams}t cap"
            );
            assert_eq!(
                row.total_buddies(),
                total,
                "{mode:?} {players}p {teams}t buddies"
            );
            assert_eq!(
                row.total_entities(),
                entities,
                "{mode:?} {players}p {teams}t entities"
            );
        }

        // Nothing outside the table resolves.
        assert!(
            cap_row(Mode::Ffa, 6, 3).is_none(),
            "FFA is one player per team"
        );
        assert!(
            cap_row(Mode::Teams, 3, 2).is_none(),
            "3 players cannot split"
        );
        assert!(
            cap_row(Mode::Teams, 6, 4).is_none(),
            "no 4-team mode exists"
        );
        assert!(
            cap_row(Mode::Ffa, 1, 1).is_none(),
            "a duel needs two players"
        );
        assert!(
            cap_row(Mode::Ffa, 7, 7).is_none(),
            "6 players is the ceiling"
        );
    }

    #[test]
    fn the_worst_case_entity_budget_holds() {
        // §4.7: "the number the entity budget is built around".
        for row in CAP_MATRIX {
            assert!(
                row.total_entities() <= MAX_ENTITIES,
                "{:?} {}p {}t produces {} entities",
                row.mode,
                row.players,
                row.teams,
                row.total_entities()
            );
        }
        assert_eq!(
            CAP_MATRIX.iter().map(|r| r.total_entities()).max(),
            Some(MAX_ENTITIES)
        );
    }

    #[test]
    fn team_balance() {
        // AC: 2/4/6 players. Assignment is round-robin, so the first two
        // players to join a 2v2 land on opposite teams rather than both on
        // team 0 — the alternative leaves a lobby looking unbalanced until the
        // last player joins.
        let cases = [
            (Mode::Teams, 2, Some(2), vec![TeamId(0), TeamId(1)]),
            (
                Mode::Teams,
                4,
                Some(2),
                vec![TeamId(0), TeamId(1), TeamId(0), TeamId(1)],
            ),
            (
                Mode::Teams,
                6,
                Some(2),
                vec![
                    TeamId(0),
                    TeamId(1),
                    TeamId(0),
                    TeamId(1),
                    TeamId(0),
                    TeamId(1),
                ],
            ),
            (
                Mode::Teams,
                6,
                Some(3),
                vec![
                    TeamId(0),
                    TeamId(1),
                    TeamId(2),
                    TeamId(0),
                    TeamId(1),
                    TeamId(2),
                ],
            ),
        ];
        for (mode, players, teams, expected) in cases {
            let layout = Layout::resolve(mode, players, teams).expect("legal layout");
            assert_eq!(layout.assign(), expected, "{players}p {teams:?} teams");
            assert_eq!(layout.players() as usize, expected.len());
            // Balanced: every team the same size, and none empty.
            let mut sizes = [0u8; 3];
            for team in layout.assign() {
                sizes[team.0 as usize] += 1;
            }
            let live = &sizes[..layout.teams as usize];
            assert!(live.iter().all(|s| *s == layout.per_team), "{sizes:?}");
        }

        // FFA: one player per team, and the teams argument is not allowed to
        // disagree with the player count.
        let ffa = Layout::resolve(Mode::Ffa, 4, None).expect("legal");
        assert_eq!(ffa.teams, 4);
        assert_eq!(ffa.per_team, 1);
        assert_eq!(
            ffa.assign(),
            vec![TeamId(0), TeamId(1), TeamId(2), TeamId(3)]
        );
        assert!(Layout::resolve(Mode::Ffa, 4, Some(2)).is_none());
    }

    #[test]
    fn a_running_match_stays_running() {
        let teams = [status(0, true, true), status(1, true, true)];
        assert_eq!(check_outcome(&teams), Outcome::Running);
    }

    #[test]
    fn last_team_standing_wins() {
        let teams = [status(0, true, true), status(1, false, false)];
        assert_eq!(
            check_outcome(&teams),
            Outcome::Ended {
                winner: Some(TeamId(0)),
                reason: EndReason::LastTeamStanding,
            }
        );
        // The winner is the team with the live core, not the first in the list.
        let teams = [status(0, false, false), status(1, true, true)];
        assert_eq!(check_outcome(&teams).winner(), Some(TeamId(1)));
    }

    #[test]
    fn simultaneous_destruction_is_a_draw() {
        // Reachable in real play: overtime decay hits every core at once, so
        // two turtle teams die on the same tick.
        let teams = [status(0, false, false), status(1, false, false)];
        assert_eq!(
            check_outcome(&teams),
            Outcome::Ended {
                winner: None,
                reason: EndReason::Draw,
            }
        );
        assert_eq!(check_outcome(&teams).winner(), None);
    }

    #[test]
    fn match_ends_when_last_human_dies() {
        // AC (D-05): 1 human + 5 bots in a 6-player FFA. The human dies, so
        // their core is gone — but five bot cores are still standing and the
        // match must still end, with no winner.
        let mut teams: Vec<TeamStatus> = (0..6).map(|i| status(i, true, i == 0)).collect();
        assert_eq!(check_outcome(&teams), Outcome::Running);

        teams[0] = status(0, false, false);
        let outcome = check_outcome(&teams);
        assert_eq!(
            outcome,
            Outcome::Ended {
                winner: None,
                reason: EndReason::NoHumans,
            },
            "five live bot cores must not extend the match"
        );
        assert!(teams.iter().filter(|t| t.core_alive).count() == 5);
    }

    #[test]
    fn bots_cannot_win_alone() {
        // A bot team reaching the last-core state still produces no winner.
        let teams = [
            status(0, false, false), // the human's team, eliminated
            status(1, true, false),  // bots, last core standing
        ];
        let outcome = check_outcome(&teams);
        assert_eq!(outcome.winner(), None, "a bot must never be the winner");
        assert_eq!(outcome.reason(), Some(EndReason::NoHumans));

        // Same shape with a human alive on the surviving team: a real win.
        let teams = [status(0, false, false), status(1, true, true)];
        assert_eq!(check_outcome(&teams).winner(), Some(TeamId(1)));
    }

    #[test]
    fn decay_is_zero_before_overtime() {
        let rules = MatchRules::from_tuning();
        assert_eq!(rules.overtime_start_s, 1320, "22:00");
        assert!(!rules.is_overtime(Tick(0)));
        assert_eq!(rules.decay_steps(Tick(1319 * TICK_HZ)), 0);
        assert_eq!(rules.cumulative_decay(Tick(1319 * TICK_HZ), 10_000), 0);
    }

    #[test]
    fn decay_is_cumulative_and_stepwise() {
        let rules = MatchRules::from_tuning();
        let start = rules.overtime_start_s * TICK_HZ;

        // 2% of 10,000 = 200 HP per 30 s step.
        assert_eq!(rules.cumulative_decay(Tick(start), 10_000), 0);
        assert_eq!(
            rules.cumulative_decay(Tick(start + 29 * TICK_HZ), 10_000),
            0
        );
        assert_eq!(
            rules.cumulative_decay(Tick(start + 30 * TICK_HZ), 10_000),
            200
        );
        assert_eq!(
            rules.cumulative_decay(Tick(start + 59 * TICK_HZ), 10_000),
            200
        );
        assert_eq!(
            rules.cumulative_decay(Tick(start + 60 * TICK_HZ), 10_000),
            400
        );
        // Cumulative, so applying it twice in one tick is a no-op for a caller
        // that tracks what it already applied.
        assert_eq!(
            rules.cumulative_decay(Tick(start + 60 * TICK_HZ), 10_000),
            400
        );
    }

    #[test]
    fn decay_eventually_destroys_a_full_core() {
        let rules = MatchRules::from_tuning();
        let bound = rules.decay_only_duration_s();
        // 50 steps of 30 s after 22:00 = 47:00. Far past the 25-minute target,
        // which is why the README records the discrepancy.
        assert_eq!(bound, 1320 + 50 * 30);
        let at_bound = Tick(bound * TICK_HZ);
        assert!(rules.cumulative_decay(at_bound, 10_000) >= 10_000);
        let just_before = Tick((bound - 1) * TICK_HZ);
        assert!(rules.cumulative_decay(just_before, 10_000) < 10_000);
    }
}
